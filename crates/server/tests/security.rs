//! Two-factor authentication (authenticator app, recovery codes, passkeys), the workspace's
//! "require two-factor authentication", sessions, and who may change sign-in methods.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use taskcraft::mfa;
use taskcraft::webauthn::testing::Authenticator;
use taskcraft::webauthn::{RelyingParty, b64, unb64};

fn code_at(secret: &str, step: u64) -> String {
    mfa::code_at(&mfa::base32_decode(secret).unwrap(), step)
}

/// A code for the next 30-second step (accepted for clock drift; the current one may be used up).
fn next_code(secret: &str) -> String {
    code_at(secret, mfa::current_step() + 1)
}

async fn call(app: &TestApp, c: &reqwest::Client, method: Method, path: &str, body: Value) -> (u16, Value) {
    send(c, method, app.url(path), body).await
}

/// DELETE without a body (what the web app sends when no password is needed).
async fn del(app: &TestApp, c: &reqwest::Client, path: &str) -> (u16, Value) {
    let r = c.delete(app.url(path)).send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// Sets up the authenticator app; returns its secret and the recovery codes.
async fn enrol(app: &TestApp, c: &reqwest::Client) -> (String, Vec<String>) {
    let (st, setup) = call(app, c, Method::POST, "/me/mfa/setup", json!({})).await;
    assert_eq!(st, 200, "{setup}");
    let secret = setup["secret"].as_str().unwrap().to_string();
    let (st, en) = call(app, c, Method::POST, "/me/mfa/enable", json!({"code": code_at(&secret, mfa::current_step())})).await;
    assert_eq!(st, 200, "{en}");
    let codes = en["recoveryCodes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    (secret, codes)
}

/// Password login of someone with a second factor: no session yet, a token for the second step.
async fn first_step(app: &TestApp, c: &reqwest::Client, email: &str) -> Value {
    let (st, body) = call(app, c, Method::POST, "/auth/login", json!({"email": email, "password": "password123"})).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["mfaRequired"], true, "{body}");
    assert!(get(c, app.url("/me")).await["user"].is_null(), "no session before the second step");
    body
}

#[tokio::test]
async fn totp_login_recovery_codes_sessions_and_admin_reset() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (mia, mia_id) = app.user(&owner, "Mia Ramos", "mia@example.com", "member").await;

    // set up: a pending secret, confirmed with a code → recovery codes
    let s = get(&mia, app.url("/me/mfa")).await;
    assert_eq!((s["enabled"].clone(), s["passkeys"].clone(), s["required"].clone()), (json!(false), json!(0), json!(false)));
    let (_, setup) = call(&app, &mia, Method::POST, "/me/mfa/setup", json!({})).await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    assert!(setup["uri"].as_str().unwrap().starts_with("otpauth://totp/TaskCraft:mia@example.com?secret="));
    assert!(setup["qrSvg"].as_str().unwrap().contains("<svg"));
    assert_eq!(call(&app, &mia, Method::POST, "/me/mfa/enable", json!({"code": "12345"})).await.0, 400);
    let used = code_at(&secret, mfa::current_step());
    let (st, en) = call(&app, &mia, Method::POST, "/me/mfa/enable", json!({"code": used})).await;
    assert_eq!(st, 200, "{en}");
    let recovery: Vec<String> = en["recoveryCodes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    assert_eq!(recovery.len(), 10);
    let s = get(&mia, app.url("/me/mfa")).await;
    assert_eq!((s["enabled"].clone(), s["recoveryCodesLeft"].clone()), (json!(true), json!(10)));

    // the password alone no longer signs in
    let c1 = client();
    let step = first_step(&app, &c1, "mia@example.com").await;
    assert_eq!(step["methods"], json!(["code"]));
    let token = step["mfaToken"].as_str().unwrap();
    assert_eq!(call(&app, &c1, Method::POST, "/auth/mfa", json!({"mfaToken": token, "code": "123456x"})).await.0, 400);
    assert_eq!(call(&app, &c1, Method::POST, "/auth/mfa", json!({"mfaToken": "made-up", "code": next_code(&secret)})).await.0, 400);
    let (st, _) = call(&app, &c1, Method::POST, "/auth/mfa", json!({"mfaToken": token, "code": used})).await;
    assert_eq!(st, 400, "a code works once");
    let (st, body) = call(&app, &c1, Method::POST, "/auth/mfa", json!({"mfaToken": token, "code": next_code(&secret)})).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(get(&c1, app.url("/me")).await["user"]["id"], mia_id.as_str());
    assert_eq!(call(&app, &c1, Method::POST, "/auth/mfa", json!({"mfaToken": token, "code": recovery[5]})).await.0, 400, "the step is used up");

    // recovery codes: any case, dashes optional, once each
    let c2 = client();
    let token2 = first_step(&app, &c2, "mia@example.com").await["mfaToken"].as_str().unwrap().to_string();
    let rc = recovery[0].to_uppercase().replace('-', "");
    assert_eq!(call(&app, &c2, Method::POST, "/auth/mfa", json!({"mfaToken": token2, "code": rc})).await.0, 200);
    let c3 = client();
    let token3 = first_step(&app, &c3, "mia@example.com").await["mfaToken"].as_str().unwrap().to_string();
    assert_eq!(call(&app, &c3, Method::POST, "/auth/mfa", json!({"mfaToken": token3, "code": recovery[0]})).await.0, 400, "recovery codes are single-use");
    assert_eq!(call(&app, &c3, Method::POST, "/auth/mfa", json!({"mfaToken": token3, "code": recovery[1]})).await.0, 200);
    assert_eq!(get(&c3, app.url("/me/mfa")).await["recoveryCodesLeft"], 8);

    // five wrong codes end the sign-in
    let c4 = client();
    let token4 = first_step(&app, &c4, "mia@example.com").await["mfaToken"].as_str().unwrap().to_string();
    for _ in 0..4 {
        assert_eq!(call(&app, &c4, Method::POST, "/auth/mfa", json!({"mfaToken": token4, "code": "999999"})).await.0, 400);
    }
    let (_, body) = call(&app, &c4, Method::POST, "/auth/mfa", json!({"mfaToken": token4, "code": "999998"})).await;
    assert!(body["message"].as_str().unwrap().contains("too many"), "{body}");
    let (_, body) = call(&app, &c4, Method::POST, "/auth/mfa", json!({"mfaToken": token4, "code": recovery[2]})).await;
    assert!(body["message"].as_str().unwrap().contains("expired"), "{body}");

    // new recovery codes need a current code
    assert_eq!(call(&app, &c3, Method::POST, "/me/mfa/recovery-codes", json!({"code": "000000x"})).await.0, 400);
    let (st, fresh) = call(&app, &c3, Method::POST, "/me/mfa/recovery-codes", json!({"code": recovery[3]})).await;
    assert_eq!(st, 200, "{fresh}");
    assert_eq!(get(&c3, app.url("/me/mfa")).await["recoveryCodesLeft"], 10);
    let fresh: Vec<String> = fresh["recoveryCodes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();

    // sessions: this browser first, sign out one, sign out everywhere else
    let list = get(&c3, app.url("/me/sessions")).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 4, "{list:?}");
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
    assert_eq!(list[0]["current"], true);
    let c1_session = get(&c1, app.url("/me/sessions")).await[0]["id"].as_str().unwrap().to_string();
    assert_eq!(del(&app, &c3, &format!("/me/sessions/{c1_session}")).await.0, 204);
    assert_eq!(call(&app, &c1, Method::GET, "/projects", json!(null)).await.0, 401, "signed out");
    assert_eq!(del(&app, &owner, &format!("/me/sessions/{c1_session}")).await.0, 404, "only your own");
    let (_, r) = call(&app, &c3, Method::POST, "/me/sessions/revoke-others", json!({})).await;
    assert_eq!(r["revoked"], 2);
    assert_eq!(call(&app, &mia, Method::GET, "/projects", json!(null)).await.0, 401);
    assert_eq!(get(&c3, app.url("/me/sessions")).await.as_array().unwrap().len(), 1);

    // admins reset a lost second factor: everything goes, the person is signed out
    let (_, tok) = call(&app, &c3, Method::POST, "/me/tokens", json!({"name": "ci"})).await;
    let (admin, _) = app.user(&owner, "Ada Admin", "ada@example.com", "admin").await;
    let (other, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    assert_eq!(call(&app, &other, Method::POST, &format!("/users/{mia_id}/reset-mfa"), json!({})).await.0, 403, "members can't");
    let owner_id = get(&owner, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    assert_eq!(call(&app, &admin, Method::POST, &format!("/users/{owner_id}/reset-mfa"), json!({})).await.0, 403, "admins can't reset the owner");
    assert_eq!(call(&app, &owner, Method::POST, &format!("/users/{owner_id}/reset-mfa"), json!({})).await.0, 400, "your own is in Settings");
    let enrolled = get(&admin, app.url("/admin/security")).await;
    assert_eq!(enrolled["enrolled"], json!([mia_id]));
    assert_eq!(call(&app, &admin, Method::POST, &format!("/users/{mia_id}/reset-mfa"), json!({})).await.0, 204);
    assert_eq!(call(&app, &c3, Method::GET, "/projects", json!(null)).await.0, 401);
    let r = reqwest::Client::new().get(app.url("/projects")).bearer_auth(tok["token"].as_str().unwrap()).send().await.unwrap();
    assert_eq!(r.status(), 401, "API tokens are deleted too");
    assert_eq!(get(&admin, app.url("/admin/security")).await["enrolled"], json!([]));
    let c5 = client();
    let (_, body) = call(&app, &c5, Method::POST, "/auth/login", json!({"email": "mia@example.com", "password": "password123"})).await;
    assert_eq!(body["ok"], true, "{body}");
    let s = get(&c5, app.url("/me/mfa")).await;
    assert_eq!((s["enabled"].clone(), s["recoveryCodesLeft"].clone()), (json!(false), json!(0)), "{s} (fresh codes: {})", fresh.len());

    // on again, and off with the password
    let (secret, _) = enrol(&app, &c5).await;
    assert!(!secret.is_empty());
    assert_eq!(call(&app, &c5, Method::POST, "/me/mfa/disable", json!({})).await.0, 400);
    assert_eq!(call(&app, &c5, Method::POST, "/me/mfa/disable", json!({"password": "wrong-password"})).await.0, 400);
    assert_eq!(call(&app, &c5, Method::POST, "/me/mfa/disable", json!({"password": "password123"})).await.0, 204);
    assert_eq!(get(&c5, app.url("/me/mfa")).await["enabled"], false);
    let c6 = client();
    let (_, body) = call(&app, &c6, Method::POST, "/auth/login", json!({"email": "mia@example.com", "password": "password123"})).await;
    assert_eq!(body["ok"], true, "the password alone signs in again: {body}");
}

#[tokio::test]
async fn codes_cant_be_brute_forced_across_new_sign_ins() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (mo, _) = app.user(&owner, "Mo", "mo@example.com", "member").await;
    let (secret, _) = enrol(&app, &mo).await;
    // a correct password gives a fresh second step each time, but wrong codes add up per person
    for _ in 0..10 {
        let c = client();
        let step = first_step(&app, &c, "mo@example.com").await;
        assert_eq!(call(&app, &c, Method::POST, "/auth/mfa", json!({"mfaToken": step["mfaToken"], "code": "000001"})).await.0, 400);
    }
    let c = client();
    let step = first_step(&app, &c, "mo@example.com").await;
    let (st, body) = call(&app, &c, Method::POST, "/auth/mfa", json!({"mfaToken": step["mfaToken"], "code": next_code(&secret)})).await;
    assert_eq!(st, 400, "locked even with the right code");
    assert!(body["message"].as_str().unwrap().contains("too many"), "{body}");
}

#[tokio::test]
async fn a_reset_link_still_needs_the_second_factor() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (rae, rae_id) = app.user(&owner, "Rae", "rae@example.com", "member").await;
    let (secret, _) = enrol(&app, &rae).await;
    let link = post(&owner, app.url(&format!("/users/{rae_id}/link")), json!({})).await;
    let token = link["url"].as_str().unwrap().split("token=").nth(1).unwrap().to_string();
    let c = client();
    let (st, body) = call(&app, &c, Method::POST, "/auth/link", json!({"token": token, "password": "brand-new-pass"})).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["mfaRequired"], true);
    assert!(get(&c, app.url("/me")).await["user"].is_null(), "no session from the link alone");
    assert_eq!(call(&app, &rae, Method::GET, "/projects", json!(null)).await.0, 401, "old sessions end");
    // the step travels in a cookie (the page continues on /login?mfa=1)
    let (_, methods) = call(&app, &c, Method::POST, "/auth/mfa/passkey", json!({})).await;
    assert_eq!(methods["methods"], json!(["code"]));
    assert_eq!(methods["publicKey"], Value::Null);
    let (st, body) = call(&app, &c, Method::POST, "/auth/mfa", json!({"code": next_code(&secret)})).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(get(&c, app.url("/me")).await["user"]["id"], rae_id.as_str());
    assert_eq!(call(&app, &client(), Method::POST, "/auth/mfa", json!({"code": "123456"})).await.0, 400, "no step without the cookie");
}

