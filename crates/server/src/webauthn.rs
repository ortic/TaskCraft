//! WebAuthn (passkeys) for a relying party that doesn't need attestation: checks what the
//! browser can't vouch for, the way most sites do.
//!
//! * Registration (`navigator.credentials.create`): client data of type `webauthn.create` with our
//!   challenge and origin, authenticator data with our RP ID hash, user presence and a credential;
//!   the credential's public key (COSE) is kept. Attestation statements are not checked (we ask
//!   for `none`).
//! * Authentication (`navigator.credentials.get`): client data of type `webauthn.get`, RP ID hash,
//!   user presence, the signature over `authenticatorData ‖ SHA-256(clientDataJSON)` with the
//!   stored key, and a signature counter that only grows (when the authenticator keeps one).
//!
//! Keys: ES256 (P-256), EdDSA (Ed25519) and RS256, verified with `ring`. Binary values travel as
//! base64url without padding.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::Value as Cbor;
use ring::signature;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// COSE algorithm ids.
pub const ES256: i64 = -7;
pub const EDDSA: i64 = -8;
pub const RS256: i64 = -257;
pub const ALGORITHMS: [i64; 3] = [ES256, EDDSA, RS256];

/// Authenticator data flags.
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const BS: u8 = 0x10;
const AT: u8 = 0x40;

/// Who we are to authenticators: the RP ID (our host name) and the origin pages run on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingParty {
    pub id: String,
    pub origin: String,
}

impl RelyingParty {
    /// From TaskCraft's base URL (`https://tasks.example.com:8443/…` → id `tasks.example.com`,
    /// origin `https://tasks.example.com:8443`).
    pub fn from_base_url(base: &str) -> anyhow::Result<Self> {
        let (scheme, rest) = base.trim().split_once("://").ok_or_else(|| anyhow::anyhow!("base URL without a scheme: {base}"))?;
        let scheme = scheme.to_ascii_lowercase();
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
        let authority = authority.rsplit('@').next().unwrap_or("");
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') && p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), Some(p.to_string())),
            _ => (authority.to_string(), None),
        };
        anyhow::ensure!(!host.is_empty(), "base URL without a host: {base}");
        let default_port = matches!((scheme.as_str(), port.as_deref()), ("https", Some("443")) | ("http", Some("80")));
        let origin = match port {
            Some(p) if !default_port => format!("{scheme}://{host}:{p}"),
            _ => format!("{scheme}://{host}"),
        };
        Ok(Self { id: host, origin })
    }
}

pub fn b64(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

pub fn unb64(s: &str) -> anyhow::Result<Vec<u8>> {
    Ok(URL_SAFE_NO_PAD.decode(s.trim_end_matches('='))?)
}

/// A fresh random challenge.
pub fn challenge() -> Vec<u8> {
    let mut c = vec![0u8; 32];
    rand::fill(&mut c[..]);
    c
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default)]
    cross_origin: bool,
}

fn check_client_data(rp: &RelyingParty, kind: &str, challenge: &[u8], json: &[u8]) -> anyhow::Result<()> {
    let c: ClientData = serde_json::from_slice(json).map_err(|e| anyhow::anyhow!("client data: {e}"))?;
    anyhow::ensure!(c.kind == kind, "client data of type {} instead of {kind}", c.kind);
    anyhow::ensure!(unb64(&c.challenge)? == challenge, "the challenge doesn't match");
    anyhow::ensure!(c.origin == rp.origin, "signed for {} instead of {}", c.origin, rp.origin);
    anyhow::ensure!(!c.cross_origin, "signed inside a frame of another site");
    Ok(())
}

struct AuthData {
    flags: u8,
    sign_count: u32,
    /// (credential id, public key as COSE)
    credential: Option<(Vec<u8>, Vec<u8>)>,
}

