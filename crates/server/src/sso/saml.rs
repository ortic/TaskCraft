//! SAML 2.0 service provider (cargo feature `saml`): SP-initiated sign-in with an HTTP-Redirect
//! `AuthnRequest` and an HTTP-POST response to the assertion consumer service.
//!
//! Signatures are verified with libxmlsec1 (via `samael`). The IdP metadata must contain a
//! signing certificate — unsigned responses are never accepted — and the response must answer
//! a request we issued (`InResponseTo`, single use); IdP-initiated sign-ins are refused.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use samael::crypto::AllowedSignatureAlgorithm;
use samael::metadata::{EntityDescriptor, HTTP_REDIRECT_BINDING};
use samael::schema::Assertion;
use samael::service_provider::{ServiceProvider, ServiceProviderBuilder};
use serde::{Deserialize, Serialize};

use super::{External, Provider, SamlConfig, SignInError};
use crate::app::AppState;

/// IdP metadata fetched from `idpMetadataUrl`, refreshed daily (IdPs rotate certificates).
static METADATA: LazyLock<RwLock<HashMap<String, (Instant, EntityDescriptor)>>> = LazyLock::new(Default::default);
const METADATA_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Serialize, Deserialize)]
struct Pending {
    provider: String,
    request_id: String,
    next: String,
}

async fn idp_metadata(c: &SamlConfig) -> anyhow::Result<EntityDescriptor> {
    if let Some(xml) = &c.idp_metadata_xml {
        return xml.parse::<EntityDescriptor>().map_err(|e| anyhow::anyhow!("invalid IdP metadata: {e}"));
    }
    let url = c.idp_metadata_url.as_deref().ok_or_else(|| anyhow::anyhow!("SAML provider {:?} needs idpMetadataUrl or idpMetadataXml", c.common.id))?;
    if let Some((at, md)) = METADATA.read().expect("metadata cache").get(url)
        && at.elapsed() < METADATA_TTL
    {
        return Ok(md.clone());
    }
    // no redirects (SSRF), bounded time
    let http = openidconnect::reqwest::Client::builder().redirect(openidconnect::reqwest::redirect::Policy::none()).timeout(Duration::from_secs(15)).build()?;
    let xml = http.get(url).send().await?.error_for_status()?.text().await?;
    let md = xml.parse::<EntityDescriptor>().map_err(|e| anyhow::anyhow!("invalid IdP metadata from {url}: {e}"))?;
    METADATA.write().expect("metadata cache").insert(url.to_string(), (Instant::now(), md.clone()));
    Ok(md)
}

/// SP entity id: `{BASE_URL}/api/auth/sso/{id}/metadata` unless configured.
pub fn entity_id(state: &AppState, c: &SamlConfig) -> String {
    c.entity_id.clone().unwrap_or_else(|| format!("{}/api/auth/sso/{}/metadata", state.config.base(), c.common.id))
}

/// Assertion consumer service: `{BASE_URL}/api/auth/sso/{id}/acs`.
pub fn acs_url(state: &AppState, c: &SamlConfig) -> String {
    format!("{}/api/auth/sso/{}/acs", state.config.base(), c.common.id)
}

fn service_provider(state: &AppState, c: &SamlConfig, md: EntityDescriptor) -> anyhow::Result<ServiceProvider> {
    use AllowedSignatureAlgorithm::*;
    let sp = ServiceProviderBuilder::default()
        .entity_id(entity_id(state, c))
        .metadata_url(entity_id(state, c))
        .acs_url(acs_url(state, c))
        .slo_url(None::<String>)
        .idp_metadata(md)
        .allow_idp_initiated(false)
        // no SHA-1, no algorithm substitution
        .allowed_signature_algorithms(vec![RsaSha256, RsaSha384, RsaSha512, EcdsaSha256, EcdsaSha384, EcdsaSha512])
        .build()?;
    let certs = sp.idp_signing_certs().map_err(|e| anyhow::anyhow!("IdP metadata: {e}"))?;
    anyhow::ensure!(certs.is_some_and(|c| !c.is_empty()), "the IdP metadata has no signing certificate");
    Ok(sp)
}

/// Starts the sign-in: redirect to the IdP with an `AuthnRequest`; RelayState = our state key.
pub async fn login(state: &AppState, c: &SamlConfig, next: String, browser: Option<&str>) -> anyhow::Result<Response> {
    let sp = service_provider(state, c, idp_metadata(c).await?)?;
    let sso = sp.sso_binding_location(HTTP_REDIRECT_BINDING).ok_or_else(|| anyhow::anyhow!("the IdP has no HTTP-Redirect sign-in endpoint"))?;
    let request = sp.make_authentication_request(&sso).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pending = Pending { provider: c.common.id.clone(), request_id: request.id.clone(), next };
    let key = super::put_state(state, &serde_json::to_value(&pending)?, browser).await?;
    let url = request.redirect(&key).map_err(|e| anyhow::anyhow!("{e}"))?.ok_or_else(|| anyhow::anyhow!("could not build the IdP redirect"))?;
    Ok(Redirect::to(url.as_str()).into_response())
}

#[derive(Deserialize)]
pub struct AcsForm {
    #[serde(rename = "SAMLResponse")]
    saml_response: String,
    #[serde(rename = "RelayState")]
    relay_state: Option<String>,
}

/// `POST /api/auth/sso/{id}/acs` — the IdP posts the signed response here.
pub async fn acs(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap, Form(form): Form<AcsForm>) -> Response {
    let Some(p) = super::find(&state, &id).cloned() else { return super::login_error("unknown sign-in provider") };
    let Provider::Saml(c) = &p else { return super::login_error("not a SAML provider") };
    super::respond(&id, complete(&state, &p, c, &headers, form).await)
}

