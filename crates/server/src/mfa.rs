//! Two-factor authentication.
//!
//! * Time-based one-time passwords (RFC 6238: HMAC-SHA1, 30-second steps, 6 digits) from an
//!   authenticator app, and one-time recovery codes.
//! * The second step of a sign-in: after a correct password the person gets a short-lived token
//!   (`auth_challenges`), not a session, and completes it with a code or a passkey
//!   (`routes::security::login`).
//! * The workspace policy `requireTwoFactor` (`off` | `admins` | `everyone`): people it applies to
//!   who have no second factor yet may only set one up ([`MustEnrollTwoFactor`]). API tokens are
//!   separate credentials and keep working.
//! * [`confirm_recent`]: changes to sign-in methods need a recent sign-in or the password.
//!
//! Passkeys: `crate::webauthn` and `routes::passkeys`; the HTTP side: `routes::security`.

use axum::http::{HeaderMap, Method};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{User, clear_login_failures, login_allowed, random_token, record_login_failure, session_token, token_hash, verify_password};
use crate::error::ApiError;

// ---- TOTP ------------------------------------------------------------------------------------

const STEP: u64 = 30;
const DIGITS: u32 = 6;
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let v = B32.iter().position(|&x| x as char == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// A new 160-bit secret, base32-encoded.
pub fn new_secret() -> String {
    let mut b = [0u8; 20];
    rand::fill(&mut b);
    base32(&b)
}

/// The code for a given 30-second step.
pub fn code_at(secret: &[u8], step: u64) -> String {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(secret).expect("any key length");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    format!("{:0width$}", bin % 10u32.pow(DIGITS), width = DIGITS as usize)
}

pub fn current_step() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / STEP).unwrap_or(0)
}