fn parse_auth_data(rp: &RelyingParty, data: &[u8]) -> anyhow::Result<AuthData> {
    anyhow::ensure!(data.len() >= 37, "authenticator data too short");
    anyhow::ensure!(data[..32] == Sha256::digest(rp.id.as_bytes())[..], "the passkey belongs to another site");
    let flags = data[32];
    anyhow::ensure!(flags & UP != 0, "the user wasn't present");
    let sign_count = u32::from_be_bytes([data[33], data[34], data[35], data[36]]);
    let credential = if flags & AT != 0 {
        let rest = &data[37..];
        anyhow::ensure!(rest.len() >= 18, "credential data too short");
        let len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        anyhow::ensure!(len > 0 && len <= 1023 && rest.len() > 18 + len, "credential id of a wrong length");
        let id = rest[18..18 + len].to_vec();
        // the key is CBOR (extensions may follow it): kept in our own encoding
        let key: Cbor = ciborium::from_reader(&rest[18 + len..]).map_err(|e| anyhow::anyhow!("credential key: {e}"))?;
        let mut cose = Vec::new();
        ciborium::into_writer(&key, &mut cose)?;
        Some((id, cose))
    } else {
        None
    };
    Ok(AuthData { flags, sign_count, credential })
}

fn cose_field(map: &[(Cbor, Cbor)], key: i64) -> Option<&Cbor> {
    map.iter().find(|(k, _)| k.as_integer().is_some_and(|i| i128::from(i) == i128::from(key))).map(|(_, v)| v)
}

fn cose_int(map: &[(Cbor, Cbor)], key: i64) -> Option<i64> {
    cose_field(map, key).and_then(Cbor::as_integer).and_then(|i| i64::try_from(i128::from(i)).ok())
}

fn cose_bytes(map: &[(Cbor, Cbor)], key: i64) -> anyhow::Result<&[u8]> {
    cose_field(map, key).and_then(Cbor::as_bytes).map(Vec::as_slice).ok_or_else(|| anyhow::anyhow!("credential key without field {key}"))
}

/// The algorithm of a COSE key (and that we can use it).
pub fn key_algorithm(cose: &[u8]) -> anyhow::Result<i64> {
    let key: Cbor = ciborium::from_reader(cose)?;
    let map = key.as_map().ok_or_else(|| anyhow::anyhow!("credential key isn't a map"))?;
    let alg = cose_int(map, 3).ok_or_else(|| anyhow::anyhow!("credential key without algorithm"))?;
    match alg {
        ES256 => {
            anyhow::ensure!(cose_int(map, 1) == Some(2) && cose_int(map, -1) == Some(1), "ES256 key that isn't P-256");
            anyhow::ensure!(cose_bytes(map, -2)?.len() == 32 && cose_bytes(map, -3)?.len() == 32, "P-256 key of a wrong size");
        }
        EDDSA => {
            anyhow::ensure!(cose_int(map, 1) == Some(1) && cose_int(map, -1) == Some(6), "EdDSA key that isn't Ed25519");
            anyhow::ensure!(cose_bytes(map, -2)?.len() == 32, "Ed25519 key of a wrong size");
        }
        RS256 => {
            anyhow::ensure!(cose_int(map, 1) == Some(3), "RS256 key that isn't RSA");
            let n = cose_bytes(map, -1)?.len();
            anyhow::ensure!((256..=1024).contains(&n), "RSA key of {} bits (2048 to 8192 work)", n * 8);
            cose_bytes(map, -2)?;
        }
        other => anyhow::bail!("unsupported key algorithm {other}"),
    }
    Ok(alg)
}

fn verify_signature(cose: &[u8], message: &[u8], sig: &[u8]) -> anyhow::Result<()> {
    let alg = key_algorithm(cose)?;
    let key: Cbor = ciborium::from_reader(cose)?;
    let map = key.as_map().ok_or_else(|| anyhow::anyhow!("credential key isn't a map"))?;
    let ok = match alg {
        ES256 => {
            let mut point = vec![4u8];
            point.extend_from_slice(cose_bytes(map, -2)?);
            point.extend_from_slice(cose_bytes(map, -3)?);
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point).verify(message, sig).is_ok()
        }
        EDDSA => signature::UnparsedPublicKey::new(&signature::ED25519, cose_bytes(map, -2)?).verify(message, sig).is_ok(),
        _ => signature::RsaPublicKeyComponents { n: cose_bytes(map, -1)?, e: cose_bytes(map, -2)? }
            .verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, sig)
            .is_ok(),
    };
    anyhow::ensure!(ok, "the signature doesn't match the passkey");
    Ok(())
}