/// `GET /api/auth/sso/{id}/metadata` — SP metadata to register TaskCraft at the IdP.
pub async fn metadata(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(Provider::Saml(c)) = super::find(&state, &id).cloned() else { return StatusCode::NOT_FOUND.into_response() };
    ([(header::CONTENT_TYPE, "application/samlmetadata+xml")], sp_metadata(&entity_id(&state, &c), &acs_url(&state, &c))).into_response()
}

fn sp_metadata(entity_id: &str, acs: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="{}">
  <md:SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">
    <md:NameIDFormat>urn:oasis:names:tc:SAML:2.0:nameid-format:persistent</md:NameIDFormat>
    <md:NameIDFormat>urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress</md:NameIDFormat>
    <md:AssertionConsumerService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="{}" index="0" isDefault="true"/>
  </md:SPSSODescriptor>
</md:EntityDescriptor>
"#,
        esc(entity_id),
        esc(acs)
    )
}

async fn complete(state: &AppState, provider: &Provider, c: &SamlConfig, headers: &HeaderMap, form: AcsForm) -> Result<Response, SignInError> {
    let key = form
        .relay_state
        .filter(|k| !k.is_empty())
        .ok_or_else(|| SignInError::Denied("IdP-initiated sign-in isn't supported; start the sign-in in TaskCraft".into()))?;
    let pending: Pending = serde_json::from_value(super::take_state(state, &key, headers).await?)
        .map_err(|_| SignInError::Denied("the sign-in expired, please try again".into()))?;
    if pending.provider != c.common.id {
        return Err(SignInError::Denied("this sign-in was started at another provider".into()));
    }
    let sp = service_provider(state, c, idp_metadata(c).await?)?;
    let encoded: String = form.saml_response.chars().filter(|ch| !ch.is_ascii_whitespace()).collect();
    let request_id = pending.request_id.clone();
    // libxml2 / xmlsec work is CPU-bound and synchronous
    let assertion = tokio::task::spawn_blocking(move || sp.parse_base64_response(&encoded, Some(&[request_id.as_str()])).map_err(|e| e.to_string()))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map_err(|e| SignInError::Denied(format!("invalid SAML response: {e}")))?;
    let ext = identity(c, &assertion).ok_or_else(|| SignInError::Denied("the SAML assertion has no subject".into()))?;
    let user = super::sign_in(state, provider, ext).await?;
    super::finish(state, user, headers, &pending.next).await
}

const EMAIL: &[&str] =
    &["email", "mail", "emailaddress", "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress", "urn:oid:0.9.2342.19200300.100.1.3"];
const NAME: &[&str] = &[
    "displayName",
    "name",
    "http://schemas.microsoft.com/identity/claims/displayname",
    "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name",
    "urn:oid:2.16.840.1.113730.3.1.241",
    "cn",
];
const TRANSIENT: &str = "urn:oasis:names:tc:SAML:2.0:nameid-format:transient";
const STABLE_ID: &[&str] = &[
    "http://schemas.microsoft.com/identity/claims/objectidentifier",
    "objectGUID",
    "uid",
    "urn:oid:0.9.2342.19200300.100.1.1",
    "eduPersonPrincipalName",
    "urn:oid:1.3.6.1.4.1.5923.1.1.1.6",
];
const GROUPS: &[&str] = &["groups", "memberOf", "http://schemas.microsoft.com/ws/2008/06/identity/claims/groups", "http://schemas.xmlsoap.org/claims/Group"];

/// Maps a verified assertion to an external identity.
fn identity(c: &SamlConfig, a: &Assertion) -> Option<External> {
    let attrs: Vec<&samael::attribute::Attribute> = a.attribute_statements.iter().flatten().flat_map(|s| s.attributes.iter()).collect();
    let values = |configured: Option<&String>, defaults: &[&str]| -> Vec<String> {
        let names: Vec<&str> = configured.map(|s| vec![s.as_str()]).unwrap_or_else(|| defaults.to_vec());
        for n in names {
            if let Some(attr) = attrs.iter().find(|at| {
                at.name.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(n)) || at.friendly_name.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(n))
            }) {
                let v: Vec<String> = attr.values.iter().filter_map(|v| v.value.clone()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
                if !v.is_empty() {
                    return v;
                }
            }
        }
        Vec::new()
    };
    let name_id = a.subject.as_ref()?.name_id.as_ref()?;
    let email = values(c.email_attribute.as_ref(), EMAIL).into_iter().next().or_else(|| name_id.value.contains('@').then(|| name_id.value.trim().to_string()));
    // a transient NameID changes on every sign-in: identify the person by a stable attribute instead
    let subject = if name_id.format.as_deref() == Some(TRANSIENT) {
        values(None, STABLE_ID).into_iter().next().or_else(|| email.clone().map(|e| e.to_lowercase()))?
    } else {
        name_id.value.trim().to_string()
    };
    if subject.is_empty() {
        return None;
    }
    let name = values(c.name_attribute.as_ref(), NAME).into_iter().next().or_else(|| {
        let given = values(None, &["givenName", "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/givenname", "urn:oid:2.5.4.42"]);
        let sur = values(None, &["sn", "surname", "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/surname", "urn:oid:2.5.4.4"]);
        let full = format!("{} {}", given.first().map(String::as_str).unwrap_or(""), sur.first().map(String::as_str).unwrap_or("")).trim().to_string();
        (!full.is_empty()).then_some(full)
    });
    Some(External {
        provider_key: a.issuer.value.clone().unwrap_or_else(|| format!("saml:{}", c.common.id)),
        subject,
        email,
        // the IdP vouches for the directory e-mail it asserts
        email_verified: None,
        name,
        groups: values(c.groups_attribute.as_ref(), GROUPS),
    })
}
