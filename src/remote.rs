//! The sidecar reached by URL (pg_auth.sidecar_url) instead of its Unix socket: the same protocol,
//! one request per `POST <url>/v1/ask` over HTTPS. Each request is signed with a secret the sidecar
//! holds (pg_auth.sidecar_secret_file), and each answer must carry the sidecar's signature over it
//! and the question it answers: an answer that does not is refused, and the statement fails as it
//! does when the sidecar does not answer (sidecar/query/src/net.rs;
//! docs/proposals/external-providers.md).

use crate::sidecar::{CA_FILE, SECRET_FILE, TIMEOUT_MS, URL};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The endpoint's name in the signature: fixed, so a proxy that rewrites paths changes nothing.
const ENDPOINT: &str = "/v1/ask";
const TIMESTAMP: &str = "pg-auth-timestamp";
const SIGNATURE: &str = "pg-auth-signature";
const MIN_SECRET: usize = 32;
/// Answers above this are refused: a list of every object a subject reaches is far below it.
const MAX_ANSWER: u64 = 256 * 1024 * 1024;

/// One HTTPS client per backend (its connections kept alive), for one URL and set of roots.
struct Client {
    url: String,
    ca: String,
    agent: ureq::Agent,
}

thread_local! {
    static CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    /// The secret, by the file it was read from; read again when the sidecar refuses it (rotated).
    static SECRET: RefCell<Option<(String, Vec<u8>)>> = const { RefCell::new(None) };
}

fn setting(s: &pgrx::GucSetting<Option<std::ffi::CString>>) -> Option<String> {
    s.get().map(|v| v.to_string_lossy().into_owned()).filter(|v| !v.is_empty())
}

/// The URL when the sidecar is reached by one, else None (the Unix socket).
pub fn url() -> Option<String> {
    setting(&URL)
}

/// Ask the sidecar at `url`. Err: the message the statement fails with.
pub fn exchange(url: &str, request: &Value) -> Result<Value, String> {
    if !url.starts_with("https://") {
        return Err(format!("pg_auth.sidecar_url must be https:// (it is {url}): authorization answers do not travel unencrypted"));
    }
    let body = request.to_string();
    match send(url, &body, false) {
        // a secret rotated meanwhile: the file is read again, once
        Err(Refused::Signature) => send(url, &body, true),
        other => other,
    }
    .map_err(|e| match e {
        Refused::Signature => format!(
            "the pg_auth sidecar at {url} refused pg_auth's signature: pg_auth.sidecar_secret_file must hold one of the secrets in its \
             PG_AUTH_SECRET_FILE"
        ),
        Refused::Other(m) => format!("pg_auth sidecar at {url}: {m}"),
    })
}

enum Refused {
    Signature,
    Other(String),
}

impl From<String> for Refused {
    fn from(m: String) -> Self {
        Refused::Other(m)
    }
}

fn send(url: &str, body: &str, reread: bool) -> Result<Value, Refused> {
    let secret = secret(reread)?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_string();
    let signature = sign_request(&secret, &timestamp, body.as_bytes());
    let endpoint = format!("{}{ENDPOINT}", url.trim_end_matches('/'));
    let agent = agent(url)?;
    let mut response = agent
        .post(&endpoint)
        .header(TIMESTAMP, &timestamp)
        .header(SIGNATURE, &signature)
        .content_type("application/json")
        .send(body.as_bytes())
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if status == 401 {
        return Err(Refused::Signature);
    }
    if status != 200 {
        return Err(Refused::Other(format!("HTTP {status}")));
    }
    let signed = response.headers().get(SIGNATURE).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let answer = response.body_mut().with_config().limit(MAX_ANSWER).read_to_vec().map_err(|e| e.to_string())?;
    // an answer that is not the sidecar's, to this question, is no answer
    let signed = signed.ok_or_else(|| "an unsigned answer, refused".to_string())?;
    if !verify_response(&secret, &signature, status, &answer, &signed) {
        return Err(Refused::Other("an answer not signed with the secret, refused".into()));
    }
    serde_json::from_slice(&answer).map_err(|e| Refused::Other(format!("bad sidecar response: {e}")))
}