#[tokio::test]
async fn changing_sign_in_methods_needs_a_recent_sign_in_and_a_browser() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (eve, eve_id) = app.user(&owner, "Eve", "eve@example.com", "member").await;
    let (_, tok) = call(&app, &eve, Method::POST, "/me/tokens", json!({"name": "script"})).await;
    let token = tok["token"].as_str().unwrap().to_string();
    let bearer = |method: Method, path: &str| {
        let r = reqwest::Client::new().request(method, app.url(path)).bearer_auth(&token).json(&json!({"password": "password123", "requireTwoFactor": "off"}));
        async move { r.send().await.unwrap().status().as_u16() }
    };
    // an API token can't touch sign-in methods at all
    for path in ["/me/mfa/setup", "/me/passkeys/options", "/me/sessions/revoke-others"] {
        assert_eq!(bearer(Method::POST, path).await, 403, "{path}");
    }

    // signed in more than 15 minutes ago: the password confirms
    sqlx::query("UPDATE sessions SET created_at = now() - interval '1 hour' WHERE user_id = $1::uuid").bind(&eve_id).execute(&app.state.db).await.unwrap();
    let (st, err) = call(&app, &eve, Method::POST, "/me/mfa/setup", json!({})).await;
    assert_eq!(st, 403, "{err}");
    assert!(err["message"].as_str().unwrap().contains("password"));
    assert_eq!(call(&app, &eve, Method::POST, "/me/mfa/setup", json!({"password": "wrong-password"})).await.0, 403);
    let (st, setup) = call(&app, &eve, Method::POST, "/me/mfa/setup", json!({"password": "password123"})).await;
    assert_eq!(st, 200);
    let secret = setup["secret"].as_str().unwrap();
    assert_eq!(call(&app, &eve, Method::POST, "/me/mfa/enable", json!({"code": code_at(secret, mfa::current_step())})).await.0, 200);
    // an active app can't be swapped for another with just a session
    let (st, body) = call(&app, &eve, Method::POST, "/me/mfa/setup", json!({"password": "password123"})).await;
    assert_eq!(st, 400, "{body}");

    // admins: the workspace policy and resets take a browser session too
    let (_, admin_tok) = call(&app, &owner, Method::POST, "/me/tokens", json!({"name": "admin script"})).await;
    let admin_tok = admin_tok["token"].as_str().unwrap();
    let r = reqwest::Client::new().put(app.url("/admin/security")).bearer_auth(admin_tok).json(&json!({"requireTwoFactor": "off"})).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = reqwest::Client::new().post(app.url(&format!("/users/{eve_id}/reset-mfa"))).bearer_auth(admin_tok).json(&json!({})).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = reqwest::Client::new().get(app.url("/admin/security")).bearer_auth(admin_tok).send().await.unwrap();
    assert_eq!(r.status(), 200, "reading is fine");
}