/// A credential from a registration.
#[derive(Debug)]
pub struct NewCredential {
    pub id: Vec<u8>,
    pub public_key: Vec<u8>,
    pub algorithm: i64,
    pub sign_count: u32,
    pub user_verified: bool,
    /// synced to other devices (a passkey in iCloud Keychain, Google Password Manager, …)
    pub backed_up: bool,
}

/// Checks the answer to `navigator.credentials.create()`.
pub fn verify_registration(rp: &RelyingParty, challenge: &[u8], client_data_json: &[u8], attestation_object: &[u8]) -> anyhow::Result<NewCredential> {
    check_client_data(rp, "webauthn.create", challenge, client_data_json)?;
    let att: Cbor = ciborium::from_reader(attestation_object).map_err(|e| anyhow::anyhow!("attestation object: {e}"))?;
    let map = att.as_map().ok_or_else(|| anyhow::anyhow!("attestation object isn't a map"))?;
    let auth_data = map
        .iter()
        .find(|(k, _)| k.as_text() == Some("authData"))
        .and_then(|(_, v)| v.as_bytes())
        .ok_or_else(|| anyhow::anyhow!("attestation object without authenticator data"))?;
    let data = parse_auth_data(rp, auth_data)?;
    let (id, public_key) = data.credential.ok_or_else(|| anyhow::anyhow!("no credential in the answer"))?;
    let algorithm = key_algorithm(&public_key)?;
    Ok(NewCredential { id, public_key, algorithm, sign_count: data.sign_count, user_verified: data.flags & UV != 0, backed_up: data.flags & BS != 0 })
}

/// What a successful sign-in with a passkey tells.
#[derive(Debug)]
pub struct Verified {
    pub sign_count: u32,
    pub user_verified: bool,
    pub backed_up: bool,
}

/// Checks the answer to `navigator.credentials.get()` against a stored passkey.
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &[u8],
    client_data_json: &[u8],
    authenticator_data: &[u8],
    sig: &[u8],
    public_key: &[u8],
    stored_count: u32,
) -> anyhow::Result<Verified> {
    check_client_data(rp, "webauthn.get", challenge, client_data_json)?;
    let data = parse_auth_data(rp, authenticator_data)?;
    let mut message = authenticator_data.to_vec();
    message.extend_from_slice(&Sha256::digest(client_data_json));
    verify_signature(public_key, &message, sig)?;
    // authenticators that count signatures must count up (else the key may have been copied)
    if (data.sign_count != 0 || stored_count != 0) && data.sign_count <= stored_count {
        anyhow::bail!("the passkey's signature counter went backwards");
    }
    Ok(Verified { sign_count: data.sign_count, user_verified: data.flags & UV != 0, backed_up: data.flags & BS != 0 })
}