/// The matching step within ±1 step of now (clock drift), if any.
pub fn matching_step(secret_b32: &str, code: &str) -> Option<u64> {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != DIGITS as usize {
        return None;
    }
    let secret = base32_decode(secret_b32)?;
    let now = current_step();
    [now.saturating_sub(1), now, now + 1].into_iter().find(|&s| constant_eq(code_at(&secret, s).as_bytes(), code.as_bytes()))
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn otpauth_uri(account: &str, secret: &str) -> String {
    let enc = |s: &str| {
        s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.@".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>()
    };
    format!("otpauth://totp/TaskCraft:{}?secret={secret}&issuer=TaskCraft&algorithm=SHA1&digits={DIGITS}&period={STEP}", enc(account))
}

pub fn qr_svg(data: &str) -> anyhow::Result<String> {
    let code = qrcode::QrCode::new(data.as_bytes())?;
    Ok(code.render::<qrcode::render::svg::Color>().min_dimensions(200, 200).quiet_zone(true).build())
}

// ---- recovery codes --------------------------------------------------------------------------

fn hash_code(code: &str) -> String {
    let normalized: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    hex::encode(Sha256::digest(normalized.as_bytes()))
}

/// Ten fresh recovery codes (shown once) and their hashes (stored).
pub fn recovery_codes() -> (Vec<String>, Vec<String>) {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let codes: Vec<String> = (0..10)
        .map(|_| {
            let mut b = [0u8; 12];
            rand::fill(&mut b);
            let s: String = b.iter().map(|x| ALPHABET[*x as usize % ALPHABET.len()] as char).collect();
            format!("{}-{}-{}", &s[0..4], &s[4..8], &s[8..12])
        })
        .collect();
    let hashes = codes.iter().map(|c| hash_code(c)).collect();
    (codes, hashes)
}

/// Error for a locked second factor.
#[derive(Debug, thiserror::Error)]
#[error("too many wrong codes; try again in 15 minutes")]
pub struct TooManyAttempts;

/// Checks a TOTP code (once per step) or consumes a recovery code. `true` = second factor ok.
/// Wrong codes count against a per-person limit (10 per 15 minutes) that a correct password
/// doesn't reset, so codes can't be brute-forced by starting new sign-ins.
pub async fn verify(state: &AppState, user: Uuid, input: &str) -> Result<bool, ApiError> {
    let key = format!("mfa:{user}");
    if !login_allowed(state, &key).await {
        return Err(ApiError::bad(TooManyAttempts.to_string()));
    }
    let ok = check(state, user, input).await?;
    if ok {
        clear_login_failures(state, &key).await;
    } else {
        record_login_failure(state, &key).await;
    }
    Ok(ok)
}

async fn check(state: &AppState, user: Uuid, input: &str) -> Result<bool, ApiError> {
    let secret: Option<Option<String>> =
        sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = $1 AND totp_enabled_at IS NOT NULL").bind(user).fetch_optional(&state.db).await?;
    let Some(Some(secret)) = secret else { return Ok(false) };
    if let Some(step) = matching_step(&secret, input) {
        // atomically refuse a code whose step was already used
        let n = sqlx::query("UPDATE users SET totp_last_step = $2 WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)")
            .bind(user)
            .bind(step as i64)
            .execute(&state.db)
            .await?
            .rows_affected();
        return Ok(n == 1);
    }
    let n = sqlx::query("UPDATE users SET recovery_codes = array_remove(recovery_codes, $2) WHERE id = $1 AND $2 = ANY(recovery_codes)")
        .bind(user)
        .bind(hash_code(input))
        .execute(&state.db)
        .await?
        .rows_affected();
    Ok(n == 1)
}

// ---- which factors a person has; the workspace policy ----------------------------------------

/// The second factors a person can use: `code` (authenticator app or recovery code), `passkey`.
pub async fn methods(state: &AppState, user: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (totp, passkey): (bool, bool) =
        sqlx::query_as("SELECT totp_enabled_at IS NOT NULL, EXISTS (SELECT 1 FROM passkeys WHERE user_id = $1) FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&state.db)
            .await?;
    Ok([("code", totp), ("passkey", passkey)].into_iter().filter(|(_, on)| *on).map(|(m, _)| m).collect())
}

/// The workspace setting `requireTwoFactor`: `off`, `admins` or `everyone`.
pub const POLICIES: [&str; 3] = ["off", "admins", "everyone"];

/// SQL over `users u` and `workspaces w`: whether the workspace requires this person to have a
/// second factor.
pub const POLICY_APPLIES_SQL: &str = "(COALESCE(w.settings->>'requireTwoFactor', 'off') = 'everyone'
     OR (COALESCE(w.settings->>'requireTwoFactor', 'off') = 'admins' AND u.role IN ('owner', 'admin')))";

/// Whether the workspace requires `user` to have a second factor (then they can't remove their last one).
pub async fn policy_applies(state: &AppState, user: Uuid) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {POLICY_APPLIES_SQL} FROM users u JOIN workspaces w ON w.id = u.workspace_id WHERE u.id = $1")))
        .bind(user)
        .fetch_one(&state.db)
        .await?)
}

/// Set on requests of people who must set up two-factor authentication before anything else
/// (the workspace requires it and they have neither an authenticator app nor a passkey).
#[derive(Debug, Clone, Copy)]
pub struct MustEnrollTwoFactor;

/// What such a session may still do: see who it is, set up a second factor, sign out.
pub fn allowed_before_enrollment(method: &Method, path: &str) -> bool {
    let p = path.trim_end_matches('/');
    let p = p.strip_prefix("/api/v1").or_else(|| p.strip_prefix("/api")).unwrap_or(p);
    matches!(p, "/me" | "/auth/logout" | "/me/mfa" | "/me/mfa/setup" | "/me/mfa/enable" | "/me/passkeys" | "/me/passkeys/options")
        || (method == Method::GET && matches!(p, "/setup" | "/me/sessions"))
}

/// The refusal for everything else.
pub fn enrollment_required() -> ApiError {
    ApiError::Denied("two-factor authentication is required in this workspace: set it up first".into())
}

// ---- confirming changes to sign-in methods ---------------------------------------------------

/// How long after signing in changes to sign-in methods need no password.
pub const RECENT_SIGN_IN_MINUTES: i32 = 15;

