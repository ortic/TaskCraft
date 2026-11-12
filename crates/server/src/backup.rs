//! `taskcraft backup` and `taskcraft restore`: the whole workspace (database and attachments) in
//! one `.tar.gz` file.
//!
//! The archive holds `manifest.json` first, then one `db/<table>.copy` per table (PostgreSQL's
//! text `COPY` format, taken in a single repeatable-read snapshot, tables ordered so that
//! referenced rows come first), then `files/<path>` per attachment. A restore needs an empty
//! database (or `--force`): it builds the schema up to the backup's migration, loads the tables,
//! runs this version's newer migrations over the restored data and uploads the attachments to
//! whatever storage is configured (so it also moves attachments between disk and S3).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail, ensure};
use futures::TryStreamExt;
use object_store::{ObjectStore, ObjectStoreExt};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::config::Config;

/// Archive layout version.
const FORMAT: u32 = 1;
const MIGRATIONS_TABLE: &str = "_sqlx_migrations";

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub app_version: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// the schema migrations the database had (restores build the schema up to the last one)
    pub migrations: Vec<MigrationInfo>,
    /// in load order
    pub tables: Vec<TableInfo>,
    pub files: u64,
    pub file_bytes: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MigrationInfo {
    pub version: i64,
    pub description: String,
    pub checksum: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: u64,
}

/// What a backup or restore did.
#[derive(Debug, Default)]
pub struct Summary {
    pub tables: usize,
    pub rows: u64,
    pub files: u64,
    pub file_bytes: u64,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} tables, {} rows, {} attachments ({})", self.tables, self.rows, self.files, human_bytes(self.file_bytes))
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} kB", n as f64 / (1u64 << 10) as f64),
        n => format!("{n} bytes"),
    }
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `COPY` statement for a table and its columns.
fn copy_sql(table: &TableInfo, direction: &str) -> String {
    let cols = table.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ");
    format!("COPY public.{} ({cols}) {direction}", quote(&table.name))
}

/// The tables of the `public` schema (without the migration bookkeeping), referenced tables
/// before the tables referencing them.
async fn tables(conn: &mut sqlx::PgConnection) -> anyhow::Result<Vec<TableInfo>> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE' AND table_name <> $1",
    )
    .bind(MIGRATIONS_TABLE)
    .fetch_all(&mut *conn)
    .await?;
    let edges: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, c.confrelid::regclass::text FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace
          WHERE c.contype = 'f' AND n.nspname = 'public' AND c.conrelid <> c.confrelid",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut needs: BTreeMap<String, BTreeSet<String>> = names.iter().map(|n| (n.clone(), BTreeSet::new())).collect();
    for (from, to) in edges {
        let (from, to) = (from.trim_matches('"').to_string(), to.trim_matches('"').to_string());
        if needs.contains_key(&to)
            && let Some(set) = needs.get_mut(&from)
        {
            set.insert(to);
        }
    }
    // Kahn's algorithm, alphabetical among equals so archives are stable
    let mut order = Vec::new();
    while !needs.is_empty() {
        let ready: Vec<String> = needs.iter().filter(|(_, deps)| deps.is_empty()).map(|(n, _)| n.clone()).collect();
        let batch = if ready.is_empty() {
            // a reference cycle: load the rest in name order (constraints are checked per statement)
            tracing::warn!("tables reference each other in a cycle: {:?}", needs.keys().collect::<Vec<_>>());
            needs.keys().cloned().collect()
        } else {
            ready
        };
        for n in &batch {
            needs.remove(n);
        }
        for deps in needs.values_mut() {
            for n in &batch {
                deps.remove(n);
            }
        }
        order.extend(batch);
    }
    let mut out = Vec::new();
    for name in order {
        // generated columns are computed again on load
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns
              WHERE table_schema = 'public' AND table_name = $1 AND is_generated = 'NEVER' ORDER BY ordinal_position",
        )
        .bind(&name)
        .fetch_all(&mut *conn)
        .await?;
        out.push(TableInfo { name, columns, rows: 0 });
    }
    Ok(out)
}

/// A scratch directory next to the output, removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn append(tar: &mut tar::Builder<impl Write>, path: &str, size: u64, data: impl Read) -> anyhow::Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(size);
    h.set_mode(0o600);
    h.set_mtime(chrono::Utc::now().timestamp().max(0) as u64);
    h.set_entry_type(tar::EntryType::Regular);
    tar.append_data(&mut h, path, data).with_context(|| format!("writing {path}"))?;
    Ok(())
}

