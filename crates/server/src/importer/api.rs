//! Imports from other trackers' REST APIs: a connection (the source, its address, a token, the
//! projects asked for), read into the model by the source's producer, and the files of
//! attachments downloaded while importing. The token is used for the request it came with and
//! never stored.

use serde::{Deserialize, Serialize};

use super::http::Http;
use super::model::{Attachment, BoxFuture, File, Files, Format, Model};
use super::{Available, ProgressHandle, github, gitlab, redmine};

/// The trackers whose APIs are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Github,
    Gitlab,
    Redmine,
}

impl Kind {
    /// As `backup_imports.source` and the model's source name it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Github => "github",
            Kind::Gitlab => "gitlab",
            Kind::Redmine => "redmine",
        }
    }

    /// For people: in messages and job titles.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Github => "GitHub",
            Kind::Gitlab => "GitLab",
            Kind::Redmine => "Redmine",
        }
    }
}

impl std::str::FromStr for Kind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "github" => Ok(Kind::Github),
            "gitlab" => Ok(Kind::Gitlab),
            "redmine" => Ok(Kind::Redmine),
            _ => Err("source must be github, gitlab or redmine".into()),
        }
    }
}

/// What to read, from where, with which credentials.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub source: Kind,
    /// the server (empty: the public service)
    #[serde(default)]
    pub base_url: String,
    /// a personal access token or API key (read access is enough)
    #[serde(default)]
    pub token: Option<String>,
    /// repositories or projects (`owner/repo`, `group/project`, identifiers)
    #[serde(default)]
    pub projects: Vec<String>,
    /// how texts are written, for sources where that's a server setting
    #[serde(default)]
    pub markup: Option<Format>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").field("source", &self.source).field("base_url", &self.base_url).field("projects", &self.projects).finish_non_exhaustive()
    }
}

impl Connection {
    fn token(&self) -> Option<&str> {
        self.token.as_deref().map(str::trim).filter(|t| !t.is_empty())
    }

    /// A client for the source.
    pub fn client(&self, allow_private: bool, progress: ProgressHandle) -> anyhow::Result<Http> {
        match self.source {
            Kind::Github => github::client(&github::site(&self.base_url)?, self.token(), allow_private, progress),
            Kind::Gitlab => gitlab::client(&gitlab::site(&self.base_url)?, self.token(), allow_private, progress),
            Kind::Redmine => redmine::client(&self.base_url, self.token(), allow_private, progress),
        }
    }

    /// The address shown for the source (and kept with the job).
    pub fn shown_url(&self) -> String {
        match self.source {
            Kind::Github => github::site(&self.base_url).map(|s| s.web.to_string()).unwrap_or_else(|_| self.base_url.clone()),
            Kind::Gitlab => gitlab::site(&self.base_url).map(|s| s.web.to_string()).unwrap_or_else(|_| self.base_url.clone()),
            Kind::Redmine => super::http::base_url(&self.base_url).map(|u| u.to_string()).unwrap_or_else(|_| self.base_url.clone()),
        }
    }

    /// Reads the projects asked for into the model.
    pub async fn fetch(&self, http: &Http, progress: &ProgressHandle) -> anyhow::Result<Model> {
        http.check_host().await?;
        match self.source {
            Kind::Github => github::fetch(http, &github::site(&self.base_url)?, &self.projects, progress).await,
            Kind::Gitlab => gitlab::fetch(http, &gitlab::site(&self.base_url)?, &self.projects, progress).await,
            Kind::Redmine => redmine::fetch(http, &self.projects, self.markup, progress).await,
        }
    }

    /// Projects to choose from (`owner`: a user, organization or group).
    pub async fn browse(&self, http: &Http, owner: Option<&str>) -> anyhow::Result<Vec<Available>> {
        http.check_host().await?;
        match self.source {
            Kind::Github => github::browse(http, owner).await,
            Kind::Gitlab => gitlab::browse(http, owner).await,
            Kind::Redmine => redmine::browse(http, owner).await,
        }
    }
}

/// The files of a source's attachments, downloaded while importing.
pub struct ApiFiles {
    pub http: Option<Http>,
    pub count: u64,
}

impl ApiFiles {
    pub fn new(m: &Model, http: Option<Http>) -> ApiFiles {
        ApiFiles { http, count: m.issues.iter().map(|i| i.attachments.iter().filter(|a| a.url.is_some()).count() as u64).sum() }
    }
}

impl Files for ApiFiles {
    fn has(&self, a: &Attachment) -> bool {
        a.url.is_some()
    }

    fn count(&self) -> u64 {
        self.count
    }

    fn size(&mut self, a: &Attachment) -> Option<u64> {
        a.size
    }

    fn read<'a>(&'a mut self, a: &'a Attachment, max: u64) -> BoxFuture<'a, anyhow::Result<Option<File>>> {
        Box::pin(async move {
            let Some(http) = &self.http else { return Ok(None) };
            let first = match &a.url {
                Some(url) => http.download(url, max).await,
                None => Ok(None),
            };
            // not there (or not allowed): where else it may be
            match (first, &a.fallback_url) {
                (Ok(None), Some(other)) => http.download(other, max).await,
                (Err(e), Some(other)) if matches!(super::http::status_of(&e), Some(401 | 403 | 404)) => http.download(other, max).await,
                (r, _) => r,
            }
        })
    }
}
