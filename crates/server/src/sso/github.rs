//! GitHub (and GitHub Enterprise Server) sign-in via OAuth2.

use std::time::Duration;

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{External, GithubConfig, Provider, SignInError};
use crate::app::AppState;

#[derive(Serialize, Deserialize)]
struct Pending {
    provider: String,
    next: String,
}

fn web(c: &GithubConfig) -> String {
    c.web_url.clone().unwrap_or_else(|| "https://github.com".into()).trim_end_matches('/').to_string()
}

fn api(c: &GithubConfig) -> String {
    c.api_url.clone().unwrap_or_else(|| "https://api.github.com".into()).trim_end_matches('/').to_string()
}

fn http() -> openidconnect::reqwest::Client {
    openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("TaskCraft/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

/// Starts the sign-in: redirect to GitHub's authorization page.
pub async fn login(state: &AppState, c: &GithubConfig, next: String, browser: Option<&str>) -> anyhow::Result<Response> {
    let key = super::put_state(state, &serde_json::to_value(Pending { provider: c.common.id.clone(), next })?, browser).await?;
    let scope = if c.allowed_orgs.is_empty() { "read:user user:email" } else { "read:user user:email read:org" };
    let url = format!(
        "{}/login/oauth/authorize?client_id={}&redirect_uri={}&scope={}&state={}",
        web(c),
        super::urlencode(&c.client_id),
        super::urlencode(&super::oidc::redirect_uri(state, &c.common.id)),
        super::urlencode(scope),
        super::urlencode(&key)
    );
    Ok(Redirect::to(&url).into_response())
}

async fn get_json(token: &str, url: &str) -> anyhow::Result<Value> {
    let text = http().get(url).bearer_auth(token).header("accept", "application/vnd.github+json").send().await?.error_for_status()?.text().await?;
    Ok(serde_json::from_str(&text)?)
}

/// The callback: exchanges the code, reads the profile, verified primary e-mail and (when
/// restricted) organizations.
pub async fn complete(
    state: &AppState,
    provider: &Provider,
    c: &GithubConfig,
    headers: &HeaderMap,
    code: Option<String>,
    key: Option<String>,
    error: Option<String>,
) -> Result<Response, SignInError> {
    if let Some(e) = error {
        return Err(SignInError::Denied(format!("GitHub says: {e}")));
    }
    let (Some(code), Some(key)) = (code, key) else { return Err(SignInError::Denied("GitHub sent no code".into())) };
    let pending: Pending = serde_json::from_value(super::take_state(state, &key, headers).await?)
        .map_err(|_| SignInError::Denied("the sign-in expired, please try again".into()))?;
    if pending.provider != c.common.id {
        return Err(SignInError::Denied("this sign-in was started at another provider".into()));
    }
    let text = http()
        .post(format!("{}/login/oauth/access_token", web(c)))
        .header("accept", "application/json")
        .form(&[
            ("client_id", c.client_id.as_str()),
            ("client_secret", c.client_secret.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", &super::oidc::redirect_uri(state, &c.common.id)),
        ])
        .send()
        .await
        .map_err(anyhow::Error::from)?
        .text()
        .await
        .map_err(anyhow::Error::from)?;
    let tok: Value = serde_json::from_str(&text).map_err(anyhow::Error::from)?;
    let token = tok["access_token"]
        .as_str()
        .ok_or_else(|| SignInError::Denied(format!("GitHub token exchange failed: {}", tok["error_description"].as_str().unwrap_or("unknown error"))))?
        .to_string();
    let user = get_json(&token, &format!("{}/user", api(c))).await?;
    let emails = get_json(&token, &format!("{}/user/emails", api(c))).await.unwrap_or(json!([]));
    let primary =
        emails.as_array().into_iter().flatten().find(|e| e["primary"] == true && e["verified"] == true).and_then(|e| e["email"].as_str()).map(String::from);
    if !c.allowed_orgs.is_empty() {
        let orgs = get_json(&token, &format!("{}/user/orgs", api(c))).await?;
        let member =
            orgs.as_array().into_iter().flatten().filter_map(|o| o["login"].as_str()).any(|l| c.allowed_orgs.iter().any(|a| a.eq_ignore_ascii_case(l)));
        if !member {
            return Err(SignInError::Denied("your GitHub account isn't a member of an allowed organization".into()));
        }
    }
    let ext = External {
        provider_key: format!("github:{}", web(c)),
        subject: user["id"].as_i64().map(|i| i.to_string()).ok_or_else(|| anyhow::anyhow!("GitHub user without id"))?,
        email_verified: Some(primary.is_some()),
        email: primary,
        name: user["name"].as_str().or(user["login"].as_str()).map(String::from),
        groups: vec![],
    };
    let uid = super::sign_in(state, provider, ext).await?;
    super::finish(state, uid, headers, &pending.next).await
}