/// A workspace can require two-factor authentication: until it's set up, a session can only
/// set it up (or sign out); API tokens are separate credentials.
#[tokio::test]
async fn required_two_factor() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (member, _) = app.user(&owner, "Mem", "mem@example.com", "member").await;
    let (_, t) = call(&app, &member, Method::POST, "/me/tokens", json!({"name": "ci"})).await;
    let token = t["token"].as_str().unwrap().to_string();
    assert_eq!(get(&owner, app.url("/admin/security")).await["requireTwoFactor"], "off");
    assert_eq!(call(&app, &owner, Method::PUT, "/admin/security", json!({"requireTwoFactor": "sometimes"})).await.0, 400);
    assert_eq!(call(&app, &member, Method::PUT, "/admin/security", json!({"requireTwoFactor": "off"})).await.0, 403);
    let (st, s) = call(&app, &owner, Method::PUT, "/admin/security", json!({"requireTwoFactor": "admins"})).await;
    assert_eq!((st, &s["requireTwoFactor"]), (200, &json!("admins")), "{s}");

    // admins: only the setup until they have a second factor; members aren't affected yet
    let (st, err) = call(&app, &owner, Method::GET, "/projects", json!(null)).await;
    assert_eq!(st, 403);
    assert!(err["message"].as_str().unwrap().contains("two-factor"));
    assert_eq!(call(&app, &owner, Method::PUT, "/admin/security", json!({"requireTwoFactor": "off"})).await.0, 403, "not even switch it off");
    let me = get(&owner, app.url("/me")).await;
    assert_eq!(me["mustSetUpTwoFactor"], true);
    assert_eq!(me["user"]["name"], "Olivia Owner");
    assert_eq!(get(&owner, app.url("/me/mfa")).await["required"], true);
    assert_eq!(get(&member, app.url("/me")).await["mustSetUpTwoFactor"], false);
    assert_eq!(call(&app, &member, Method::GET, "/projects", json!(null)).await.0, 200);
    enrol(&app, &owner).await;
    assert_eq!(call(&app, &owner, Method::GET, "/projects", json!(null)).await.0, 200);
    assert_eq!(get(&owner, app.url("/me")).await["mustSetUpTwoFactor"], false);
    // the last second factor can't be removed while it's required
    let (st, err) = call(&app, &owner, Method::POST, "/me/mfa/disable", json!({"password": "password123"})).await;
    assert_eq!(st, 400);
    assert!(err["message"].as_str().unwrap().contains("required"), "{err}");

    // everyone
    assert_eq!(call(&app, &owner, Method::PUT, "/admin/security", json!({"requireTwoFactor": "everyone"})).await.0, 200);
    assert_eq!(call(&app, &member, Method::GET, "/projects", json!(null)).await.0, 403);
    assert_eq!(call(&app, &member, Method::POST, "/projects", json!({"key": "X", "name": "y"})).await.0, 403);
    assert_eq!(call(&app, &member, Method::GET, "/me/sessions", json!(null)).await.0, 200);
    let r = reqwest::Client::new().get(app.url("/projects")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(r.status(), 200, "API tokens are separate credentials");
    // a fresh sign-in of someone without a second factor lands on the setup
    let c = client();
    let (_, body) = call(&app, &c, Method::POST, "/auth/login", json!({"email": "mem@example.com", "password": "password123"})).await;
    assert_eq!(body["ok"], true);
    assert_eq!(get(&c, app.url("/me")).await["mustSetUpTwoFactor"], true);
    enrol(&app, &member).await;
    assert_eq!(call(&app, &member, Method::GET, "/projects", json!(null)).await.0, 200);
    assert_eq!(call(&app, &c, Method::GET, "/projects", json!(null)).await.0, 200, "the other session too");
    let policy = get(&owner, app.url("/admin/security")).await;
    assert_eq!(policy["enrolled"].as_array().unwrap().len(), 2);
    assert_eq!((policy["people"].clone(), policy["admins"].clone()), (json!({"total": 2, "enrolled": 2}), json!({"total": 1, "enrolled": 1})));

    // an admin reset puts the person back on the setup
    let member_id = get(&member, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    assert_eq!(call(&app, &owner, Method::POST, &format!("/users/{member_id}/reset-mfa"), json!({})).await.0, 204);
    let c = client();
    call(&app, &c, Method::POST, "/auth/login", json!({"email": "mem@example.com", "password": "password123"})).await;
    assert_eq!(get(&c, app.url("/me")).await["mustSetUpTwoFactor"], true);
    assert_eq!(call(&app, &owner, Method::PUT, "/admin/security", json!({"requireTwoFactor": "off"})).await.0, 200);
    assert_eq!(get(&c, app.url("/me")).await["mustSetUpTwoFactor"], false);
}

/// Registers a passkey for the signed-in client with a software authenticator.
async fn add_passkey(app: &TestApp, c: &reqwest::Client, ed25519: bool, password: Option<&str>) -> Authenticator {
    let rp = RelyingParty::from_base_url(&app.base).unwrap();
    let (st, opts) = call(app, c, Method::POST, "/me/passkeys/options", json!({"password": password})).await;
    assert_eq!(st, 200, "{opts}");
    let auth = Authenticator::new(rp, ed25519);
    let challenge = unb64(opts["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    let (client_data, attestation) = auth.register(&challenge);
    let (st, row) = call(
        app,
        c,
        Method::POST,
        "/me/passkeys",
        json!({"token": opts["token"], "name": "Laptop", "credential": {
            "id": b64(&auth.credential_id), "rawId": b64(&auth.credential_id), "type": "public-key",
            "response": {"clientDataJSON": b64(&client_data), "attestationObject": b64(&attestation), "transports": ["internal"]}
        }}),
    )
    .await;
    assert_eq!(st, 201, "{row}");
    auth
}

/// The authenticator's answer to `navigator.credentials.get()` options.
fn assertion(auth: &mut Authenticator, options: &Value, user: Option<&str>) -> Value {
    let challenge = unb64(options["challenge"].as_str().unwrap()).unwrap();
    let (client_data, data, sig) = auth.sign(&challenge);
    json!({"id": b64(&auth.credential_id), "rawId": b64(&auth.credential_id), "type": "public-key", "response": {
        "clientDataJSON": b64(&client_data), "authenticatorData": b64(&data), "signature": b64(&sig), "userHandle": user
    }})
}

#[tokio::test]
async fn passkeys_as_second_step_and_without_a_password() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (mia, mia_id) = app.user(&owner, "Mia", "mia@example.com", "member").await;
    let handle = b64(mia_id.parse::<uuid::Uuid>().unwrap().as_bytes());

    // the options name this TaskCraft and the person (their id as the user handle)
    let (_, opts) = call(&app, &mia, Method::POST, "/me/passkeys/options", json!({})).await;
    assert_eq!(opts["publicKey"]["rp"]["id"], "127.0.0.1");
    assert_eq!(opts["publicKey"]["user"]["id"], handle.as_str());
    let mut key = add_passkey(&app, &mia, false, None).await;
    let list = get(&mia, app.url("/me/passkeys")).await;
    assert_eq!(list[0]["name"], "Laptop");
    assert_eq!(get(&mia, app.url("/me/mfa")).await["passkeys"], 1);
    // the same authenticator can't be registered twice; the options leave it out
    let (_, opts) = call(&app, &mia, Method::POST, "/me/passkeys/options", json!({})).await;
    assert_eq!(opts["publicKey"]["excludeCredentials"][0]["id"], b64(&key.credential_id).as_str());
    // a setup token works once, and only for its person
    let (other, _) = app.user(&owner, "Ola", "ola@example.com", "member").await;
    let (st, _) = call(
        &app,
        &other,
        Method::POST,
        "/me/passkeys",
        json!({"token": opts["token"], "credential": {"rawId": "", "response": {"clientDataJSON": "", "attestationObject": ""}}}),
    )
    .await;
    assert_eq!(st, 400);

    // a sign-in older than 15 minutes needs the password to add one
    sqlx::query("UPDATE sessions SET created_at = now() - interval '1 hour' WHERE user_id = $1::uuid").bind(&mia_id).execute(&app.state.db).await.unwrap();
    assert_eq!(call(&app, &mia, Method::POST, "/me/passkeys/options", json!({})).await.0, 403);
    assert_eq!(call(&app, &mia, Method::POST, "/me/passkeys/options", json!({"password": "wrong-password"})).await.0, 403);
    let mut phone = add_passkey(&app, &mia, true, Some("password123")).await;

    // the password is no longer enough: the second step takes a passkey
    let c = client();
    let login = first_step(&app, &c, "mia@example.com").await;
    assert_eq!(login["methods"], json!(["passkey"]));
    let token = login["mfaToken"].as_str().unwrap();
    let (st, _) =
        call(&app, &c, Method::POST, "/auth/mfa", json!({"mfaToken": token, "passkey": assertion(&mut key, &json!({"challenge": b64(&[0; 32])}), None)})).await;
    assert_eq!(st, 400, "the passkey step must be started");
    let (_, step) = call(&app, &c, Method::POST, "/auth/mfa/passkey", json!({"mfaToken": token})).await;
    assert_eq!(step["methods"], json!(["passkey"]));
    assert_eq!(step["publicKey"]["allowCredentials"].as_array().unwrap().len(), 2);
    let answer = assertion(&mut phone, &step["publicKey"], Some(&handle));
    let (st, done) = call(&app, &c, Method::POST, "/auth/mfa", json!({"mfaToken": token, "passkey": answer})).await;
    assert_eq!(st, 200, "{done}");
    assert_eq!(get(&c, app.url("/me")).await["user"]["id"], mia_id.as_str());
    // the same answer again (a replay) is refused
    let c2 = client();
    let token = first_step(&app, &c2, "mia@example.com").await["mfaToken"].as_str().unwrap().to_string();
    call(&app, &c2, Method::POST, "/auth/mfa/passkey", json!({"mfaToken": token})).await;
    assert_eq!(call(&app, &c2, Method::POST, "/auth/mfa", json!({"mfaToken": token, "passkey": answer})).await.0, 400);
    // someone else's passkey doesn't do
    let mut olas = add_passkey(&app, &other, false, None).await;
    let (_, step) = call(&app, &c2, Method::POST, "/auth/mfa/passkey", json!({"mfaToken": token})).await;
    assert_eq!(call(&app, &c2, Method::POST, "/auth/mfa", json!({"mfaToken": token, "passkey": assertion(&mut olas, &step["publicKey"], None)})).await.0, 400);
    assert!(get(&c2, app.url("/me")).await["user"].is_null());

    // without a password: the authenticator verified the person
    let c3 = client();
    let (_, opts) = call(&app, &c3, Method::POST, "/auth/passkey/options", json!({})).await;
    assert_eq!(opts["publicKey"]["userVerification"], "required");
    let (st, r) =
        call(&app, &c3, Method::POST, "/auth/passkey", json!({"token": opts["token"], "credential": assertion(&mut key, &opts["publicKey"], Some(&handle))}))
            .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(get(&c3, app.url("/me")).await["user"]["id"], mia_id.as_str());
    assert_eq!(
        call(&app, &c3, Method::POST, "/auth/passkey", json!({"token": opts["token"], "credential": assertion(&mut key, &opts["publicKey"], None)})).await.0,
        400,
        "a sign-in token works once"
    );
    // … not when it only saw someone touch it
    key.user_verified = false;
    let c4 = client();
    let (_, opts) = call(&app, &c4, Method::POST, "/auth/passkey/options", json!({})).await;
    let (st, r) =
        call(&app, &c4, Method::POST, "/auth/passkey", json!({"token": opts["token"], "credential": assertion(&mut key, &opts["publicKey"], None)})).await;
    assert_eq!(st, 400, "{r}");
    assert!(get(&c4, app.url("/me")).await["user"].is_null());

    // a passkey counts as two-factor when the workspace requires it
    let (newbie, _) = app.user(&owner, "Neo", "neo@example.com", "member").await;
    assert_eq!(call(&app, &owner, Method::PUT, "/admin/security", json!({"requireTwoFactor": "everyone"})).await.0, 200);
    assert_eq!(get(&newbie, app.url("/me")).await["mustSetUpTwoFactor"], true);
    assert_eq!(call(&app, &newbie, Method::GET, "/projects", json!(null)).await.0, 403);
    let newbie_key = add_passkey(&app, &newbie, false, None).await;
    assert!(!newbie_key.credential_id.is_empty());
    assert_eq!(get(&newbie, app.url("/me")).await["mustSetUpTwoFactor"], false);
    assert_eq!(call(&app, &newbie, Method::GET, "/projects", json!(null)).await.0, 200);
    // … and the last one can't be removed while it's required
    let theirs = get(&newbie, app.url("/me/passkeys")).await[0]["id"].as_str().unwrap().to_string();
    let (st, err) = del(&app, &newbie, &format!("/me/passkeys/{theirs}")).await;
    assert_eq!(st, 400, "{err}");
    // (the owner sets up a second factor of their own before changing the policy back)
    let owner_c = client();
    let (_, body) = call(&app, &owner_c, Method::POST, "/auth/login", json!({"email": "owner@example.com", "password": "password123"})).await;
    assert_eq!(body["ok"], true);
    enrol(&app, &owner_c).await;
    assert_eq!(call(&app, &owner_c, Method::PUT, "/admin/security", json!({"requireTwoFactor": "off"})).await.0, 200);

    // rename, remove (a recent sign-in or the password), an admin reset removes them all
    let list = get(&c3, app.url("/me/passkeys")).await;
    let first = list[0]["id"].as_str().unwrap().to_string();
    assert_eq!(call(&app, &c3, Method::PATCH, &format!("/me/passkeys/{first}"), json!({"name": "Work laptop"})).await.1["name"], "Work laptop");
    assert_eq!(call(&app, &c3, Method::PATCH, &format!("/me/passkeys/{first}"), json!({"name": " "})).await.0, 400);
    assert_eq!(del(&app, &other, &format!("/me/passkeys/{first}")).await.0, 404, "not theirs");
    assert_eq!(del(&app, &mia, &format!("/me/passkeys/{first}")).await.0, 403, "signed in an hour ago");
    assert_eq!(call(&app, &mia, Method::DELETE, &format!("/me/passkeys/{first}"), json!({"password": "password123"})).await.0, 204);
    assert_eq!(get(&c3, app.url("/me/passkeys")).await.as_array().unwrap().len(), 1);
    assert_eq!(call(&app, &owner_c, Method::POST, &format!("/users/{mia_id}/reset-mfa"), json!({})).await.0, 204);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE user_id = $1::uuid").bind(&mia_id).fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 0);
    let c5 = client();
    let (_, opts) = call(&app, &c5, Method::POST, "/auth/passkey/options", json!({})).await;
    phone.user_verified = true;
    assert_eq!(
        call(&app, &c5, Method::POST, "/auth/passkey", json!({"token": opts["token"], "credential": assertion(&mut phone, &opts["publicKey"], None)})).await.0,
        400,
        "removed passkeys don't sign in"
    );
}
