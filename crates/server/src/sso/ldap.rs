//! LDAP / Active Directory: username + password are verified by binding as the person (after a
//! search with a service account), then mapped to a TaskCraft account; `memberOf` groups can be
//! mirrored into teams. Directory accounts sign in through the normal password form.

use std::time::Duration;

use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry, ldap_escape};
use uuid::Uuid;

use super::{External, LdapConfig, Provider};
use crate::app::AppState;
use crate::error::ApiError;

const DEFAULT_FILTER: &str = "(&(|(objectClass=person)(objectClass=inetOrgPerson)(objectClass=user))(|(uid={username})(mail={username})(sAMAccountName={username})(userPrincipalName={username})))";

/// Verifies credentials; `Ok(None)` = unknown person or wrong password.
pub async fn authenticate(c: &LdapConfig, username: &str, password: &str) -> anyhow::Result<Option<External>> {
    let username = username.trim();
    // an empty password would be an "unauthenticated bind" that many servers accept
    if username.is_empty() || password.is_empty() || username.len() > 256 {
        return Ok(None);
    }
    let settings = LdapConnSettings::new().set_conn_timeout(Duration::from_secs(10)).set_starttls(c.start_tls).set_no_tls_verify(c.insecure_skip_verify);
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &c.url).await?;
    ldap3::drive!(conn);
    if let Some(dn) = &c.bind_dn {
        ldap.simple_bind(dn, c.bind_password.as_deref().unwrap_or("")).await?.success().map_err(|e| anyhow::anyhow!("LDAP service bind failed: {e}"))?;
    }
    let filter = c.user_filter.as_deref().unwrap_or(DEFAULT_FILTER).replace("{username}", &ldap_escape(username));
    let email_attr = c.email_attribute.as_deref().unwrap_or("mail");
    let name_attr = c.name_attribute.as_deref().unwrap_or("displayName");
    let id_attr = c.id_attribute.as_deref().unwrap_or("entryUUID");
    let group_attr = c.group_attribute.as_deref().unwrap_or("memberOf");
    let attrs = vec![email_attr, name_attr, "cn", id_attr, "objectGUID", group_attr];
    let (entries, _) = ldap.search(&c.base_dn, Scope::Subtree, &filter, attrs).await?.success()?;
    if entries.len() != 1 {
        // none or ambiguous: never guess
        let _ = ldap.unbind().await;
        return Ok(None);
    }
    let entry = SearchEntry::construct(entries.into_iter().next().expect("one entry"));
    // verify the password by binding as the person
    let ok = ldap.simple_bind(&entry.dn, password).await.map(|r| r.rc == 0).unwrap_or(false);
    let _ = ldap.unbind().await;
    if !ok {
        return Ok(None);
    }
    let first = |a: &str| entry.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(a)).and_then(|(_, v)| v.first().cloned());
    let subject = first(id_attr)
        .or_else(|| entry.bin_attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case("objectGUID")).and_then(|(_, v)| v.first()).map(hex::encode))
        .unwrap_or_else(|| entry.dn.to_lowercase());
    let groups: Vec<String> =
        entry.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(group_attr)).map(|(_, v)| v.iter().map(|dn| cn_of(dn)).collect()).unwrap_or_default();
    Ok(Some(External {
        provider_key: format!("ldap:{}", c.common.id),
        subject,
        email: first(email_attr),
        // directory e-mails are administered, not self-asserted
        email_verified: None,
        name: first(name_attr).or_else(|| first("cn")),
        groups,
    }))
}

/// The password form's directory step: tries the configured LDAP providers in order and returns
/// `(user id, workspace id)` of the account signed in, `None` when no directory knows these
/// credentials.
pub async fn login(state: &AppState, username: &str, password: &str) -> Result<Option<(Uuid, Uuid)>, ApiError> {
    for p in state.providers.iter() {
        let Provider::Ldap(c) = p else { continue };
        // a directory that stops answering mustn't hold the sign-in forever
        let res =
            tokio::time::timeout(Duration::from_secs(20), authenticate(c, username, password)).await.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out")));
        match res {
            Ok(Some(ext)) => {
                let id = super::sign_in(state, p, ext).await?;
                let ws: Uuid = sqlx::query_scalar("SELECT workspace_id FROM users WHERE id = $1").bind(id).fetch_one(&state.db).await?;
                return Ok(Some((id, ws)));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(provider = %c.common.id, "LDAP sign-in failed: {e:#}"),
        }
    }
    Ok(None)
}

/// `cn=Engineering,ou=groups,dc=acme,dc=com` → `Engineering`
fn cn_of(dn: &str) -> String {
    dn.split(',').next().and_then(|rdn| rdn.split_once('=')).map(|(_, v)| v.trim().to_string()).unwrap_or_else(|| dn.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn group_names_from_dns() {
        assert_eq!(super::cn_of("cn=Engineering,ou=groups,dc=acme,dc=com"), "Engineering");
        assert_eq!(super::cn_of("plain"), "plain");
    }
}
