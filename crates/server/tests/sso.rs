//! Single sign-on: OpenID Connect against a mock provider that runs inside the test (discovery,
//! signing keys, authorization and token endpoints, RS256 ID tokens; it checks PKCE and the
//! redirect URI), account linking and creation rules, allowed domains, login-CSRF binding, group
//! → team sync and turning local passwords off.
//!
//! LDAP runs against a real directory when one is reachable — lldap on 127.0.0.1:3890 (or
//! `TASKCRAFT_TEST_LDAP_URL`) with base `dc=acme,dc=test`, admin `uid=admin,ou=people,…` /
//! `adminpassword` and user `dana` / `dana-secret-1` (Dana Directory, dana@acme.test) in group
//! `Engineering`; those tests are skipped otherwise. SAML needs `--features saml` and a test IdP
//! (see the `saml` module at the end).

mod common;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing;
use axum::{Json, Router};
use base64::Engine;
use common::*;
use openidconnect::core::{CoreGenderClaim, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey};
use openidconnect::{
    AdditionalClaims, Audience, EndUserEmail, EndUserName, IdToken, IdTokenClaims, IssuerUrl, JsonWebKeyId, JsonWebKeySet, LocalizedClaim, Nonce,
    PrivateSigningKey, StandardClaims, SubjectIdentifier,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// ---- a mock OpenID Connect provider -------------------------------------------------------

/// A signing key for the whole test run (generated, so no private key lives in the repository).
static KEY_PEM: LazyLock<String> = LazyLock::new(|| {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).expect("RSA key");
    key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).expect("PEM").to_string()
});

/// Whom the provider signs in next.
#[derive(Clone)]
struct Person {
    sub: String,
    email: Option<String>,
    verified: Option<bool>,
    name: Option<String>,
    groups: Option<Vec<String>>,
}

fn person(sub: &str, email: &str, name: &str) -> Person {
    Person { sub: sub.into(), email: Some(email.into()), verified: Some(true), name: Some(name.into()), groups: None }
}

struct Grant {
    client_id: String,
    redirect_uri: String,
    nonce: String,
    challenge: String,
    person: Person,
}

struct Idp {
    issuer: String,
    key: CoreRsaPrivateSigningKey,
    person: Mutex<Person>,
    grants: Mutex<HashMap<String, Grant>>,
    /// redirect URIs the provider was asked to send people back to
    redirects: Mutex<Vec<String>>,
}

