//! Single sign-on: OpenID Connect (Microsoft Entra ID, Google, GitLab, Okta, Keycloak,
//! Authentik, …), GitHub (OAuth2), LDAP / Active Directory and SAML 2.0 (cargo feature `saml`).
//!
//! Providers are configured with `TASKCRAFT_AUTH_PROVIDERS` (JSON list) or
//! `TASKCRAFT_AUTH_PROVIDERS_FILE`, plus shortcuts (`TASKCRAFT_OIDC_*`, `TASKCRAFT_GOOGLE_*`,
//! `TASKCRAFT_MICROSOFT_*`). All of them end in [`sign_in`], which applies the same rules:
//! allowed e-mail domains, identity linking by (provider, subject), linking existing or invited
//! accounts by trusted e-mail, just-in-time accounts and optional group → team sync.

pub mod config;
pub mod github;
pub mod ldap;
pub mod oidc;
#[cfg(feature = "saml")]
pub mod saml;

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub use self::config::SsoConfig;
use crate::app::{AppState, audit, new_id};
use crate::auth::{random_token, token_hash};
use crate::error::ApiError;

/// Settings shared by all provider types.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Common {
    pub id: String,
    /// Button label on the login page.
    #[serde(default)]
    pub name: Option<String>,
    /// Only these e-mail domains may sign in (empty = any).
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Create accounts for unknown people (default: true when the provider is private or
    /// `allowedDomains` / `allowedOrgs` restrict it, false for public providers like Google or
    /// GitHub).
    #[serde(default)]
    pub auto_create: Option<bool>,
    /// Mirror the provider's groups into TaskCraft teams.
    #[serde(default)]
    pub sync_groups: bool,
    /// Role of new accounts: `member` (default) or `guest`.
    #[serde(default)]
    pub default_role: Option<String>,
    /// Trust e-mail addresses the provider doesn't explicitly mark as verified (default: yes for
    /// LDAP, SAML and single-tenant Microsoft, whose directories are administered; no for other
    /// OIDC providers and GitHub). `true` also allows linking to existing admin accounts.
    #[serde(default)]
    pub trust_email: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OidcConfig {
    #[serde(flatten)]
    pub common: Common,
    /// `google`, `microsoft`, `gitlab` or none (generic: give `issuer`)
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    /// Microsoft Entra tenant id (for preset `microsoft`)
    #[serde(default)]
    pub tenant: Option<String>,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Claim holding group names (e.g. `groups`; Entra `groups` / `roles`).
    #[serde(default)]
    pub groups_claim: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubConfig {
    #[serde(flatten)]
    pub common: Common,
    pub client_id: String,
    pub client_secret: String,
    /// GitHub Enterprise Server API base, default https://api.github.com
    #[serde(default)]
    pub api_url: Option<String>,
    /// GitHub Enterprise Server web base, default https://github.com
    #[serde(default)]
    pub web_url: Option<String>,
    /// Members of these organizations only (asks for the read:org scope).
    #[serde(default)]
    pub allowed_orgs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LdapConfig {
    #[serde(flatten)]
    pub common: Common,
    /// `ldap://host:389` or `ldaps://host:636`
    pub url: String,
    #[serde(default)]
    pub start_tls: bool,
    /// Accept self-signed certificates (testing only).
    #[serde(default)]
    pub insecure_skip_verify: bool,
    /// Service account used to search people (anonymous search when absent).
    #[serde(default)]
    pub bind_dn: Option<String>,
    #[serde(default)]
    pub bind_password: Option<String>,
    pub base_dn: String,
    /// `{username}` is replaced (escaped). Default matches uid, mail, sAMAccountName or
    /// userPrincipalName.
    #[serde(default)]
    pub user_filter: Option<String>,
    #[serde(default)]
    pub email_attribute: Option<String>,
    #[serde(default)]
    pub name_attribute: Option<String>,
    /// Stable id attribute (entryUUID / objectGUID); the DN is used when absent.
    #[serde(default)]
    pub id_attribute: Option<String>,
    #[serde(default)]
    pub group_attribute: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamlConfig {
    #[serde(flatten)]
    pub common: Common,
    /// IdP metadata XML (inline) or a URL to fetch it from.
    #[serde(default)]
    pub idp_metadata_url: Option<String>,
    #[serde(default)]
    pub idp_metadata_xml: Option<String>,
    /// SP entity id (default `{base_url}/api/auth/sso/{id}/metadata`).
    #[serde(default)]
    pub entity_id: Option<String>,
    #[serde(default)]
    pub email_attribute: Option<String>,
    #[serde(default)]
    pub name_attribute: Option<String>,
    #[serde(default)]
    pub groups_attribute: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Provider {
    Oidc(OidcConfig),
    Github(GithubConfig),
    Ldap(LdapConfig),
    Saml(SamlConfig),
}

impl Provider {
    pub fn common(&self) -> &Common {
        match self {
            Provider::Oidc(c) => &c.common,
            Provider::Github(c) => &c.common,
            Provider::Ldap(c) => &c.common,
            Provider::Saml(c) => &c.common,
        }
    }

    pub fn id(&self) -> &str {
        &self.common().id
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Provider::Oidc(_) => "oidc",
            Provider::Github(_) => "github",
            Provider::Ldap(_) => "ldap",
            Provider::Saml(_) => "saml",
        }
    }

    /// The configured name, else one by type (the web app translates the generic ones).
    pub fn display_name(&self) -> String {
        if let Some(n) = self.common().name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            return n.to_string();
        }
        match self {
            Provider::Oidc(c) => match c.preset.as_deref() {
                Some("google") => "Google".into(),
                Some("microsoft") => "Microsoft".into(),
                Some("gitlab") => "GitLab".into(),
                _ => "Single sign-on".into(),
            },
            Provider::Github(_) => "GitHub".into(),
            Provider::Ldap(_) => "Directory (LDAP)".into(),
            Provider::Saml(_) => "SAML single sign-on".into(),
        }
    }

    /// Public identity providers anyone can register with.
    fn is_public(&self) -> bool {
        match self {
            Provider::Github(g) => g.web_url.as_deref().is_none_or(|u| u.trim_end_matches('/') == "https://github.com"),
            Provider::Oidc(c) => match c.preset.as_deref() {
                Some("google") => true,
                // gitlab.com is public, a self-hosted GitLab (own issuer) isn't
                Some("gitlab") => c.issuer.as_deref().is_none_or(|i| i.trim_end_matches('/') == "https://gitlab.com"),
                Some("microsoft") => matches!(c.tenant.as_deref(), None | Some("common" | "consumers")),
                _ => false,
            },
            _ => false,
        }
    }

    /// Whether this sign-in's e-mail may be used to link, claim or restrict by domain.
    pub fn email_trusted(&self, verified: Option<bool>) -> bool {
        match verified {
            Some(v) => v,
            None => self.common().trust_email.unwrap_or(match self {
                Provider::Ldap(_) | Provider::Saml(_) => true,
                Provider::Oidc(c) => c.preset.as_deref() == Some("microsoft"),
                Provider::Github(_) => false,
            }),
        }
    }

    pub fn auto_create(&self) -> bool {
        let c = self.common();
        let restricted = !c.allowed_domains.is_empty() || matches!(self, Provider::Github(g) if !g.allowed_orgs.is_empty());
        c.auto_create.unwrap_or(!self.is_public() || restricted)
    }
}

fn split_list(s: Option<&String>) -> Vec<String> {
    s.map(|v| v.split(',').map(|d| d.trim().to_lowercase()).filter(|d| !d.is_empty()).collect()).unwrap_or_default()
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Builds the provider list from the configuration; fails on invalid configuration (at startup).
pub fn load(config: &SsoConfig) -> anyhow::Result<Vec<Provider>> {
    let mut out: Vec<Provider> = Vec::new();
    let mut json = config.auth_providers.clone();
    if let Some(path) = &config.auth_providers_file {
        json = Some(std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?);
    }
    if let Some(j) = json.filter(|j| !j.trim().is_empty()) {
        out.extend(serde_json::from_str::<Vec<Provider>>(&j).map_err(|e| anyhow::anyhow!("invalid TASKCRAFT_AUTH_PROVIDERS: {e}"))?);
    }
    // shortcuts
    if let (Some(issuer), Some(client_id)) = (nonempty(&config.oidc_issuer), nonempty(&config.oidc_client_id)) {
        out.push(Provider::Oidc(OidcConfig {
            common: Common {
                id: "oidc".into(),
                name: nonempty(&config.oidc_name),
                allowed_domains: split_list(config.oidc_allowed_domains.as_ref()),
                auto_create: config.oidc_auto_create,
                ..Default::default()
            },
            preset: None,
            issuer: Some(issuer),
            tenant: None,
            client_id,
            client_secret: nonempty(&config.oidc_client_secret),
            scopes: vec![],
            groups_claim: None,
        }));
    }
    if let (Some(client_id), Some(secret)) = (nonempty(&config.google_client_id), nonempty(&config.google_client_secret)) {
        out.push(Provider::Oidc(OidcConfig {
            common: Common { id: "google".into(), allowed_domains: split_list(config.google_allowed_domains.as_ref()), ..Default::default() },
            preset: Some("google".into()),
            issuer: None,
            tenant: None,
            client_id,
            client_secret: Some(secret),
            scopes: vec![],
            groups_claim: None,
        }));
    }
    if let (Some(client_id), Some(secret)) = (nonempty(&config.microsoft_client_id), nonempty(&config.microsoft_client_secret)) {
        out.push(Provider::Oidc(OidcConfig {
            common: Common { id: "microsoft".into(), allowed_domains: split_list(config.microsoft_allowed_domains.as_ref()), ..Default::default() },
            preset: Some("microsoft".into()),
            issuer: None,
            tenant: nonempty(&config.microsoft_tenant),
            client_id,
            client_secret: Some(secret),
            scopes: vec![],
            groups_claim: Some("groups".into()),
        }));
    }
    let mut seen = HashSet::new();
    for p in &out {
        let id = p.id();
        if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            anyhow::bail!("sign-in provider id {id:?} must be non-empty and use letters, digits, '-' or '_'");
        }
        if !seen.insert(id.to_string()) {
            anyhow::bail!("duplicate sign-in provider id {id:?}");
        }
        if let Some(r) = p.common().default_role.as_deref()
            && !matches!(r, "member" | "guest")
        {
            anyhow::bail!("sign-in provider {id:?}: defaultRole must be member or guest");
        }
        if let Provider::Oidc(c) = p {
            if c.issuer.is_none() && c.preset.is_none() {
                anyhow::bail!("OIDC provider {id:?} needs an issuer or a preset");
            }
            if c.preset.as_deref() == Some("microsoft") && matches!(c.tenant.as_deref(), None | Some("common" | "organizations" | "consumers")) {
                anyhow::bail!("Microsoft provider {id:?} needs a tenant id (multi-tenant 'common' isn't supported)");
            }
        }
        #[cfg(not(feature = "saml"))]
        if matches!(p, Provider::Saml(_)) {
            anyhow::bail!("SAML provider {id:?} configured, but this build has no SAML support (cargo feature `saml`)");
        }
    }
    for p in &mut out {
        for d in match p {
            Provider::Oidc(c) => &mut c.common.allowed_domains,
            Provider::Github(c) => &mut c.common.allowed_domains,
            Provider::Ldap(c) => &mut c.common.allowed_domains,
            Provider::Saml(c) => &mut c.common.allowed_domains,
        } {
            *d = d.trim().trim_start_matches('@').to_lowercase();
        }
    }
    Ok(out)
}

pub fn find<'a>(state: &'a AppState, id: &str) -> Option<&'a Provider> {
    state.providers.iter().find(|p| p.id() == id)
}

/// Whether local passwords sign in (`TASKCRAFT_DISABLE_PASSWORD_LOGIN` turns them off).
pub fn passwords_enabled(state: &AppState) -> bool {
    !state.config.sso.disable_password_login
}

/// Whether the password form signs anyone in: local passwords or a directory (LDAP).
pub fn password_form_enabled(state: &AppState) -> bool {
    passwords_enabled(state) || state.providers.iter().any(|p| matches!(p, Provider::Ldap(_)))
}

/// The sign-in routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    let r = Router::new()
        .route("/auth/providers", get(list))
        .route("/auth/sso/{id}/login", get(oidc::sso_login))
        .route("/auth/sso/{id}/callback", get(oidc::sso_callback))
        .route("/me/sign-in", get(my_sign_in));
    #[cfg(feature = "saml")]
    let r = r.route("/auth/sso/{id}/acs", axum::routing::post(saml::acs)).route("/auth/sso/{id}/metadata", get(saml::metadata));
    r
}