/// Writes a backup to `out` (a file path, or `-` for standard output).
pub async fn backup(db: &PgPool, blobs: &dyn ObjectStore, out: &Path, with_files: bool) -> anyhow::Result<Summary> {
    let to_stdout = out.as_os_str() == "-";
    let dir = if to_stdout { std::env::temp_dir() } else { out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf() };
    let scratch = Scratch(dir.join(format!(".taskcraft-backup-{}", uuid::Uuid::new_v4().simple())));
    std::fs::create_dir_all(&scratch.0).with_context(|| format!("creating {}", scratch.0.display()))?;

    // the database, in one consistent snapshot
    let mut tx = db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
    let migrations: Vec<(i64, String, Vec<u8>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT version, description, checksum FROM public.{MIGRATIONS_TABLE} WHERE success ORDER BY version")))
            .fetch_all(&mut *tx)
            .await
            .context("reading the schema version (is this a TaskCraft database?)")?;
    ensure!(!migrations.is_empty(), "the database has no TaskCraft schema yet");
    let mut tables = tables(&mut tx).await?;
    let mut summary = Summary { tables: tables.len(), ..Default::default() };
    for (i, t) in tables.iter_mut().enumerate() {
        let path = scratch.0.join(format!("{i:03}.copy"));
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        let mut stream = tx.copy_out_raw(&copy_sql(t, "TO STDOUT")).await?;
        let mut rows = 0u64;
        while let Some(chunk) = stream.try_next().await? {
            // text format: one line per row (newlines inside values are escaped)
            rows += chunk.iter().filter(|b| **b == b'\n').count() as u64;
            file.write_all(&chunk)?;
        }
        drop(stream);
        file.flush()?;
        t.rows = rows;
        summary.rows += rows;
    }
    tx.rollback().await?;

    // attachments: listed after the snapshot, so every file a row points to is there
    let mut files = Vec::new();
    if with_files {
        let mut list = blobs.list(None);
        while let Some(meta) = list.try_next().await? {
            files.push(meta);
        }
        files.sort_by(|a, b| a.location.as_ref().cmp(b.location.as_ref()));
    }
    summary.files = files.len() as u64;
    summary.file_bytes = files.iter().map(|f| f.size).sum();

    let manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created_at: chrono::Utc::now(),
        migrations: migrations
            .into_iter()
            .map(|(version, description, checksum)| MigrationInfo { version, description, checksum: hex::encode(checksum) })
            .collect(),
        tables,
        files: summary.files,
        file_bytes: summary.file_bytes,
    };

    // the archive: written beside the target and renamed at the end, so a failed backup never
    // leaves a half-written file under the real name
    let partial = scratch.0.join("archive.tar.gz");
    let sink: Box<dyn Write> = if to_stdout { Box::new(std::io::stdout().lock()) } else { Box::new(std::fs::File::create(&partial)?) };
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(std::io::BufWriter::new(sink), flate2::Compression::default()));
    let json = serde_json::to_vec_pretty(&manifest)?;
    append(&mut tar, "manifest.json", json.len() as u64, json.as_slice())?;
    for (i, t) in manifest.tables.iter().enumerate() {
        let path = scratch.0.join(format!("{i:03}.copy"));
        let size = std::fs::metadata(&path)?.len();
        append(&mut tar, &format!("db/{}.copy", t.name), size, std::fs::File::open(&path)?)?;
        std::fs::remove_file(&path)?;
    }
    for meta in &files {
        let bytes = blobs.get(&meta.location).await.with_context(|| format!("reading attachment {}", meta.location))?.bytes().await?;
        append(&mut tar, &format!("files/{}", meta.location), bytes.len() as u64, bytes.as_ref())?;
    }
    let mut gz = tar.into_inner()?;
    gz.flush()?;
    gz.finish()?.flush()?;
    if !to_stdout {
        std::fs::rename(&partial, out).with_context(|| format!("writing {}", out.display()))?;
    }
    Ok(summary)
}

/// Reads the manifest of a backup without restoring it.
pub fn inspect(file: &Path) -> anyhow::Result<Manifest> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(file)?));
    let mut entries = archive.entries()?;
    read_manifest(&mut entries)
}

fn read_manifest<R: Read>(entries: &mut tar::Entries<'_, R>) -> anyhow::Result<Manifest> {
    let mut first = entries.next().context("the backup is empty")??;
    ensure!(first.path()?.as_os_str() == "manifest.json", "not a TaskCraft backup (no manifest.json at the start)");
    let mut json = Vec::new();
    first.read_to_end(&mut json)?;
    let manifest: Manifest = serde_json::from_slice(&json).context("reading manifest.json")?;
    ensure!(manifest.format == FORMAT, "unknown backup format {} (this TaskCraft reads format {FORMAT})", manifest.format);
    Ok(manifest)
}

