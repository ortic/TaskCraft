//! Where a backup's files are: a zip archive (on disk or uploaded) or an unpacked folder, with
//! `entities.xml`, optionally `activeobjects.xml`, and the attachments under
//! `data/attachments/<PROJECT>/<bucket>/<ISSUE-KEY>/<attachment id>`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

trait ReadSeek: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> ReadSeek for T {}

enum Kind {
    Dir { entities: PathBuf, active_objects: Option<PathBuf> },
    Zip { archive: Box<zip::ZipArchive<Box<dyn ReadSeek>>>, entities: usize, active_objects: Option<usize> },
}

/// A file of the backup: a path on disk or an entry of the archive.
#[derive(Debug, Clone)]
enum Loc {
    Path(PathBuf),
    Entry(usize),
}

pub struct Source {
    kind: Kind,
    /// attachment id → its file
    files: HashMap<String, Loc>,
}

/// Whether a path names an attachment's file: `…/attachments/…/<KEY-12>/<digits>` (thumbnails
/// live in `thumbs/` folders and are left out).
fn attachment_id(path: &str) -> Option<&str> {
    let path = path.trim_end_matches('/');
    let mut parts = path.rsplit(['/', '\\']);
    let name = parts.next()?;
    let parent = parts.next()?;
    let ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) && parent != "thumbs" && path.contains("attachments");
    ok.then_some(name)
}

impl Source {
    /// A zip file, a folder holding `entities.xml` (directly or one level down) or the
    /// `entities.xml` file itself. `attachments` names a separate attachments folder.
    pub fn open(path: &Path, attachments: Option<&Path>) -> anyhow::Result<Source> {
        let mut s = if path.is_dir() {
            Self::dir(path)?
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")) {
            Self::dir(path.parent().unwrap_or(Path::new(".")))?
        } else {
            let f = File::open(path).with_context(|| format!("can't open {}", path.display()))?;
            Self::zip(Box::new(BufReader::new(f)))?
        };
        if let Some(dir) = attachments {
            anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
            s.scan_dir(dir)?;
        }
        Ok(s)
    }

    /// A zip archive held in memory (an upload).
    pub fn from_zip_bytes(bytes: Vec<u8>) -> anyhow::Result<Source> {
        Self::zip(Box::new(Cursor::new(bytes)))
    }

    fn dir(root: &Path) -> anyhow::Result<Source> {
        let find = |name: &str| -> Option<PathBuf> {
            let direct = root.join(name);
            if direct.is_file() {
                return Some(direct);
            }
            // an archive unpacked into a folder of its own
            std::fs::read_dir(root).ok()?.flatten().map(|e| e.path().join(name)).find(|p| p.is_file())
        };
        let Some(entities) = find("entities.xml") else { bail!("no entities.xml in {}: is this an unpacked XML site backup?", root.display()) };
        let base = entities.parent().unwrap_or(root).to_path_buf();
        let active_objects = Some(base.join("activeobjects.xml")).filter(|p| p.is_file());
        let mut s = Source { kind: Kind::Dir { entities, active_objects }, files: HashMap::new() };
        let data = base.join("data").join("attachments");
        if data.is_dir() {
            s.scan_dir(&data)?;
        }
        Ok(s)
    }

    fn zip(reader: Box<dyn ReadSeek>) -> anyhow::Result<Source> {
        let archive = zip::ZipArchive::new(reader).context("not a zip archive (an XML site backup is a .zip file or an unpacked folder)")?;
        let mut entities: Option<(usize, usize)> = None;
        let mut active_objects: Option<(usize, usize)> = None;
        let mut files = HashMap::new();
        for i in 0..archive.len() {
            let Some(Ok(name)) = archive.name_for_index(i) else { continue };
            let depth = name.matches('/').count();
            let file = name.rsplit('/').next().unwrap_or("");
            if file == "entities.xml" && entities.is_none_or(|(_, d)| depth < d) {
                entities = Some((i, depth));
            } else if file == "activeobjects.xml" && active_objects.is_none_or(|(_, d)| depth < d) {
                active_objects = Some((i, depth));
            } else if let Some(id) = attachment_id(&name) {
                files.insert(id.to_string(), Loc::Entry(i));
            }
        }
        let Some((entities, _)) = entities else { bail!("the archive holds no entities.xml: is it an XML site backup?") };
        Ok(Source { kind: Kind::Zip { archive: Box::new(archive), entities, active_objects: active_objects.map(|a| a.0) }, files })
    }