/// For changes to sign-in methods: the request's session must belong to `user` and be younger
/// than [`RECENT_SIGN_IN_MINUTES`], or `password` must be theirs (wrong passwords count against
/// a per-person limit). A stolen session alone can't swap in another authenticator or take one
/// away.
pub async fn confirm_recent(state: &AppState, user: &User, headers: &HeaderMap, password: Option<&str>) -> Result<(), ApiError> {
    if let Some(t) = session_token(headers) {
        let recent: Option<bool> = sqlx::query_scalar(
            "SELECT created_at > now() - make_interval(mins => $3) FROM sessions WHERE token_hash = $1 AND user_id = $2 AND expires_at > now()",
        )
        .bind(token_hash(t))
        .bind(user.id)
        .bind(RECENT_SIGN_IN_MINUTES)
        .fetch_optional(&state.db)
        .await?;
        if recent == Some(true) {
            return Ok(());
        }
    }
    let Some(password) = password.filter(|p| !p.is_empty()) else {
        return Err(ApiError::Denied("confirm with your password (or sign in again)".into()));
    };
    if check_password(state, user.id, password).await? { Ok(()) } else { Err(ApiError::Denied("that password isn't right".into())) }
}

/// Whether `password` is the person's (wrong ones count against a per-person limit).
pub async fn check_password(state: &AppState, user: Uuid, password: &str) -> Result<bool, ApiError> {
    let key = format!("confirm:{user}");
    if !login_allowed(state, &key).await {
        return Err(ApiError::bad("too many wrong passwords, try again in 15 minutes"));
    }
    let hash: Option<String> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1").bind(user).fetch_one(&state.db).await?;
    let ok = match hash {
        Some(h) => verify_password(password.to_string(), h).await,
        None => false,
    };
    if ok {
        clear_login_failures(state, &key).await;
    } else {
        record_login_failure(state, &key).await;
    }
    Ok(ok)
}

// ---- sign-in steps in progress ---------------------------------------------------------------

/// Kinds of [`put_step`] rows.
pub const SECOND_STEP: &str = "second_step";
pub const PASSKEY_ADD: &str = "passkey_add";
pub const PASSKEY_LOGIN: &str = "passkey_login";

/// The cookie that carries a second step for sign-ins that end in a redirect (single sign-on) or
/// a password reset; password logins get the token in the response instead.
pub const MFA_COOKIE: &str = "tc_mfa";

/// Stores a sign-in step (ten minutes); returns its opaque token.
pub async fn put_step(state: &AppState, kind: &str, user: Option<Uuid>, challenge: Option<&[u8]>, next: Option<&str>) -> Result<String, ApiError> {
    let token = random_token();
    sqlx::query("DELETE FROM auth_challenges WHERE expires_at < now()").execute(&state.db).await?;
    sqlx::query(
        "INSERT INTO auth_challenges (token_hash, kind, user_id, challenge, next, expires_at) VALUES ($1, $2, $3, $4, $5, now() + interval '10 minutes')",
    )
    .bind(token_hash(&token))
    .bind(kind)
    .bind(user)
    .bind(challenge)
    .bind(next)
    .execute(&state.db)
    .await?;
    Ok(token)
}

/// Consumes a sign-in step (one-time): `(user, challenge)`.
pub async fn take_step(state: &AppState, kind: &str, token: &str) -> Result<(Option<Uuid>, Option<Vec<u8>>), ApiError> {
    let row: Option<(Option<Uuid>, Option<Vec<u8>>)> =
        sqlx::query_as("DELETE FROM auth_challenges WHERE token_hash = $1 AND kind = $2 AND expires_at > now() RETURNING user_id, challenge")
            .bind(token_hash(token))
            .bind(kind)
            .fetch_optional(&state.db)
            .await?;
    row.ok_or_else(|| ApiError::bad("this step expired, please try again"))
}

/// After a correct password: `None` when the person has no second factor (sign them in), else
/// the answer that starts the second step, `{mfaRequired: true, mfaToken, methods}`.
pub async fn second_step(state: &AppState, user: Uuid) -> Result<Option<Value>, ApiError> {
    let methods = methods(state, user).await?;
    if methods.is_empty() {
        return Ok(None);
    }
    let token = put_step(state, SECOND_STEP, Some(user), None, None).await?;
    Ok(Some(json!({ "mfaRequired": true, "mfaToken": token, "methods": methods })))
}