/// `GET /api/auth/providers` — what the login page shows.
pub async fn list(State(state): State<AppState>) -> Json<Value> {
    let providers: Vec<Value> = state
        .providers
        .iter()
        .map(|p| {
            let redirect = matches!(p, Provider::Oidc(_) | Provider::Github(_) | Provider::Saml(_));
            let preset = match p {
                Provider::Oidc(c) => c.preset.as_deref(),
                _ => None,
            };
            json!({ "id": p.id(), "name": p.display_name(), "type": p.kind(), "preset": preset, "loginUrl": redirect.then(|| format!("/api/auth/sso/{}/login", p.id())) })
        })
        .collect();
    Json(json!({ "passwordLogin": passwords_enabled(&state), "providers": providers }))
}

/// How people sign in, by user: `{password, providers: [{id, name, type}]}` (Admin → Users).
pub async fn sign_in_methods(state: &AppState, workspace: Uuid) -> sqlx::Result<HashMap<Uuid, Value>> {
    let rows: Vec<(Uuid, bool, Vec<String>)> = sqlx::query_as(
        "SELECT u.id, u.password_hash IS NOT NULL, COALESCE(array_agg(DISTINCT i.provider_id) FILTER (WHERE i.provider_id IS NOT NULL), '{}')
           FROM users u LEFT JOIN identities i ON i.user_id = u.id WHERE u.workspace_id = $1 GROUP BY u.id",
    )
    .bind(workspace)
    .fetch_all(&state.db)
    .await?;
    let passwords = passwords_enabled(state);
    Ok(rows
        .into_iter()
        .map(|(id, password, providers)| {
            let providers: Vec<Value> = providers
                .iter()
                .map(|pid| match find(state, pid) {
                    Some(p) => json!({"id": pid, "name": p.display_name(), "type": p.kind()}),
                    // linked through a provider that is no longer configured
                    None => json!({"id": pid, "name": pid, "type": null}),
                })
                .collect();
            (id, json!({"password": password && passwords, "providers": providers}))
        })
        .collect())
}

