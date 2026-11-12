//! Reading a mailbox over IMAP (`TASKCRAFT_IMAP_URL`): unread messages go through
//! [`super::receive`] and are then marked read (or moved to `?move=Folder`). Messages whose
//! handling failed for a passing reason stay unread and are tried again next time. Use a
//! mailbox only TaskCraft reads: messages someone else marks read are not picked up.

use std::sync::Arc;
use std::time::Duration;

use async_imap::Client;
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use crate::app::AppState;

/// Messages handled per round (the rest wait for the next minute).
const BATCH: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// imaps:// (port 993)
    Tls,
    /// imap://…?tls=starttls (port 143)
    StartTls,
    /// imap:// without encryption: local servers only
    None,
}

/// A mailbox to read, from `TASKCRAFT_IMAP_URL`.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    pub security: Security,
    pub host: String,
    pub port: u16,
    pub user: String,
    password: String,
    pub mailbox: String,
    /// move handled messages here instead of marking them read
    pub move_to: Option<String>,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

/// `user@host:993/INBOX (TLS)` — shown to admins, never the password.
impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let security = match self.security {
            Security::Tls => "TLS",
            Security::StartTls => "STARTTLS",
            Security::None => "unencrypted",
        };
        write!(f, "{}@{}:{}/{} ({security})", self.user, self.host, self.port, self.mailbox)
    }
}

impl Target {
    /// `local+KEY@domain` addresses reach this mailbox when its user name is an e-mail address.
    pub fn plus_address(&self, key: &str) -> Option<String> {
        let (local, domain) = self.user.split_once('@')?;
        let local = local.split('+').next().unwrap_or(local);
        (!local.is_empty() && domain.contains('.')).then(|| format!("{local}+{key}@{domain}").to_lowercase())
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit())).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Checks an IMAP URL: `imaps://user:pass@host[:993]/Mailbox`, `imap://user:pass@host[:143]/Mailbox?tls=starttls`
/// or `imap://…` (unencrypted); `?move=Folder` optional.
pub fn parse_url(url: &str) -> Result<Target, String> {
    let u = reqwest::Url::parse(url.trim()).map_err(|e| format!("not a URL ({e})"))?;
    let q = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned()).filter(|v| !v.is_empty());
    let (security, port) = match (u.scheme(), q("tls").as_deref()) {
        ("imaps", _) => (Security::Tls, 993),
        ("imap", Some("starttls" | "required")) => (Security::StartTls, 143),
        ("imap", None) => (Security::None, 143),
        ("imap", Some(other)) => return Err(format!("tls={other} isn't supported; use tls=starttls or imaps://")),
        (other, _) => return Err(format!("use imaps:// or imap://, not {other}://")),
    };
    let host = u.host_str().filter(|h| !h.is_empty()).ok_or("the URL needs a host, e.g. imaps://user:password@imap.example.com/INBOX")?;
    let user = percent_decode(u.username());
    let password = percent_decode(u.password().unwrap_or(""));
    if user.is_empty() || password.is_empty() {
        return Err("the URL needs a user name and password, e.g. imaps://user:password@imap.example.com/INBOX".into());
    }
    let mailbox = percent_decode(u.path().trim_matches('/'));
    Ok(Target {
        security,
        host: host.trim_matches(|c| c == '[' || c == ']').to_string(),
        port: u.port().unwrap_or(port),
        user,
        password,
        mailbox: if mailbox.is_empty() { "INBOX".into() } else { mailbox },
        move_to: q("move"),
    })
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> Io for T {}
type Stream = Box<dyn Io>;