/// Removes everything TaskCraft keeps in the `public` schema: tables, functions, types.
async fn wipe(db: &PgPool) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    let tables: Vec<String> = sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname = 'public'").fetch_all(&mut *tx).await?;
    for t in tables {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS public.{} CASCADE", quote(&t)))).execute(&mut *tx).await?;
    }
    let functions: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'public' AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e')",
    )
    .fetch_all(&mut *tx)
    .await?;
    for f in functions {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP FUNCTION IF EXISTS {f} CASCADE"))).execute(&mut *tx).await?;
    }
    let types: Vec<String> = sqlx::query_scalar(
        "SELECT t.typname::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
          WHERE n.nspname = 'public' AND t.typtype IN ('e', 'd') AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = t.oid AND d.deptype = 'e')",
    )
    .fetch_all(&mut *tx)
    .await?;
    for t in types {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TYPE IF EXISTS public.{} CASCADE", quote(&t)))).execute(&mut *tx).await?;
    }
    let sequences: Vec<String> = sqlx::query_scalar("SELECT sequencename::text FROM pg_sequences WHERE schemaname = 'public'").fetch_all(&mut *tx).await?;
    for s in sequences {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP SEQUENCE IF EXISTS public.{} CASCADE", quote(&s)))).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Restores a backup from `file` (a path, or `-` for standard input) into the configured database
/// and attachment storage. Without `force` the database must not hold a workspace yet.
pub async fn restore(config: &Config, file: &Path, force: bool) -> anyhow::Result<Summary> {
    let source: Box<dyn Read> = if file.as_os_str() == "-" {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?)
    };
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(source)));
    let mut entries = archive.entries()?;
    let manifest = read_manifest(&mut entries)?;

    // the backup's schema must be one this version knows (same migrations, same contents)
    let migrator = crate::app::migrator();
    for m in &manifest.migrations {
        let known = migrator.iter().find(|k| k.version == m.version && !k.migration_type.is_down_migration());
        match known {
            Some(k) if hex::encode(&*k.checksum) == m.checksum => {}
            Some(_) => bail!("migration {} ({}) differs from this version's; restore with TaskCraft {}", m.version, m.description, manifest.app_version),
            None => bail!("the backup comes from a newer TaskCraft ({}, migration {}); upgrade first", manifest.app_version, m.version),
        }
    }
    let target = manifest.migrations.iter().map(|m| m.version).max().context("the backup has no schema version")?;

    let db = crate::app::connect(config).await?;
    let has_workspace: bool = sqlx::query_scalar("SELECT to_regclass('public.workspaces') IS NOT NULL").fetch_one(&db).await?;
    if has_workspace {
        let workspaces: i64 = sqlx::query_scalar("SELECT count(*) FROM public.workspaces").fetch_one(&db).await?;
        ensure!(
            workspaces == 0 || force,
            "the database already holds a workspace; restore into an empty database, or pass --force to replace everything in it"
        );
    }
    wipe(&db).await?;
    migrator.run_to(target, &db).await.context("building the backup's schema")?;

    // the tables, in one transaction
    let mut summary = Summary { tables: manifest.tables.len(), ..Default::default() };
    let mut tx = db.begin().await?;
    for t in &manifest.tables {
        let mut entry = entries.next().with_context(|| format!("the backup ends before table {}", t.name))??;
        let path = entry.path()?.to_string_lossy().into_owned();
        ensure!(path == format!("db/{}.copy", t.name), "unexpected {path} in the backup (expected table {})", t.name);
        let mut copy = tx.copy_in_raw(&copy_sql(t, "FROM STDIN")).await.with_context(|| format!("loading {}", t.name))?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            copy.send(&buf[..n]).await?;
        }
        let rows = copy.finish().await.with_context(|| format!("loading {}", t.name))?;
        ensure!(rows == t.rows, "table {}: {rows} rows loaded, the backup lists {}", t.name, t.rows);
        summary.rows += rows;
    }
    tx.commit().await?;
    // this version's newer migrations run over the restored data
    migrator.run(&db).await?;

    // attachments, into whatever storage is configured
    let blobs: Arc<dyn ObjectStore> = crate::app::blob_store(config)?;
    for entry in entries {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(location) = path.strip_prefix("files/") else { bail!("unexpected {path} in the backup") };
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        summary.file_bytes += bytes.len() as u64;
        summary.files += 1;
        let location = object_store::path::Path::parse(location).with_context(|| format!("attachment path {location}"))?;
        blobs.put(&location, bytes.into()).await.with_context(|| format!("storing attachment {location}"))?;
    }
    ensure!(summary.files == manifest.files, "{} of {} attachments restored", summary.files, manifest.files);
    db.close().await;
    Ok(summary)
}
