//! OpenID Connect providers (authorization code flow + PKCE + nonce). Presets for Google,
//! Microsoft Entra ID and GitLab; any other OIDC issuer (Okta, Keycloak, Authentik, Auth0,
//! Zitadel, …) via `issuer`. Also the generic `/api/auth/sso/{id}/login` and `…/callback` routes
//! for every redirect-based provider.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{External, OidcConfig, Provider, SignInError};
use crate::app::AppState;

/// Discovery documents (with the signing keys), refreshed hourly: providers rotate their keys.
static METADATA: LazyLock<RwLock<HashMap<String, (Instant, CoreProviderMetadata)>>> = LazyLock::new(Default::default);
const METADATA_TTL: Duration = Duration::from_secs(3600);

#[derive(Serialize, Deserialize)]
struct Pending {
    provider: String,
    verifier: String,
    nonce: String,
    next: String,
}

fn http() -> openidconnect::reqwest::Client {
    // following redirects would open the client up to SSRF
    openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("TaskCraft/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

fn issuer(c: &OidcConfig) -> String {
    if let Some(i) = &c.issuer {
        return i.clone();
    }
    match c.preset.as_deref() {
        Some("google") => "https://accounts.google.com".into(),
        Some("microsoft") => format!("https://login.microsoftonline.com/{}/v2.0", c.tenant.as_deref().unwrap_or("organizations")),
        Some("gitlab") => "https://gitlab.com".into(),
        _ => String::new(),
    }
}

async fn metadata(c: &OidcConfig, refresh: bool) -> anyhow::Result<CoreProviderMetadata> {
    let iss = issuer(c);
    if !refresh
        && let Some((at, m)) = METADATA.read().expect("metadata cache").get(&iss)
        && at.elapsed() < METADATA_TTL
    {
        return Ok(m.clone());
    }
    let m = CoreProviderMetadata::discover_async(IssuerUrl::new(iss.clone())?, &http())
        .await
        .map_err(|e| anyhow::anyhow!("OpenID Connect discovery for {iss} failed: {e}"))?;
    METADATA.write().expect("metadata cache").insert(iss, (Instant::now(), m.clone()));
    Ok(m)
}

/// `{BASE_URL}/api/auth/sso/{id}/callback` — register this at the provider.
pub fn redirect_uri(state: &AppState, provider_id: &str) -> String {
    format!("{}/api/auth/sso/{provider_id}/callback", state.config.base())
}

macro_rules! oidc_client {
    ($state:expr, $c:expr, $md:expr) => {
        CoreClient::from_provider_metadata($md, ClientId::new($c.client_id.clone()), $c.client_secret.clone().map(ClientSecret::new))
            .set_redirect_uri(RedirectUrl::new(redirect_uri($state, &$c.common.id)).map_err(anyhow::Error::from)?)
    };
}

/// Starts the sign-in: redirect to the identity provider.
pub async fn login(state: &AppState, c: &OidcConfig, next: String, browser: Option<&str>) -> anyhow::Result<Response> {
    let md = metadata(c, false).await?;
    let client = oidc_client!(state, c, md);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let nonce = Nonce::new_random();
    let pending = Pending { provider: c.common.id.clone(), verifier: verifier.secret().clone(), nonce: nonce.secret().clone(), next };
    // the OAuth `state` is our state key (stored in the database, so the callback may reach any replica)
    let key = super::put_state(state, &serde_json::to_value(&pending)?, browser).await?;
    let mut req = client.authorize_url(CoreAuthenticationFlow::AuthorizationCode, move || CsrfToken::new(key), move || nonce).set_pkce_challenge(challenge);
    let mut scopes: Vec<String> = vec!["email".into(), "profile".into()];
    for s in &c.scopes {
        // `openid` is always asked for
        if s != "openid" && !scopes.contains(s) {
            scopes.push(s.clone());
        }
    }
    for s in scopes {
        req = req.add_scope(Scope::new(s));
    }
    let (url, _, _) = req.url();
    Ok(Redirect::to(url.as_str()).into_response())
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Reads extra claims (e.g. `groups`) from an ID token whose signature was already verified.
fn raw_claims(id_token: &str) -> Value {
    id_token
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

async fn complete(state: &AppState, provider: &Provider, c: &OidcConfig, headers: &HeaderMap, cb: Callback) -> Result<Response, SignInError> {
    if let Some(e) = cb.error {
        return Err(SignInError::Denied(format!("the identity provider says: {e} {}", cb.error_description.unwrap_or_default()).trim().to_string()));
    }
    let (Some(code), Some(key)) = (cb.code, cb.state) else { return Err(SignInError::Denied("the identity provider sent no code".into())) };
    let pending: Pending = serde_json::from_value(super::take_state(state, &key, headers).await?)
        .map_err(|_| SignInError::Denied("the sign-in expired, please try again".into()))?;
    if pending.provider != c.common.id {
        return Err(SignInError::Denied("this sign-in was started at another provider".into()));
    }
    let md = metadata(c, false).await?;
    let client = oidc_client!(state, c, md);
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
        .request_async(&http())
        .await
        .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;
    let id_token = token.id_token().ok_or_else(|| anyhow::anyhow!("the identity provider sent no ID token"))?;
    let nonce = Nonce::new(pending.nonce);
    let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
        Ok(c) => c.clone(),
        // signed with a key we haven't seen yet (rotation): fetch the keys again, once
        Err(_) => {
            let fresh = oidc_client!(state, c, metadata(c, true).await?);
            id_token.claims(&fresh.id_token_verifier(), &nonce).map_err(|e| anyhow::anyhow!("invalid ID token: {e}"))?.clone()
        }
    };
    let raw = raw_claims(&id_token.to_string());
    let groups: Vec<String> = c
        .groups_claim
        .as_deref()
        .and_then(|g| raw.get(g))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    // the username fallback is user-chosen at some providers: never treat it as verified
    let claimed_email = claims.email().map(|e| e.to_string());
    let from_username = claimed_email.is_none();
    let email = claimed_email.or_else(|| raw.get("preferred_username").and_then(Value::as_str).filter(|u| u.contains('@')).map(String::from));
    let name = claims.name().and_then(|n| n.get(None)).map(|n| n.to_string()).or_else(|| claims.preferred_username().map(|u| u.to_string()));
    let ext = External {
        provider_key: claims.issuer().to_string(),
        subject: claims.subject().to_string(),
        email,
        // Entra ID doesn't send email_verified; its addresses are trusted per provider type
        email_verified: if from_username { None } else { claims.email_verified() },
        name,
        groups,
    };
    let user = super::sign_in(state, provider, ext).await?;
    super::finish(state, user, headers, &pending.next).await
}

// ---- routes -------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

/// `GET /api/auth/sso/{id}/login` — starts a sign-in at a redirect-based provider.
pub async fn sso_login(State(state): State<AppState>, Path(id): Path<String>, Query(q): Query<LoginQuery>) -> Response {
    let next = super::safe_next(q.next);
    let Some(p) = super::find(&state, &id).cloned() else { return super::login_error("unknown sign-in provider") };
    // bind the sign-in to this browser; SAML returns with a cross-site POST, which only carries
    // a `SameSite=None; Secure` cookie, so it's bound when served over HTTPS
    let secure = state.config.secure_cookies;
    let cross_site_post = matches!(p, Provider::Saml(_));
    let browser = crate::auth::random_token();
    let bind = (!cross_site_post || secure).then_some(browser.as_str());
    let res = match &p {
        Provider::Oidc(c) => login(&state, c, next, bind).await,
        Provider::Github(c) => super::github::login(&state, c, next, bind).await,
        #[cfg(feature = "saml")]
        Provider::Saml(c) => super::saml::login(&state, c, next, bind).await,
        _ => return super::login_error("this provider signs in with the password form"),
    };
    match res {
        Ok(mut r) => {
            if bind.is_some()
                && let Ok(v) = HeaderValue::from_str(&super::browser_cookie(&browser, 600, cross_site_post, secure))
            {
                r.headers_mut().append(header::SET_COOKIE, v);
            }
            r
        }
        Err(e) => {
            tracing::warn!(provider = %id, "starting the sign-in failed: {e:#}");
            super::login_error(&format!("{e}"))
        }
    }
}

/// `GET /api/auth/sso/{id}/callback` — the provider sends the browser back here.
pub async fn sso_callback(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap, Query(cb): Query<Callback>) -> Response {
    let Some(p) = super::find(&state, &id).cloned() else { return super::login_error("unknown sign-in provider") };
    let res = match &p {
        Provider::Oidc(c) => complete(&state, &p, c, &headers, cb).await,
        Provider::Github(c) => super::github::complete(&state, &p, c, &headers, cb.code, cb.state, cb.error).await,
        _ => return super::login_error("this provider doesn't sign in here"),
    };
    super::respond(&id, res)
}