impl Idp {
    fn sign_in_as(&self, p: Person) {
        *self.person.lock().unwrap() = p;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Groups {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    groups: Option<Vec<String>>,
}
impl AdditionalClaims for Groups {}

type Token = IdToken<Groups, CoreGenderClaim, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm>;

async fn discovery(State(idp): State<Arc<Idp>>) -> Json<Value> {
    let i = &idp.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "jwks_uri": format!("{i}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

async fn jwks(State(idp): State<Arc<Idp>>) -> Json<Value> {
    Json(serde_json::to_value(JsonWebKeySet::new(vec![idp.key.as_verification_key()])).unwrap())
}

async fn authorize(State(idp): State<Arc<Idp>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let redirect_uri = q["redirect_uri"].clone();
    idp.redirects.lock().unwrap().push(redirect_uri.clone());
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256", "PKCE");
    assert!(q["scope"].split(' ').any(|s| s == "openid"), "{q:?}");
    let code = uuid::Uuid::new_v4().simple().to_string();
    let grant = Grant {
        client_id: q["client_id"].clone(),
        redirect_uri: redirect_uri.clone(),
        nonce: q["nonce"].clone(),
        challenge: q["code_challenge"].clone(),
        person: idp.person.lock().unwrap().clone(),
    };
    idp.grants.lock().unwrap().insert(code.clone(), grant);
    let mut to = reqwest::Url::parse(&redirect_uri).unwrap();
    to.query_pairs_mut().append_pair("code", &code).append_pair("state", &q["state"]);
    Redirect::to(to.as_str()).into_response()
}

async fn token(State(idp): State<Arc<Idp>>, Form(f): Form<HashMap<String, String>>) -> Response {
    let refuse = |e: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    let Some(g) = idp.grants.lock().unwrap().remove(f.get("code").map(String::as_str).unwrap_or("")) else { return refuse("invalid_grant") };
    if f.get("redirect_uri") != Some(&g.redirect_uri) || f.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return refuse("invalid_request");
    }
    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != g.challenge {
        return refuse("invalid_grant");
    }
    let p = g.person;
    let mut std = StandardClaims::new(SubjectIdentifier::new(p.sub)).set_email(p.email.map(EndUserEmail::new)).set_email_verified(p.verified);
    if let Some(n) = p.name {
        let mut l = LocalizedClaim::new();
        l.insert(None, EndUserName::new(n));
        std = std.set_name(Some(l));
    }
    let now = chrono::Utc::now();
    let claims = IdTokenClaims::new(
        IssuerUrl::new(idp.issuer.clone()).unwrap(),
        vec![Audience::new(g.client_id)],
        now + chrono::Duration::minutes(5),
        now,
        std,
        Groups { groups: p.groups },
    )
    .set_nonce(Some(Nonce::new(g.nonce)));
    let id_token = Token::new(claims, &idp.key, CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256, None, None).unwrap();
    Json(json!({"access_token": "mock-access-token", "token_type": "Bearer", "expires_in": 300, "id_token": id_token.to_string()})).into_response()
}

/// Starts a mock provider on an ephemeral port; it signs in `sso.user@acme.test` until told otherwise.
async fn idp() -> Arc<Idp> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let key = CoreRsaPrivateSigningKey::from_pem(&KEY_PEM, Some(JsonWebKeyId::new("test-key".into()))).unwrap();
    let idp = Arc::new(Idp {
        issuer,
        key,
        person: Mutex::new(person("user-1", "sso.user@acme.test", "Sso User")),
        grants: Default::default(),
        redirects: Default::default(),
    });
    let app = Router::new()
        .route("/.well-known/openid-configuration", routing::get(discovery))
        .route("/jwks", routing::get(jwks))
        .route("/authorize", routing::get(authorize))
        .route("/token", routing::post(token))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    idp
}

fn oidc(id: &str, idp: &Idp, extra: Value) -> Value {
    let mut p = json!({"type": "oidc", "id": id, "issuer": idp.issuer, "clientId": "taskcraft", "clientSecret": "secret"});
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    p
}

async fn spawn_with_providers(providers: Value) -> TestApp {
    spawn_with(|c| c.sso.auth_providers = Some(providers.to_string())).await
}

/// A browser: keeps cookies, follows redirects, no CSRF header.
fn browser() -> reqwest::Client {
    reqwest::Client::builder().cookie_store(true).build().unwrap()
}

/// A browser that stops at each redirect.
fn manual() -> reqwest::Client {
    reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

/// Runs a whole sign-in at provider `id`; returns where the browser ended up.
async fn sign_in(app: &TestApp, b: &reqwest::Client, id: &str, next: &str) -> reqwest::Url {
    let r = b.get(app.url(&format!("/auth/sso/{id}/login?next={}", urlencode(next)))).send().await.unwrap();
    r.url().clone()
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The `?error=` the login page was given.
fn error_of(u: &reqwest::Url) -> String {
    assert_eq!(u.path(), "/login", "expected the login page, got {u}");
    u.query_pairs().find(|(k, _)| k == "error").map(|(_, v)| v.to_string()).unwrap_or_default()
}

fn location(r: &reqwest::Response) -> String {
    r.headers().get("location").map(|l| l.to_str().unwrap().to_string()).unwrap_or_default()
}

async fn me(app: &TestApp, b: &reqwest::Client) -> Value {
    get(b, app.url("/me")).await["user"].clone()
}

/// Names of the teams `user` is in (sorted, as the API lists them).
async fn teams_of(app: &TestApp, admin: &reqwest::Client, user: &str) -> Vec<String> {
    let all = get(admin, app.url("/teams")).await;
    all.as_array()
        .unwrap()
        .iter()
        .filter(|t| t["memberIds"].as_array().unwrap().iter().any(|m| m == user))
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&app.state.db).await.unwrap()
}

// ---- OpenID Connect -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn oidc_sign_in_accepts_invitations_and_links_by_subject() {
    let idp = idp().await;
    let issuer = idp.issuer.clone();
    let app = spawn_with(|c| {
        c.sso.oidc_issuer = Some(issuer);
        c.sso.oidc_client_id = Some("taskcraft".into());
        c.sso.oidc_client_secret = Some("secret".into());
    })
    .await;
    let owner = app.setup(false).await;
    let info = get(&client(), app.url("/auth/providers")).await;
    assert_eq!(info["passwordLogin"], true);
    assert_eq!(info["providers"], json!([{"id": "oidc", "name": "Single sign-on", "type": "oidc", "preset": null, "loginUrl": "/api/auth/sso/oidc/login"}]));

    // an invited person signs in with the provider: the invitation is taken (case-insensitive e-mail)
    let invited = post(&owner, app.url("/users"), json!({"name": "Invited Person", "email": "sso.user@acme.test"})).await;
    let invited_id = invited["id"].as_str().unwrap().to_string();
    let invite_token = invited["inviteUrl"].as_str().unwrap().split("token=").nth(1).unwrap().to_string();
    idp.sign_in_as(person("user-1", "SSO.User@acme.test", "Sso User"));
    let b = browser();
    let end = sign_in(&app, &b, "oidc", "/projects?view=all").await;
    assert_eq!((end.path(), end.query()), ("/projects", Some("view=all")), "{end}");
    assert_eq!(idp.redirects.lock().unwrap()[0], format!("{}/api/auth/sso/oidc/callback", app.base));
    let u = me(&app, &b).await;
    assert_eq!(u["id"], invited_id.as_str());
    assert_eq!(u["email"], "sso.user@acme.test");
    assert_eq!(u["status"], "active");
    assert_eq!(u["name"], "Sso User");
    let (s, _) = send(&client(), Method::GET, app.url(&format!("/auth/link?token={invite_token}")), json!(null)).await;
    assert_eq!(s, 400, "the invitation link is used up");

    // the next sign-in finds the account by (issuer, subject), even with another e-mail address
    idp.sign_in_as(person("user-1", "renamed@acme.test", "Sso User"));
    let b2 = browser();
    sign_in(&app, &b2, "oidc", "/").await;
    assert_eq!(me(&app, &b2).await["id"], invited_id.as_str());
    assert_eq!(count(&app, "SELECT count(*) FROM identities").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM identities WHERE provider_id = 'oidc' AND last_login_at IS NOT NULL").await, 1);

    // how people sign in, for admins and for the person
    let users = get(&owner, app.url("/users")).await;
    let row = |id: &str| users.as_array().unwrap().iter().find(|u| u["id"] == id).unwrap().clone();
    assert_eq!(row(&invited_id)["signIn"], json!({"password": false, "providers": [{"id": "oidc", "name": "Single sign-on", "type": "oidc"}]}));
    let owner_id = me(&app, &owner).await["id"].as_str().unwrap().to_string();
    assert_eq!(row(&owner_id)["signIn"], json!({"password": true, "providers": []}));
    let mine = get(&b2, app.url("/me/sign-in")).await;
    assert_eq!(mine["password"], false);
    assert_eq!(mine["providers"][0]["name"], "Single sign-on");

    // a forged or replayed state, a provider error, an unsafe `next`
    let r = b2.get(app.url("/auth/sso/oidc/callback?code=x&state=forged")).send().await.unwrap();
    assert!(error_of(r.url()).contains("expired"), "{}", r.url());
    let r = b2.get(app.url("/auth/sso/oidc/callback?error=access_denied&error_description=nope")).send().await.unwrap();
    assert!(error_of(r.url()).contains("access_denied"), "{}", r.url());
    let end = sign_in(&app, &browser(), "oidc", "//evil.example/x").await;
    assert_eq!((end.host_str(), end.path()), (reqwest::Url::parse(&app.base).unwrap().host_str(), "/"), "{end}");

    // deactivated people can't sign in with the provider either
    let (s, _) = send(&owner, Method::PATCH, app.url(&format!("/users/{invited_id}")), json!({"status": "deactivated"})).await;
    assert_eq!(s, 204);
    let end = sign_in(&app, &browser(), "oidc", "/").await;
    assert!(error_of(&end).contains("deactivated"), "{end}");
}

#[tokio::test(flavor = "multi_thread")]
async fn several_providers_domain_rules_and_login_csrf() {
    let idp = idp().await;
    let app = spawn_with_providers(json!([
        oidc("corp", &idp, json!({"name": "Corp SSO", "allowedDomains": ["acme.test"]})),
        oidc("partner", &idp, json!({"name": "Partner SSO", "allowedDomains": ["partner.test"]})),
    ]))
    .await;
    app.setup(false).await;
    let info = get(&client(), app.url("/auth/providers")).await;
    let list = info["providers"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["loginUrl"], "/api/auth/sso/corp/login");
    assert_eq!(list[1]["name"], "Partner SSO");

    // sso.user@acme.test isn't from the partner's domain
    let end = sign_in(&app, &browser(), "partner", "/").await;
    assert!(error_of(&end).contains("not allowed"), "{end}");
    let b = browser();
    let end = sign_in(&app, &b, "corp", "/projects").await;
    assert_eq!(end.path(), "/projects");
    assert_eq!(me(&app, &b).await["email"], "sso.user@acme.test");
    // an address the provider doesn't vouch for doesn't pass the domain rule
    idp.sign_in_as(Person { verified: Some(false), ..person("user-2", "eve@acme.test", "Eve") });
    let end = sign_in(&app, &browser(), "corp", "/").await;
    assert!(error_of(&end).contains("not allowed"), "{end}");

    // a sign-in started at one provider can't be completed at another
    let start = |c: reqwest::Client| {
        let url = app.url("/auth/sso/corp/login");
        async move {
            let r = c.get(url).send().await.unwrap();
            assert_eq!(r.status(), 303);
            let to = reqwest::Url::parse(&location(&r)).unwrap();
            to.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string()
        }
    };
    let b = manual();
    let state = start(b.clone()).await;
    let r = b.get(app.url(&format!("/auth/sso/partner/callback?code=x&state={}", urlencode(&state)))).send().await.unwrap();
    assert!(location(&r).contains("another%20provider"), "{}", location(&r));

    // login CSRF: a sign-in started in one browser can't be completed in another one
    let attacker = manual();
    let state = start(attacker.clone()).await;
    let victim = manual();
    let r = victim.get(app.url(&format!("/auth/sso/corp/callback?code=x&state={}", urlencode(&state)))).send().await.unwrap();
    assert!(location(&r).contains("different%20browser"), "{}", location(&r));

    // unknown providers; LDAP-style providers have no redirect
    let end = sign_in(&app, &browser(), "nope", "/").await;
    assert!(error_of(&end).contains("unknown"), "{end}");
}

#[tokio::test(flavor = "multi_thread")]
async fn account_linking_and_creation_rules() {
    let idp = idp().await;
    let issuer = idp.issuer.clone();
    let app = spawn_with_providers(json!([
        oidc("closed", &idp, json!({"autoCreate": false})),
        oidc("open", &idp, json!({"defaultRole": "guest"})),
        // trusts addresses without `email_verified`, like a single-tenant Entra ID
        {"type": "oidc", "id": "entra", "preset": "microsoft", "tenant": "acme", "issuer": issuer, "clientId": "taskcraft", "clientSecret": "secret"},
    ]))
    .await;
    let owner = app.setup(false).await;
    let (_, member_id) = app.user(&owner, "Max Member", "max@acme.test", "member").await;

    // without autoCreate only existing accounts may sign in
    idp.sign_in_as(person("new-1", "newbie@acme.test", "Newbie"));
    let end = sign_in(&app, &browser(), "closed", "/").await;
    assert!(error_of(&end).contains("ask an administrator"), "{end}");
    idp.sign_in_as(person("max-1", "max@acme.test", "Max M."));
    let b = browser();
    sign_in(&app, &b, "closed", "/").await;
    assert_eq!(me(&app, &b).await["id"], member_id.as_str(), "linked by verified e-mail");
    assert_eq!(me(&app, &b).await["name"], "Max Member", "an active account keeps its name");

    // new accounts get the provider's default role
    idp.sign_in_as(person("new-1", "newbie@acme.test", "Newbie"));
    let b = browser();
    sign_in(&app, &b, "open", "/").await;
    let u = me(&app, &b).await;
    assert_eq!((u["role"].as_str(), u["email"].as_str(), u["name"].as_str()), (Some("guest"), Some("newbie@acme.test"), Some("Newbie")));

    // an unverified address never links to (or takes over) an existing account
    idp.sign_in_as(Person { verified: Some(false), ..person("mallory-1", "max@acme.test", "Mallory") });
    let b = browser();
    sign_in(&app, &b, "open", "/").await;
    let u = me(&app, &b).await;
    assert_ne!(u["id"], member_id.as_str());
    assert!(u["email"].is_null(), "the unverified address isn't stored: {u}");

    // an administrator's account is only linked with a verified address or `trustEmail`
    idp.sign_in_as(Person { verified: None, ..person("ms-owner", "owner@example.com", "Olivia") });
    let end = sign_in(&app, &browser(), "entra", "/").await;
    assert!(error_of(&end).contains("administrator account"), "{end}");
    assert_eq!(count(&app, "SELECT count(*) FROM identities i JOIN users u ON u.id = i.user_id WHERE u.role = 'owner'").await, 0);
    idp.sign_in_as(person("ms-owner", "owner@example.com", "Olivia"));
    let b = browser();
    sign_in(&app, &b, "entra", "/").await;
    assert_eq!(me(&app, &b).await["role"], "owner", "a verified address links the owner");
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_groups_become_teams() {
    let idp = idp().await;
    let app = spawn_with_providers(json!([oidc("corp", &idp, json!({"groupsClaim": "groups", "syncGroups": true}))])).await;
    let owner = app.setup(false).await;
    // a local team that happens to have the name of a provider group
    let local = post(&owner, app.url("/teams"), json!({"name": "Engineering", "memberIds": []})).await["id"].as_str().unwrap().to_string();

    idp.sign_in_as(Person { groups: Some(vec!["Engineering".into(), "Design".into()]), ..person("u-1", "dana@acme.test", "Dana") });
    let b = browser();
    sign_in(&app, &b, "corp", "/").await;
    let uid = me(&app, &b).await["id"].as_str().unwrap().to_string();
    assert_eq!(teams_of(&app, &owner, &uid).await, ["Design", "Engineering (corp)"], "the local team isn't taken over");

    // membership follows the provider; teams people were added to by hand stay
    send(&owner, Method::PATCH, app.url(&format!("/teams/{local}")), json!({"memberIds": [uid]})).await;
    idp.sign_in_as(Person { groups: Some(vec!["Design".into()]), ..person("u-1", "dana@acme.test", "Dana") });
    sign_in(&app, &browser(), "corp", "/").await;
    assert_eq!(teams_of(&app, &owner, &uid).await, ["Design", "Engineering"]);
    assert_eq!(count(&app, "SELECT count(*) FROM teams WHERE external_ref IS NOT NULL").await, 2, "provider teams are kept");
    // a second person of the same group joins the same team
    idp.sign_in_as(Person { groups: Some(vec!["Design".into()]), ..person("u-2", "erin@acme.test", "Erin") });
    sign_in(&app, &browser(), "corp", "/").await;
    assert_eq!(count(&app, "SELECT count(*) FROM team_members m JOIN teams t ON t.id = m.team_id WHERE t.name = 'Design'").await, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn password_sign_in_can_be_turned_off() {
    let idp = idp().await;
    let providers = json!([oidc("corp", &idp, json!({}))]);
    let app = spawn_with(|c| {
        c.sso.auth_providers = Some(providers.to_string());
        c.sso.disable_password_login = true;
    })
    .await;
    // the first-run setup still asks for a password and signs the owner in
    let owner = app.setup(false).await;
    assert_eq!(me(&app, &owner).await["role"], "owner");
    assert_eq!(get(&client(), app.url("/auth/providers")).await["passwordLogin"], false);
    let (s, v) = send(&client(), Method::POST, app.url("/auth/login"), json!({"email": "owner@example.com", "password": "password123"})).await;
    assert_eq!(s, 400, "{v}");
    let (s, _) = send(&owner, Method::PUT, app.url("/me/password"), json!({"current": "password123", "newPassword": "password456"})).await;
    assert_eq!(s, 400);
    // invitations are accepted by signing in with the provider, not with a password
    let invited = post(&owner, app.url("/users"), json!({"name": "Ina", "email": "ina@acme.test"})).await;
    let token = invited["inviteUrl"].as_str().unwrap().split("token=").nth(1).unwrap().to_string();
    let (s, _) = send(&client(), Method::POST, app.url("/auth/link"), json!({"token": token, "password": "password123"})).await;
    assert_eq!(s, 400);
    idp.sign_in_as(person("ina-1", "ina@acme.test", "Ina"));
    let b = browser();
    sign_in(&app, &b, "corp", "/").await;
    assert_eq!(me(&app, &b).await["id"], invited["id"]);
    // the owner signs in with the provider too
    idp.sign_in_as(person("olivia-1", "owner@example.com", "Olivia Owner"));
    let b = browser();
    sign_in(&app, &b, "corp", "/").await;
    assert_eq!(me(&app, &b).await["role"], "owner");
    let users = get(&owner, app.url("/users")).await;
    assert!(users.as_array().unwrap().iter().all(|u| u["signIn"]["password"] == false), "passwords don't count: {users}");
}

#[tokio::test]
async fn invalid_provider_configuration_stops_the_start() {
    let config = |json: &str| taskcraft::sso::SsoConfig { auth_providers: Some(json.into()), ..Default::default() };
    assert!(taskcraft::sso::load(&config(r#"[{"type":"oidc","id":"x","clientId":"a"}]"#)).is_err(), "no issuer");
    assert!(taskcraft::sso::load(&config("not json")).is_err());
    assert_eq!(taskcraft::sso::load(&config("[]")).unwrap().len(), 0);
}

// ---- LDAP / Active Directory ------------------------------------------------------------------

fn ldap_url() -> String {
    std::env::var("TASKCRAFT_TEST_LDAP_URL").unwrap_or_else(|_| "ldap://127.0.0.1:3890".into())
}

fn ldap_providers() -> String {
    json!([{
        "type": "ldap",
        "id": "corp",
        "name": "ACME directory",
        "url": ldap_url(),
        "bindDn": "uid=admin,ou=people,dc=acme,dc=test",
        "bindPassword": "adminpassword",
        "baseDn": "dc=acme,dc=test",
        "syncGroups": true,
    }])
    .to_string()
}

async fn ldap_running() -> bool {
    let url = ldap_url();
    let addr = url.trim_start_matches("ldap://").trim_start_matches("ldaps://").trim_end_matches('/').to_string();
    let ok = matches!(tokio::time::timeout(std::time::Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await, Ok(Ok(_)));
    if !ok {
        eprintln!("skipping: no LDAP server at {url}");
    }
    ok
}

async fn login(app: &TestApp, user: &str, password: &str) -> (u16, Value, reqwest::Client) {
    let c = client();
    let (s, v) = send(&c, Method::POST, app.url("/auth/login"), json!({"email": user, "password": password})).await;
    (s, v, c)
}

#[tokio::test(flavor = "multi_thread")]
async fn ldap_sign_in_creates_accounts_and_syncs_teams() {
    if !ldap_running().await {
        return;
    }
    let app = spawn_with(|c| c.sso.auth_providers = Some(ldap_providers())).await;
    let owner = app.setup(false).await;
    let info = get(&client(), app.url("/auth/providers")).await;
    assert_eq!(info["providers"][0]["type"], "ldap");
    assert_eq!(info["providers"][0]["loginUrl"], Value::Null, "LDAP signs in through the password form");

    let (s, v, dana) = login(&app, "dana", "dana-secret-1").await;
    assert_eq!(s, 200, "{v}");
    let u = me(&app, &dana).await;
    assert_eq!((u["email"].as_str(), u["name"].as_str(), u["role"].as_str()), (Some("dana@acme.test"), Some("Dana Directory"), Some("member")));
    let dana_id = u["id"].as_str().unwrap().to_string();
    let teams: Vec<String> = sqlx::query_scalar("SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id WHERE m.user_id = $1::uuid")
        .bind(&dana_id)
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert!(teams.contains(&"Engineering".to_string()), "{teams:?}");

    // the e-mail address works as the login name too
    let (s, _, again) = login(&app, "dana@acme.test", "dana-secret-1").await;
    assert_eq!(s, 200);
    assert_eq!(me(&app, &again).await["id"], dana_id.as_str());
    // wrong password, empty password, unknown user, filter injection
    for (user, pw) in [("dana", "wrong"), ("dana", ""), ("nobody", "x"), ("*", "dana-secret-1"), ("dana)(uid=*", "dana-secret-1")] {
        let (s, _, _) = login(&app, user, pw).await;
        assert_eq!(s, 403, "{user}/{pw}");
    }
    // local accounts still work next to the directory
    assert_eq!(login(&app, "owner@example.com", "password123").await.0, 200);
    // deactivated directory accounts are refused even with the right directory password
    let (s, _) = send(&owner, Method::PATCH, app.url(&format!("/users/{dana_id}")), json!({"status": "deactivated"})).await;
    assert_eq!(s, 204);
    let (s, v, _) = login(&app, "dana", "dana-secret-1").await;
    assert_eq!(s, 403);
    assert!(v["message"].as_str().unwrap().contains("deactivated"), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ldap_still_works_when_local_passwords_are_off() {
    if !ldap_running().await {
        return;
    }
    let app = spawn_with(|c| {
        c.sso.auth_providers = Some(ldap_providers());
        c.sso.disable_password_login = true;
    })
    .await;
    app.setup(false).await;
    assert_eq!(get(&client(), app.url("/auth/providers")).await["passwordLogin"], false);
    assert_eq!(login(&app, "owner@example.com", "password123").await.0, 403, "local password refused");
    assert_eq!(login(&app, "dana", "dana-secret-1").await.0, 200, "directory sign-in allowed");
}

#[tokio::test(flavor = "multi_thread")]
async fn directory_sign_ins_dont_take_over_admins_or_local_teams() {
    if !ldap_running().await {
        return;
    }
    let app = spawn_with(|c| c.sso.auth_providers = Some(ldap_providers())).await;
    let owner = app.setup(false).await;
    // a local admin happens to have the directory person's e-mail
    let (_, admin_id) = app.user(&owner, "Dana Admin", "dana@acme.test", "admin").await;
    let (s, v, _) = login(&app, "dana", "dana-secret-1").await;
    assert_eq!(s, 403);
    assert!(v["message"].as_str().unwrap().contains("administrator account"), "{v}");
    assert_eq!(count(&app, &format!("SELECT count(*) FROM identities WHERE user_id = '{admin_id}'")).await, 0);

    // as an ordinary account it's linked; a local team named like a directory group isn't taken over
    send(&owner, Method::PATCH, app.url(&format!("/users/{admin_id}")), json!({"role": "member"})).await;
    let local = post(&owner, app.url("/teams"), json!({"name": "Engineering", "memberIds": []})).await["id"].as_str().unwrap().to_string();
    let (s, v, _) = login(&app, "dana", "dana-secret-1").await;
    assert_eq!(s, 200, "{v}");
    let teams: Vec<(String, String)> =
        sqlx::query_as("SELECT t.id::text, t.name FROM team_members m JOIN teams t ON t.id = m.team_id WHERE m.user_id = $1::uuid ORDER BY t.name")
            .bind(&admin_id)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(!teams.iter().any(|(id, _)| *id == local), "local team untouched: {teams:?}");
    assert!(teams.iter().any(|(_, n)| n == "Engineering (corp)"), "{teams:?}");
}

// ---- SAML 2.0 (cargo feature `saml`) ----------------------------------------------------------

/// Against a real IdP (SimpleSAMLphp):
///
/// ```sh
/// docker run -d --name tc-saml-idp -p 127.0.0.1:8180:8080 \
///   -e SIMPLESAMLPHP_SP_ENTITY_ID=http://localhost:8790/api/auth/sso/okta/metadata \
///   -e SIMPLESAMLPHP_SP_ASSERTION_CONSUMER_SERVICE=http://localhost:8790/api/auth/sso/okta/acs \
///   kristophjunge/test-saml-idp
/// # the image signs with RSA-SHA1 by default, which TaskCraft refuses; switch it to SHA-256:
/// docker exec tc-saml-idp sed -i "s|//'signature.algorithm'|'signature.algorithm'|" \
///   /var/www/simplesamlphp/metadata/saml20-idp-hosted.php
/// ```
///
/// Skipped when the IdP isn't running.
#[cfg(feature = "saml")]
mod saml {
    use super::*;

    const METADATA: &str = "http://localhost:8180/simplesaml/saml2/idp/metadata.php";

    /// Value of `name="…" value="…"` in an HTML form.
    fn input(html: &str, name: &str) -> Option<String> {
        let at = html.find(&format!("name=\"{name}\""))?;
        let rest = &html[at..];
        let v = rest.find("value=\"")? + 7;
        let end = rest[v..].find('"')?;
        Some(rest[v..v + end].replace("&quot;", "\"").replace("&amp;", "&").replace("&#x3D;", "=").replace("&#x2B;", "+").replace("&#x2F;", "/"))
    }

    fn form_action(html: &str) -> Option<String> {
        let at = html.find("action=\"")? + 8;
        Some(html[at..at + html[at..].find('"')?].replace("&amp;", "&"))
    }

    /// Runs the browser part of the flow; returns the (SAMLResponse, RelayState) the IdP posts back.
    async fn idp_login(app: &TestApp, user: &str, password: &str) -> (String, String) {
        let idp = reqwest::Client::builder().cookie_store(true).build().unwrap();
        let r = manual().get(app.url("/auth/sso/okta/login")).send().await.unwrap();
        assert_eq!(r.status(), 303);
        let to_idp = location(&r);
        assert!(to_idp.starts_with("http://localhost:8180/"), "{to_idp}");
        let login_page = idp.get(&to_idp).send().await.unwrap();
        let login_url = login_page.url().clone();
        let html = login_page.text().await.unwrap();
        let auth_state = input(&html, "AuthState").expect("IdP login form");
        let action = login_url.join(&form_action(&html).unwrap_or_default()).unwrap();
        let html = idp
            .post(action)
            .form(&[("username", user), ("password", password), ("AuthState", auth_state.as_str())])
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        (input(&html, "SAMLResponse").expect("IdP posts a SAMLResponse"), input(&html, "RelayState").expect("RelayState"))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saml_sign_in_against_simplesamlphp() {
        if reqwest::get(METADATA).await.is_err() {
            eprintln!("skipping: SAML test IdP not running");
            return;
        }
        let providers = json!([{"type": "saml", "id": "okta", "name": "Test IdP", "idpMetadataUrl": METADATA, "groupsAttribute": "eduPersonAffiliation", "syncGroups": true}]);
        // the IdP knows the SP as localhost:8790; requests still go to the test server's real port
        let app = spawn_with(|c| {
            c.base_url = "http://localhost:8790".into();
            c.sso.auth_providers = Some(providers.to_string());
        })
        .await;
        app.setup(false).await;

        let md = reqwest::get(app.url("/auth/sso/okta/metadata")).await.unwrap().text().await.unwrap();
        assert!(md.contains(r#"entityID="http://localhost:8790/api/auth/sso/okta/metadata""#) && md.contains("/api/auth/sso/okta/acs"), "{md}");
        let acs =
            |b: &reqwest::Client, resp: &str, relay: &str| b.post(app.url("/auth/sso/okta/acs")).form(&[("SAMLResponse", resp), ("RelayState", relay)]).send();

        // happy path: account created, groups → teams
        let (resp, relay) = idp_login(&app, "user1", "user1pass").await;
        let b = manual();
        let r = acs(&b, &resp, &relay).await.unwrap();
        assert_eq!(r.status(), 303);
        assert_eq!(location(&r), "/", "signed in");
        assert_eq!(me(&app, &b).await["email"], "user1@example.com");
        let teams: Vec<String> = sqlx::query_scalar(
            "SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id JOIN users u ON u.id = m.user_id WHERE u.email = 'user1@example.com'",
        )
        .fetch_all(&app.state.db)
        .await
        .unwrap();
        assert_eq!(teams, vec!["group1".to_string()]);

        // replaying the same response is refused (single-use state)
        let r = acs(&manual(), &resp, &relay).await.unwrap();
        assert!(location(&r).starts_with("/login?error="), "{}", location(&r));

        // a tampered assertion fails signature verification
        let (resp, relay) = idp_login(&app, "user2", "user2pass").await;
        let xml =
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(&resp).unwrap()).unwrap().replace("user2@example.com", "owner@example.com");
        let forged = base64::engine::general_purpose::STANDARD.encode(xml);
        let fresh = manual();
        let r = acs(&fresh, &forged, &relay).await.unwrap();
        let loc = location(&r);
        assert!(loc.starts_with("/login?error=") && loc.contains("signed"), "{loc}");
        assert!(me(&app, &fresh).await.is_null(), "not signed in");

        // IdP-initiated responses (no RelayState) are refused
        let r = fresh.post(app.url("/auth/sso/okta/acs")).form(&[("SAMLResponse", resp.as_str())]).send().await.unwrap();
        assert!(location(&r).starts_with("/login?error=IdP"), "{}", location(&r));
    }
}