/// This backend's client for `url`, made again when the URL or the roots changed.
fn agent(url: &str) -> Result<ureq::Agent, String> {
    let ca = setting(&CA_FILE).unwrap_or_default();
    let timeout = Duration::from_millis(u64::try_from(TIMEOUT_MS.get().max(1)).unwrap_or(1));
    CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(client) = c.as_ref().filter(|client| client.url == url && client.ca == ca) {
            return Ok(client.agent.clone());
        }
        let roots = if ca.is_empty() {
            ureq::tls::RootCerts::WebPki
        } else {
            let pem = std::fs::read(&ca).map_err(|e| format!("pg_auth.sidecar_ca_file {ca}: {e}"))?;
            let certs = ureq::tls::parse_pem(&pem)
                .filter_map(|item| match item {
                    Ok(ureq::tls::PemItem::Certificate(cert)) => Some(Ok(cert)),
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("pg_auth.sidecar_ca_file {ca}: {e}"))?;
            if certs.is_empty() {
                return Err(format!("pg_auth.sidecar_ca_file {ca}: no certificate in it"));
            }
            ureq::tls::RootCerts::Specific(Arc::new(certs))
        };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots).build())
            .build()
            .into();
        *c = Some(Client { url: url.to_owned(), ca, agent: agent.clone() });
        Ok(agent)
    })
}

fn secret(reread: bool) -> Result<Vec<u8>, String> {
    let file = setting(&SECRET_FILE).ok_or("pg_auth.sidecar_url needs pg_auth.sidecar_secret_file: requests to it are signed")?;
    SECRET.with(|s| {
        let mut s = s.borrow_mut();
        if let Some((_, secret)) = s.as_ref().filter(|(path, _)| !reread && *path == file) {
            return Ok(secret.clone());
        }
        let text = std::fs::read_to_string(&file).map_err(|e| format!("pg_auth.sidecar_secret_file {file}: {e}"))?;
        let secret = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().as_bytes().to_vec();
        if secret.len() < MIN_SECRET {
            return Err(format!("pg_auth.sidecar_secret_file {file}: the secret must be at least {MIN_SECRET} bytes"));
        }
        *s = Some((file, secret.clone()));
        Ok(secret)
    })
}

#[expect(clippy::expect_used, reason = "HMAC takes a key of any length")]
fn mac(secret: &[u8], text: &[&[u8]]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes a key of any length");
    for t in text {
        mac.update(t);
    }
    mac
}

fn sign_request(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
    let head = format!("v1\n{timestamp}\nPOST\n{ENDPOINT}\n");
    format!("v1={}", hex(&mac(secret, &[head.as_bytes(), body]).finalize().into_bytes()))
}

/// Whether `signed` is the sidecar's signature over this answer to the request signed
/// `request_signature` (compared in constant time).
fn verify_response(secret: &[u8], request_signature: &str, status: u16, answer: &[u8], signed: &str) -> bool {
    let head = format!("v1 response\n{request_signature}\n{status}\n");
    signed.strip_prefix("v1=").and_then(unhex).is_some_and(|given| mac(secret, &[head.as_bytes(), answer]).verify_slice(&given).is_ok())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod unit {
    use super::*;

    const A: &[u8] = b"a-secret-of-at-least-thirty-two-bytes-long";

    /// The values the sidecar's tests check (sidecar/query/src/net.rs), computed independently:
    /// both ends sign the same bytes.
    #[test]
    fn signatures_match_the_sidecars() {
        assert_eq!(
            sign_request(A, "1793350000", br#"{"op":"ping"}"#),
            "v1=cf7fdf96f7f1fa1155874c98dbe4fa3c88858340dfebb10b961fb74e23f00c9b"
        );
        let signed = "v1=ba7fa6cc1f05797639848c9025512d781ed4d8a1a3dc341890697fbde393e149";
        assert!(verify_response(A, "v1=aa", 200, b"{\"engine\":\"openfga\"}\n", signed));
        assert!(!verify_response(A, "v1=bb", 200, b"{\"engine\":\"openfga\"}\n", signed), "an answer to another question");
        assert!(!verify_response(A, "v1=aa", 200, b"{\"engine\":\"spicedb\"}\n", signed), "another answer");
        assert!(!verify_response(b"another-secret-of-at-least-thirty-two-bytes", "v1=aa", 200, b"{\"engine\":\"openfga\"}\n", signed));
        assert!(!verify_response(A, "v1=aa", 200, b"{\"engine\":\"openfga\"}\n", "v1=zz"));
    }
}
