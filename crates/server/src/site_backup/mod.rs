//! Migration from another tracker's XML site backup (a `.zip` with `entities.xml`,
//! `activeobjects.xml` and the attachments): people, projects, issues with their numbers,
//! comments, time logs, attachments, links, watchers, labels, components, versions, sprints and
//! the history the reports replay.
//!
//! The backup is read ([`read`]) and turned into the source-neutral model ([`to_model`]) that
//! [`crate::importer`] writes, in one transaction, telling nobody.

mod convert;
pub mod read;
pub mod source;

pub use crate::importer::{
    Choice, Count, ExistingProject, Options, People, PeopleSummary, Progress, ProgressHandle, ProjectReport, ProjectSummary, Report, Summary, Warning,
};
pub use convert::{origin, to_model};
pub use read::Backup;
pub use source::Source;

use crate::app::AppState;
use crate::auth::User;

/// Reads a backup's rows (and sprints when it has `activeobjects.xml`).
pub fn load(source: &mut Source, tz: chrono_tz::Tz) -> anyhow::Result<Backup> {
    let mut b = Backup::read(source.entities()?, tz)?;
    if let Some(ao) = source.active_objects()? {
        b.read_active_objects(ao, tz)?;
    }
    anyhow::ensure!(!b.projects.is_empty() || !b.issues.is_empty(), "the backup holds no projects");
    Ok(b)
}

/// Imports a backup (or with `dry_run` checks it), as `user`.
pub async fn run(state: &AppState, user: &User, source: &mut Source, b: &Backup, opts: &Options, progress: ProgressHandle) -> anyhow::Result<Report> {
    let m = to_model(b);
    crate::importer::run(state, user, source, &m, opts, progress).await
}

/// What a backup holds and how it meets this workspace (the check before an import).
pub async fn summarize(state: &AppState, workspace: uuid::Uuid, source: &Source, b: &Backup) -> anyhow::Result<Summary> {
    crate::importer::summarize(state, workspace, &to_model(b), source).await
}