    fn scan_dir(&mut self, dir: &Path) -> anyhow::Result<()> {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).with_context(|| format!("can't read {}", d.display()))? {
                let e = e?;
                let p = e.path();
                if e.file_type()?.is_dir() {
                    stack.push(p);
                } else if let Some(id) = p.to_str().and_then(attachment_id) {
                    self.files.insert(id.to_string(), Loc::Path(p.clone()));
                }
            }
        }
        Ok(())
    }

    /// `entities.xml`.
    pub fn entities(&mut self) -> anyhow::Result<Box<dyn BufRead + '_>> {
        match &mut self.kind {
            Kind::Dir { entities, .. } => Ok(Box::new(BufReader::with_capacity(1 << 16, File::open(entities)?))),
            Kind::Zip { archive, entities, .. } => Ok(Box::new(BufReader::with_capacity(1 << 16, archive.by_index(*entities)?))),
        }
    }

    /// `activeobjects.xml` (boards and sprints), when the backup has it.
    pub fn active_objects(&mut self) -> anyhow::Result<Option<Box<dyn BufRead + '_>>> {
        match &mut self.kind {
            Kind::Dir { active_objects: Some(p), .. } => Ok(Some(Box::new(BufReader::with_capacity(1 << 16, File::open(p)?)))),
            Kind::Zip { archive, active_objects: Some(i), .. } => Ok(Some(Box::new(BufReader::with_capacity(1 << 16, archive.by_index(*i)?)))),
            _ => Ok(None),
        }
    }

    /// Whether the backup holds the file of an attachment.
    pub fn has_file(&self, attachment: &str) -> bool {
        self.files.contains_key(attachment)
    }

    /// How many attachment files the backup holds.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The file of an attachment: `Ok(None)` when it's missing, an error when it's larger than
    /// `max` bytes.
    pub fn read_file(&mut self, attachment: &str, max: u64) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(loc) = self.files.get(attachment).cloned() else { return Ok(None) };
        let mut data = Vec::new();
        match (loc, &mut self.kind) {
            (Loc::Path(p), _) => {
                File::open(&p)?.take(max + 1).read_to_end(&mut data)?;
            }
            (Loc::Entry(i), Kind::Zip { archive, .. }) => {
                archive.by_index(i)?.take(max + 1).read_to_end(&mut data)?;
            }
            (Loc::Entry(_), Kind::Dir { .. }) => return Ok(None),
        }
        if data.len() as u64 > max {
            bail!("larger than the upload limit");
        }
        Ok(Some(data))
    }

    /// The size of an attachment's file without reading it (dry runs).
    pub fn file_size(&mut self, attachment: &str) -> Option<u64> {
        match (self.files.get(attachment)?, &mut self.kind) {
            (Loc::Path(p), _) => std::fs::metadata(p).ok().map(|m| m.len()),
            (Loc::Entry(i), Kind::Zip { archive, .. }) => archive.by_index_raw(*i).ok().map(|f| f.size()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::attachment_id;

    #[test]
    fn attachment_paths() {
        assert_eq!(attachment_id("data/attachments/ABC/10000/ABC-1/10042"), Some("10042"));
        assert_eq!(attachment_id("backup/data/attachments/ABC/10000/ABC-1/thumbs/10042"), None);
        assert_eq!(attachment_id("data/attachments/ABC/10000/ABC-1/_thumb_10042.png"), None);
        assert_eq!(attachment_id("data/avatars/10042"), None);
    }
}