fn tls() -> anyhow::Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder_with_provider(Arc::new(tokio_rustls::rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn login(t: &Target) -> anyhow::Result<async_imap::Session<Stream>> {
    let tcp = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect((t.host.as_str(), t.port)))
        .await
        .map_err(|_| anyhow::anyhow!("connecting to {}:{} timed out", t.host, t.port))??;
    let name = || ServerName::try_from(t.host.clone()).map_err(|e| anyhow::anyhow!("{}: {e}", t.host));
    let mut client: Client<Stream> = match t.security {
        Security::Tls => Client::new(Box::new(tls()?.connect(name()?, tcp).await?)),
        Security::None => Client::new(Box::new(tcp)),
        Security::StartTls => {
            let mut plain = Client::new(tcp);
            plain.read_response().await?.ok_or_else(|| anyhow::anyhow!("the server closed the connection"))?;
            plain.run_command_and_check_ok("STARTTLS", None).await?;
            let tcp = plain.into_inner();
            Client::new(Box::new(tls()?.connect(name()?, tcp).await?))
        }
    };
    if t.security != Security::StartTls {
        client.read_response().await?.ok_or_else(|| anyhow::anyhow!("the server closed the connection"))?;
    }
    client.login(&t.user, &t.password).await.map_err(|(e, _)| anyhow::anyhow!("signing in as {} failed: {e}", t.user))
}

/// Handles the mailbox's unread messages; returns how many were handled.
pub async fn poll(state: &AppState, t: &Target) -> anyhow::Result<usize> {
    let mut s = login(t).await?;
    s.select(&t.mailbox).await.map_err(|e| anyhow::anyhow!("opening {}: {e}", t.mailbox))?;
    let mut uids: Vec<u32> = s.uid_search("UNSEEN").await?.into_iter().collect();
    uids.sort_unstable();
    uids.truncate(BATCH);
    let limit = super::MAX_MESSAGE_MB * 1024 * 1024;
    let mut handled = 0;
    for uid in uids {
        let set = uid.to_string();
        let sizes: Vec<_> = s.uid_fetch(&set, "RFC822.SIZE").await?.try_collect().await?;
        let size = sizes.first().and_then(|f| f.size).unwrap_or(0) as usize;
        // too large: only the header is downloaded (to log who sent it)
        let query = if size > limit { "BODY.PEEK[HEADER]" } else { "BODY.PEEK[]" };
        let fetched: Vec<_> = s.uid_fetch(&set, query).await?.try_collect().await?;
        let Some(f) = fetched.first() else { continue };
        let raw = if size > limit { f.header() } else { f.body() }.unwrap_or_default().to_vec();
        let size = size.max(raw.len());
        while s.unsolicited_responses.try_recv().is_ok() {}
        match super::receive(state, &raw, size, "imap").await {
            Ok(_) => {
                handled += 1;
                let moved = match &t.move_to {
                    Some(folder) => s.uid_mv(&set, folder).await.map_err(|e| tracing::warn!(folder, "moving a handled message failed: {e}")).is_ok(),
                    None => false,
                };
                if !moved {
                    let _: Vec<_> = s.uid_store(&set, "+FLAGS.SILENT (\\Seen)").await?.try_collect().await?;
                }
            }
            Err(e) => tracing::warn!(uid, "e-mail in: handling a message failed (tried again later): {e:#}"),
        }
    }
    let _ = s.logout().await;
    Ok(handled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imap_urls() {
        let t = parse_url("imaps://tasks%40acme.com:s3cr%40t@imap.acme.com/INBOX").unwrap();
        assert_eq!(
            (t.security, t.host.as_str(), t.port, t.user.as_str(), t.password.as_str()),
            (Security::Tls, "imap.acme.com", 993, "tasks@acme.com", "s3cr@t")
        );
        assert_eq!(t.mailbox, "INBOX");
        assert_eq!(t.to_string(), "tasks@acme.com@imap.acme.com:993/INBOX (TLS)");
        assert!(!format!("{t:?}").contains("s3cr"), "the password stays out of logs");
        assert_eq!(t.plus_address("SUP").as_deref(), Some("tasks+sup@acme.com"));
        let t = parse_url("imap://u:p@mail.local:1143/Support%20Inbox?tls=starttls&move=Done").unwrap();
        assert_eq!((t.security, t.port, t.mailbox.as_str(), t.move_to.as_deref()), (Security::StartTls, 1143, "Support Inbox", Some("Done")));
        assert_eq!(t.plus_address("SUP"), None, "a user name without domain");
        let t = parse_url("imap://u:p@127.0.0.1").unwrap();
        assert_eq!((t.security, t.port, t.mailbox.as_str()), (Security::None, 143, "INBOX"));
        assert!(parse_url("imaps://imap.acme.com").unwrap_err().contains("user name and password"));
        assert!(parse_url("pop3://u:p@x").unwrap_err().contains("imaps://"));
        assert!(parse_url("imap://u:p@x?tls=always").unwrap_err().contains("tls=always"));
        assert!(parse_url("not a url").is_err());
        assert_eq!(percent_decode("a%2Fb%zz%"), "a/b%zz%");
    }
}