/// Like [`second_step`] for sign-ins that continue on `/login?mfa=1`: the `Set-Cookie` value that
/// carries the step (`None`: no second factor). `next`: where to go afterwards.
pub async fn second_step_cookie(state: &AppState, user: Uuid, next: Option<&str>) -> Result<Option<String>, ApiError> {
    if methods(state, user).await?.is_empty() {
        return Ok(None);
    }
    let token = put_step(state, SECOND_STEP, Some(user), None, next).await?;
    Ok(Some(mfa_cookie(state, &token, 600)))
}

/// For sign-ins that end in a redirect (single sign-on): `(Set-Cookie, location)` for a session —
/// or, when the person has a second factor, for the second step on `/login?mfa=1`.
pub async fn session_or_second_step(state: &AppState, user: Uuid, headers: &HeaderMap, next: &str) -> Result<(String, String), ApiError> {
    if let Some(cookie) = second_step_cookie(state, user, Some(next)).await? {
        return Ok((cookie, format!("/login?mfa=1&next={}", urlencode(next))));
    }
    Ok((crate::auth::create_session(state, user, headers).await?, next.to_string()))
}

pub fn mfa_cookie(state: &AppState, token: &str, max_age: i64) -> String {
    format!("{MFA_COOKIE}={token}; Path=/api; HttpOnly; SameSite=Lax; Max-Age={max_age}{}", if state.config.secure_cookies { "; Secure" } else { "" })
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vectors() {
        // RFC 6238 appendix B, SHA1 secret "12345678901234567890" (8 digits there; the last 6 here)
        let secret = b"12345678901234567890";
        assert_eq!(code_at(secret, 59 / 30), "287082");
        assert_eq!(code_at(secret, 1111111109 / 30), "081804");
        assert_eq!(code_at(secret, 1111111111 / 30), "050471");
        assert_eq!(code_at(secret, 1234567890 / 30), "005924");
        assert_eq!(code_at(secret, 2000000000 / 30), "279037");
        assert_eq!(code_at(secret, 20000000000 / 30), "353130");
    }

    #[test]
    fn base32_roundtrip() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(base32_decode("mzxw 6ytb oi==").unwrap(), b"foobar", "spaces, case and padding don't matter");
        assert!(base32_decode("MZ1W").is_none(), "1 isn't base32");
        let s = new_secret();
        assert_eq!(s.len(), 32);
        assert_eq!(base32_decode(&s).unwrap().len(), 20);
    }

    #[test]
    fn current_code_matches() {
        let s = new_secret();
        let secret = base32_decode(&s).unwrap();
        let code = code_at(&secret, current_step());
        assert!(matching_step(&s, &code).is_some());
        assert!(matching_step(&s, &format!("{} {}", &code[..3], &code[3..])).is_some(), "spaces don't matter");
        assert!(matching_step(&s, &code_at(&secret, current_step() + 1)).is_some(), "one step of clock drift");
        assert!(matching_step(&s, "12345").is_none());
        let (codes, hashes) = recovery_codes();
        assert_eq!(codes.len(), 10);
        assert_eq!(hash_code(&codes[0].to_uppercase().replace('-', "")), hashes[0], "case and dashes don't matter");
    }

    #[test]
    fn otpauth_uri_and_qr() {
        let uri = otpauth_uri("mia ramos@example.com", "ABC");
        assert_eq!(uri, "otpauth://totp/TaskCraft:mia%20ramos@example.com?secret=ABC&issuer=TaskCraft&algorithm=SHA1&digits=6&period=30");
        assert!(qr_svg(&uri).unwrap().contains("<svg"));
    }

    #[test]
    fn enrollment_allows_only_setting_up() {
        for p in ["/me", "/api/me", "/api/v1/me/mfa/setup", "/me/passkeys/options", "/auth/logout", "/me/mfa/enable/"] {
            assert!(allowed_before_enrollment(&Method::POST, p), "{p}");
        }
        assert!(allowed_before_enrollment(&Method::GET, "/me/sessions"));
        for p in ["/projects", "/me/tokens", "/me/mfa/disable", "/admin/security", "/events"] {
            assert!(!allowed_before_enrollment(&Method::GET, p), "{p}");
        }
        assert!(!allowed_before_enrollment(&Method::DELETE, "/me/sessions"));
    }
}