/// A software authenticator (P-256 or Ed25519), for tests.
pub mod testing {
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair};

    use super::*;

    pub enum Key {
        P256(EcdsaKeyPair),
        Ed25519(Ed25519KeyPair),
    }

    pub struct Authenticator {
        pub rp: RelyingParty,
        pub credential_id: Vec<u8>,
        pub key: Key,
        pub counter: u32,
        pub user_verified: bool,
    }

    impl Authenticator {
        pub fn new(rp: RelyingParty, ed25519: bool) -> Self {
            let rng = SystemRandom::new();
            let key = if ed25519 {
                Key::Ed25519(Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap())
            } else {
                let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
                Key::P256(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap())
            };
            Self { rp, credential_id: challenge()[..16].to_vec(), key, counter: 0, user_verified: true }
        }

        fn cose(&self) -> Vec<u8> {
            let i = |v: i64| Cbor::Integer(v.into());
            let map = match &self.key {
                Key::P256(k) => {
                    let p = k.public_key().as_ref();
                    vec![(i(1), i(2)), (i(3), i(ES256)), (i(-1), i(1)), (i(-2), Cbor::Bytes(p[1..33].to_vec())), (i(-3), Cbor::Bytes(p[33..65].to_vec()))]
                }
                Key::Ed25519(k) => vec![(i(1), i(1)), (i(3), i(EDDSA)), (i(-1), i(6)), (i(-2), Cbor::Bytes(k.public_key().as_ref().to_vec()))],
            };
            let mut out = Vec::new();
            ciborium::into_writer(&Cbor::Map(map), &mut out).unwrap();
            out
        }

        fn auth_data(&self, attested: bool) -> Vec<u8> {
            let mut d = Sha256::digest(self.rp.id.as_bytes()).to_vec();
            d.push(UP | if self.user_verified { UV } else { 0 } | if attested { AT } else { 0 });
            d.extend_from_slice(&self.counter.to_be_bytes());
            if attested {
                d.extend_from_slice(&[0u8; 16]);
                d.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
                d.extend_from_slice(&self.credential_id);
                d.extend_from_slice(&self.cose());
            }
            d
        }

        fn client_data(&self, kind: &str, challenge: &[u8]) -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({"type": kind, "challenge": b64(challenge), "origin": self.rp.origin, "crossOrigin": false})).unwrap()
        }

        /// (clientDataJSON, attestationObject) for a registration.
        pub fn register(&self, challenge: &[u8]) -> (Vec<u8>, Vec<u8>) {
            let att = Cbor::Map(vec![
                (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
                (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
                (Cbor::Text("authData".into()), Cbor::Bytes(self.auth_data(true))),
            ]);
            let mut out = Vec::new();
            ciborium::into_writer(&att, &mut out).unwrap();
            (self.client_data("webauthn.create", challenge), out)
        }

        /// (clientDataJSON, authenticatorData, signature) for a sign-in.
        pub fn sign(&mut self, challenge: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
            self.counter += 1;
            let client = self.client_data("webauthn.get", challenge);
            let data = self.auth_data(false);
            let mut msg = data.clone();
            msg.extend_from_slice(&Sha256::digest(&client));
            let sig = match &self.key {
                Key::P256(k) => k.sign(&SystemRandom::new(), &msg).unwrap().as_ref().to_vec(),
                Key::Ed25519(k) => k.sign(&msg).as_ref().to_vec(),
            };
            (client, data, sig)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Authenticator;
    use super::*;

    fn rp() -> RelyingParty {
        RelyingParty::from_base_url("https://tasks.example.com").unwrap()
    }

    #[test]
    fn relying_party_from_base_url() {
        assert_eq!(rp(), RelyingParty { id: "tasks.example.com".into(), origin: "https://tasks.example.com".into() });
        let r = RelyingParty::from_base_url("http://localhost:8788/tasks/").unwrap();
        assert_eq!((r.id.as_str(), r.origin.as_str()), ("localhost", "http://localhost:8788"));
        let r = RelyingParty::from_base_url("https://Tasks.Example.com:443").unwrap();
        assert_eq!((r.id.as_str(), r.origin.as_str()), ("tasks.example.com", "https://tasks.example.com"));
        assert!(RelyingParty::from_base_url("tasks.example.com").is_err(), "a scheme is needed");
    }

    #[test]
    fn registers_and_signs_in() {
        for ed25519 in [false, true] {
            let mut a = Authenticator::new(rp(), ed25519);
            let c = challenge();
            let (client, att) = a.register(&c);
            let cred = verify_registration(&rp(), &c, &client, &att).unwrap();
            assert_eq!(cred.id, a.credential_id);
            assert_eq!(cred.algorithm, if ed25519 { EDDSA } else { ES256 });
            assert!(cred.user_verified);

            let c = challenge();
            let (client, data, sig) = a.sign(&c);
            let v = verify_assertion(&rp(), &c, &client, &data, &sig, &cred.public_key, 0).unwrap();
            assert_eq!(v.sign_count, 1);
            // replayed: the counter didn't grow
            assert!(verify_assertion(&rp(), &c, &client, &data, &sig, &cred.public_key, 1).is_err());
            // another challenge, another site, a tampered signature
            assert!(verify_assertion(&rp(), &challenge(), &client, &data, &sig, &cred.public_key, 0).is_err());
            let other = RelyingParty::from_base_url("https://evil.example.com").unwrap();
            assert!(verify_assertion(&other, &c, &client, &data, &sig, &cred.public_key, 0).is_err());
            let mut bad = sig.clone();
            let last = bad.len() - 1;
            bad[last] ^= 1;
            assert!(verify_assertion(&rp(), &c, &client, &data, &bad, &cred.public_key, 0).is_err());
            // a signature from another key
            let mut b = Authenticator::new(rp(), ed25519);
            let (client, data, sig) = b.sign(&c);
            assert!(verify_assertion(&rp(), &c, &client, &data, &sig, &cred.public_key, 0).is_err());
        }
    }

    #[test]
    fn registration_checks_type_origin_and_challenge() {
        let a = Authenticator::new(rp(), false);
        let c = challenge();
        let (client, att) = a.register(&c);
        assert!(verify_registration(&rp(), &challenge(), &client, &att).is_err());
        let other = RelyingParty { id: rp().id, origin: "https://other.example.com".into() };
        assert!(verify_registration(&other, &c, &client, &att).unwrap_err().to_string().contains("instead of"));
        let get = serde_json::to_vec(&serde_json::json!({"type": "webauthn.get", "challenge": b64(&c), "origin": rp().origin})).unwrap();
        assert!(verify_registration(&rp(), &c, &get, &att).is_err());
    }

    #[test]
    fn rejects_malformed_answers() {
        let a = Authenticator::new(rp(), false);
        let c = challenge();
        let (client, att) = a.register(&c);
        assert!(verify_registration(&rp(), &c, &client, b"not cbor").is_err());
        assert!(verify_registration(&rp(), &c, &client, &att[..att.len() / 2]).is_err(), "truncated");
        assert!(verify_registration(&rp(), &c, b"{", &att).is_err(), "client data isn't JSON");
        // the same origin, but a passkey made for another RP ID
        let other = RelyingParty { id: "evil.example.com".into(), origin: rp().origin };
        assert!(verify_registration(&other, &c, &client, &att).unwrap_err().to_string().contains("another site"));
        // nobody touched the authenticator
        let cred = verify_registration(&rp(), &c, &client, &att).unwrap();
        let mut data = Sha256::digest(rp().id.as_bytes()).to_vec();
        data.extend_from_slice(&[0, 0, 0, 0, 1]);
        let client = serde_json::to_vec(&serde_json::json!({"type": "webauthn.get", "challenge": b64(&c), "origin": rp().origin})).unwrap();
        let err = verify_assertion(&rp(), &c, &client, &data, &[0; 64], &cred.public_key, 0).unwrap_err();
        assert!(err.to_string().contains("wasn't present"), "{err}");
        assert!(verify_assertion(&rp(), &c, &client, &data[..20], &[0; 64], &cred.public_key, 0).is_err(), "too short");
        // keys we can't use
        let cose = |m: Vec<(i64, Cbor)>| {
            let mut v = Vec::new();
            ciborium::into_writer(&Cbor::Map(m.into_iter().map(|(k, v)| (Cbor::Integer(k.into()), v)).collect()), &mut v).unwrap();
            v
        };
        let int = |i: i64| Cbor::Integer(i.into());
        assert!(key_algorithm(&cose(vec![(1, int(2)), (3, int(-35))])).is_err(), "ES384");
        assert!(key_algorithm(&cose(vec![(1, int(2)), (3, int(ES256)), (-1, int(1))])).is_err(), "no coordinates");
        assert!(
            key_algorithm(&cose(vec![(1, int(3)), (3, int(RS256)), (-1, Cbor::Bytes(vec![1; 128])), (-2, Cbor::Bytes(vec![1, 0, 1]))])).is_err(),
            "RSA-1024"
        );
    }
}