/// `GET /api/me/sign-in` — how the signed-in person signs in (the password settings use it).
pub async fn my_sign_in(State(state): State<AppState>, crate::auth::CurrentUser(user): crate::auth::CurrentUser) -> Result<Json<Value>, ApiError> {
    let mut all = sign_in_methods(&state, user.workspace_id).await?;
    let mine = all.remove(&user.id).unwrap_or_else(|| json!({"password": false, "providers": []}));
    Ok(Json(json!({ "passwordLogin": passwords_enabled(&state), "password": mine["password"], "providers": mine["providers"] })))
}

/// An identity asserted by an external provider.
#[derive(Debug, Clone)]
pub struct External {
    /// Stable key of the identity provider (issuer, `ldap:<id>`, `github:<url>`, SAML entity id).
    pub provider_key: String,
    pub subject: String,
    pub email: Option<String>,
    /// `Some(false)` when the provider says the e-mail is unverified.
    pub email_verified: Option<bool>,
    pub name: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    /// refused, with a reason for the person
    #[error("{0}")]
    Denied(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for SignInError {
    fn from(e: sqlx::Error) -> Self {
        SignInError::Other(e.into())
    }
}

impl From<SignInError> for ApiError {
    fn from(e: SignInError) -> Self {
        match e {
            SignInError::Denied(m) => ApiError::Denied(m),
            SignInError::Other(e) => ApiError::Internal(e),
        }
    }
}

/// Maps an external identity to a TaskCraft account (link, claim or create) and syncs teams.
pub async fn sign_in(state: &AppState, provider: &Provider, ext: External) -> Result<Uuid, SignInError> {
    let common = provider.common();
    let email = ext.email.as_deref().map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@') && e.len() <= 254);
    // unverified addresses are only used when the provider is trusted for them (fail closed)
    let trusted = provider.email_trusted(ext.email_verified);
    if !common.allowed_domains.is_empty() {
        let domain = email.as_deref().and_then(|e| e.rsplit('@').next()).unwrap_or("");
        let ok = trusted && common.allowed_domains.iter().any(|d| d.eq_ignore_ascii_case(domain));
        if !ok {
            return Err(SignInError::Denied(format!("{} accounts from this e-mail domain are not allowed", provider.display_name())));
        }
    }
    let ws: Uuid = sqlx::query_scalar("SELECT id FROM workspaces ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| SignInError::Denied("the workspace isn't set up yet".into()))?;
    let provided_name = ext.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(100).collect::<String>());
    let linked: Option<(Uuid, String)> =
        sqlx::query_as("SELECT u.id, u.status FROM identities i JOIN users u ON u.id = i.user_id WHERE i.provider = $1 AND i.subject = $2")
            .bind(&ext.provider_key)
            .bind(&ext.subject)
            .fetch_optional(&state.db)
            .await?;
    let user_id = match linked {
        Some((_, status)) if status == "deactivated" => return Err(SignInError::Denied("your account is deactivated".into())),
        Some((id, status)) => {
            if status == "invited" {
                activate(state, id, provided_name.as_deref()).await?;
            }
            id
        }
        None => {
            let by_email: Option<(Uuid, String, String)> = match &email {
                Some(e) if trusted => {
                    sqlx::query_as("SELECT id, status, role FROM users WHERE workspace_id = $1 AND lower(email) = $2")
                        .bind(ws)
                        .bind(e)
                        .fetch_optional(&state.db)
                        .await?
                }
                _ => None,
            };
            let id = match by_email {
                Some((_, status, _)) if status == "deactivated" => return Err(SignInError::Denied("your account is deactivated".into())),
                // admin accounts are only linked on an explicit trust signal
                Some((_, _, role)) if (role == "owner" || role == "admin") && ext.email_verified != Some(true) && common.trust_email != Some(true) => {
                    return Err(SignInError::Denied(format!(
                        "{} belongs to an administrator account; sign in with your password, or ask for trustEmail to be set for this provider",
                        email.as_deref().unwrap_or("this e-mail address")
                    )));
                }
                Some((id, status, _)) => {
                    // an invitation accepted through single sign-on
                    if status == "invited" {
                        activate(state, id, provided_name.as_deref()).await?;
                    }
                    id
                }
                None => {
                    if !provider.auto_create() {
                        return Err(SignInError::Denied(format!(
                            "there is no TaskCraft account for {}; ask an administrator to invite you",
                            email.as_deref().unwrap_or("this identity")
                        )));
                    }
                    let id = new_id();
                    let role = match common.default_role.as_deref() {
                        Some("guest") => "guest",
                        _ => "member",
                    };
                    let name = provided_name.clone().or_else(|| email.clone()).unwrap_or_else(|| format!("{} user", provider.display_name()));
                    sqlx::query("INSERT INTO users (id, workspace_id, email, name, role, status) VALUES ($1, $2, $3, $4, $5, 'active')")
                        .bind(id)
                        .bind(ws)
                        .bind(email.as_deref().filter(|_| trusted))
                        .bind(&name)
                        .bind(role)
                        .execute(&state.db)
                        .await
                        .map_err(|e| {
                            if e.as_database_error().is_some_and(|d| d.is_unique_violation()) {
                                SignInError::Denied("someone with this e-mail address already has an account".into())
                            } else {
                                e.into()
                            }
                        })?;
                    audit(state, ws, Some(id), "user.sso_created", email.clone(), json!({"provider": provider.id(), "role": role})).await;
                    id
                }
            };
            sqlx::query("INSERT INTO identities (provider, subject, user_id, provider_id, email) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING")
                .bind(&ext.provider_key)
                .bind(&ext.subject)
                .bind(id)
                .bind(provider.id())
                .bind(&email)
                .execute(&state.db)
                .await?;
            id
        }
    };
    sqlx::query("UPDATE identities SET last_login_at = now(), provider_id = $3 WHERE provider = $1 AND subject = $2")
        .bind(&ext.provider_key)
        .bind(&ext.subject)
        .bind(provider.id())
        .execute(&state.db)
        .await?;
    if common.sync_groups {
        sync_teams(state, ws, provider.id(), user_id, &ext.groups).await?;
    }
    audit(state, ws, Some(user_id), "auth.sso_login", None, json!({"provider": provider.id()})).await;
    Ok(user_id)
}

/// An invited person's first sign-in through a provider: the account becomes active and the
/// invitation link is used up.
async fn activate(state: &AppState, user: Uuid, name: Option<&str>) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET status = 'active', name = COALESCE($2, name) WHERE id = $1 AND status = 'invited'")
        .bind(user)
        .bind(name)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM user_links WHERE user_id = $1 AND kind = 'invite'").bind(user).execute(&state.db).await?;
    Ok(())
}

/// Sets the person's membership in the teams this provider manages to exactly `groups`.
async fn sync_teams(state: &AppState, ws: Uuid, provider_id: &str, user: Uuid, groups: &[String]) -> sqlx::Result<()> {
    let prefix = format!("sso:{provider_id}:");
    let mut wanted: Vec<Uuid> = Vec::new();
    for g in groups.iter().map(|g| g.trim()).filter(|g| !g.is_empty() && g.chars().count() <= 200).take(500) {
        let ext_ref = format!("{prefix}{g}");
        let by_ref = || {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM teams WHERE workspace_id = $1 AND external_ref = $2")
                .bind(ws)
                .bind(ext_ref.clone())
                .fetch_optional(&state.db)
        };
        // only teams this provider created are reused; a local team (or another provider's) with
        // the same name is never taken over — ours gets a suffix instead
        let mut id: Option<Uuid> = by_ref().await?;
        for name in [g.to_string(), format!("{g} ({provider_id})")] {
            if id.is_some() {
                break;
            }
            id = sqlx::query_scalar("INSERT INTO teams (id, workspace_id, name, external_ref) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING id")
                .bind(new_id())
                .bind(ws)
                .bind(&name)
                .bind(&ext_ref)
                .fetch_optional(&state.db)
                .await?;
            if id.is_none() {
                // another sign-in may have just created it
                id = by_ref().await?;
            }
        }
        if let Some(id) = id {
            wanted.push(id);
        }
    }
    sqlx::query(
        "DELETE FROM team_members m USING teams t WHERE m.team_id = t.id AND m.user_id = $1 AND t.workspace_id = $2 AND starts_with(t.external_ref, $3) AND NOT (t.id = ANY($4))",
    )
    .bind(user)
    .bind(ws)
    .bind(&prefix)
    .bind(&wanted)
    .execute(&state.db)
    .await?;
    for t in wanted {
        sqlx::query("INSERT INTO team_members (team_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(t).bind(user).execute(&state.db).await?;
    }
    Ok(())
}

/// Cookie holding a random value that ties a pending sign-in to the browser that started it
/// (login-CSRF protection: an attacker can't complete their own sign-in in a victim's browser).
pub const BROWSER_COOKIE: &str = "tc_sso";

/// `SameSite=None` is needed when the IdP returns with a cross-site POST (SAML); browsers only
/// accept that together with `Secure`.
pub fn browser_cookie(value: &str, max_age: i64, cross_site_post: bool, secure: bool) -> String {
    let same_site = if cross_site_post { "None" } else { "Lax" };
    format!("{BROWSER_COOKIE}={value}; Path=/api/auth; HttpOnly; SameSite={same_site}; Max-Age={max_age}{}", if secure { "; Secure" } else { "" })
}

/// After a successful sign-in: a session and the redirect to `next`.
pub async fn finish(state: &AppState, user: Uuid, headers: &HeaderMap, next: &str) -> Result<Response, SignInError> {
    // two-factor authentication: people with a second factor continue on `/login?mfa=1`
    let (cookie, to) = crate::mfa::session_or_second_step(state, user, headers, next).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut res = Redirect::to(&to).into_response();
    let h = res.headers_mut();
    h.append(header::SET_COOKIE, HeaderValue::from_str(&cookie).map_err(anyhow::Error::from)?);
    // the pending sign-in is done
    h.append(header::SET_COOKIE, HeaderValue::from_str(&browser_cookie("", 0, false, state.config.secure_cookies)).map_err(anyhow::Error::from)?);
    Ok(res)
}

/// The end of a redirect flow: the redirect, or back to the login page with the reason.
pub fn respond(provider: &str, res: Result<Response, SignInError>) -> Response {
    match res {
        Ok(r) => r,
        Err(SignInError::Denied(msg)) => {
            tracing::info!(provider, "sign-in refused: {msg}");
            login_error(&msg)
        }
        Err(SignInError::Other(e)) => {
            tracing::warn!(provider, "sign-in failed: {e:#}");
            login_error(&format!("{e}"))
        }
    }
}

/// Stores short-lived sign-in state (shared by all replicas); returns the opaque state key.
/// With `browser`, completing the sign-in requires the matching [`BROWSER_COOKIE`].
pub async fn put_state(state: &AppState, data: &Value, browser: Option<&str>) -> anyhow::Result<String> {
    let key = random_token();
    let mut data = data.clone();
    if let (Some(b), Some(obj)) = (browser, data.as_object_mut()) {
        obj.insert("_browser".into(), Value::String(hex::encode(token_hash(b))));
    }
    sqlx::query("DELETE FROM sso_states WHERE expires_at < now()").execute(&state.db).await?;
    sqlx::query("INSERT INTO sso_states (state, data, expires_at) VALUES ($1, $2, now() + interval '10 minutes')")
        .bind(hex::encode(token_hash(&key)))
        .bind(&data)
        .execute(&state.db)
        .await?;
    Ok(key)
}

/// Adds fields to pending sign-in state.
pub async fn update_state(state: &AppState, key: &str, data: &Value) -> anyhow::Result<()> {
    sqlx::query("UPDATE sso_states SET data = data || $2 WHERE state = $1").bind(hex::encode(token_hash(key))).bind(data).execute(&state.db).await?;
    Ok(())
}

/// Consumes sign-in state (one-time use), checking the browser binding.
pub async fn take_state(state: &AppState, key: &str, headers: &HeaderMap) -> Result<Value, SignInError> {
    let data: Option<Value> = sqlx::query_scalar("DELETE FROM sso_states WHERE state = $1 AND expires_at > now() RETURNING data")
        .bind(hex::encode(token_hash(key)))
        .fetch_optional(&state.db)
        .await?;
    let data = data.ok_or_else(|| SignInError::Denied("the sign-in expired, please try again".into()))?;
    if let Some(expected) = data.get("_browser").and_then(Value::as_str) {
        let actual = crate::auth::cookie_value(headers, BROWSER_COOKIE).map(|v| hex::encode(token_hash(v)));
        if actual.as_deref() != Some(expected) {
            return Err(SignInError::Denied("this sign-in was started in a different browser, please try again".into()));
        }
    }
    Ok(data)
}

/// Same-site paths only: no `//` or `/\` prefix, and no whitespace, control characters or
/// backslashes anywhere (browsers strip tabs, turning `/\t/evil.com` into `//evil.com`).
pub fn safe_next(next: Option<String>) -> String {
    next.filter(|n| n.starts_with('/') && !n.starts_with("//") && n.len() <= 2000 && !n.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\'))
        .unwrap_or_else(|| "/".into())
}

pub fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Redirect to the login page with an error message.
pub fn login_error(msg: &str) -> Response {
    Redirect::to(&format!("/login?error={}", urlencode(msg))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_stay_on_site() {
        for bad in ["//evil.com", "/\\evil.com", "/\t/evil.com", "/\n/evil.com", "https://evil.com", "/ok\\..\\x", ""] {
            assert_eq!(safe_next(Some(bad.into())), "/", "{bad:?}");
        }
        assert_eq!(safe_next(Some("/issues/WEB-1?x=1#y".into())), "/issues/WEB-1?x=1#y");
        assert_eq!(safe_next(None), "/");
    }

    fn cfg(json: &str) -> SsoConfig {
        SsoConfig { auth_providers: Some(json.into()), ..Default::default() }
    }

    #[test]
    fn provider_list_and_shortcuts() {
        let c = SsoConfig {
            auth_providers: Some(
                r#"[{"type":"github","id":"gh","clientId":"a","clientSecret":"b","allowedOrgs":["acme"]},
                {"type":"ldap","id":"ad","url":"ldap://dc","baseDn":"dc=acme","allowedDomains":["@ACME.com"]}]"#
                    .into(),
            ),
            google_client_id: Some("g".into()),
            google_client_secret: Some("s".into()),
            oidc_issuer: Some("https://sso.acme.com".into()),
            oidc_client_id: Some("tc".into()),
            ..Default::default()
        };
        let ps = load(&c).unwrap();
        let ids: Vec<&str> = ps.iter().map(|p| p.id()).collect();
        assert_eq!(ids, ["gh", "ad", "oidc", "google"]);
        assert_eq!(ps[1].common().allowed_domains, ["acme.com"]);
        assert_eq!(ps[2].display_name(), "Single sign-on");
        assert_eq!(ps[3].display_name(), "Google");
        // GitHub restricted to an organization creates accounts; plain Google doesn't
        assert!(ps[0].auto_create());
        assert!(!ps[3].auto_create());
        assert!(ps[2].auto_create());
        // e-mail trust defaults
        assert!(ps[1].email_trusted(None));
        assert!(!ps[2].email_trusted(None));
        assert!(!ps[1].email_trusted(Some(false)));
    }

    #[test]
    fn invalid_configuration_fails_fast() {
        for bad in [
            r#"[{"type":"oidc","id":"x","clientId":"a"}]"#,
            r#"[{"type":"oidc","id":"ms","preset":"microsoft","clientId":"a"}]"#,
            r#"[{"type":"oidc","id":"bad id","issuer":"https://x","clientId":"a"}]"#,
            r#"[{"type":"oidc","id":"a","issuer":"https://x","clientId":"a"},{"type":"github","id":"a","clientId":"a","clientSecret":"b"}]"#,
            r#"[{"type":"oidc","id":"a","issuer":"https://x","clientId":"a","defaultRole":"admin"}]"#,
            r#"[{"type":"kerberos","id":"a"}]"#,
        ] {
            assert!(load(&cfg(bad)).is_err(), "{bad}");
        }
        #[cfg(not(feature = "saml"))]
        assert!(load(&cfg(r#"[{"type":"saml","id":"okta","idpMetadataUrl":"https://x"}]"#)).is_err());
    }
}
