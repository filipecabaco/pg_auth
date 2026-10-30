//! The network listener (PG_AUTH_LISTEN), for an extension that reaches the sidecar by URL
//! (pg_auth.sidecar_url) rather than on its Unix socket: the same protocol, one request per
//! `POST /v1/ask`, over HTTPS. Every request and every answer is signed with a secret both ends
//! hold (HMAC-SHA256), so a caller without it is refused and an answer is bound to the question
//! it answers. A request is taken once: one whose timestamp is 30 s off, or that was seen before
//! (its nonce makes each signature new), is refused (docs/proposals/external-providers.md).
//!
//! ```text
//! pg-auth-timestamp: 1793350000
//! pg-auth-nonce:     <unique per request, up to 64 of [A-Za-z0-9_-]>
//! pg-auth-signature: v1=<hex HMAC(secret, "v1\n<timestamp>\n<nonce>\nPOST\n/v1/ask\n<body>")>
//!   response:        v1=<hex HMAC(secret, "v1 response\n<request signature>\n<status>\n<body>")>
//! ```
//!
//! Before a body is read, its headers are checked (signed, on time); connections are limited
//! (PG_AUTH_MAX_CONNECTIONS, 512), and a TLS handshake or a body has 10 s. Headers have no
//! timeout: one would also close the idle connections Postgres keeps between requests.

use crate::protocol::{self, Request, Response};
use crate::{State, answer, asked_line, env, told_line};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::{Method, StatusCode};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// A timestamp further than this from the sidecar's clock is refused (a replayed request).
pub const SKEW: Duration = Duration::from_secs(30);
/// Bodies above this are refused: a check of thousands of ids is far below it.
const MAX_BODY: usize = 16 * 1024 * 1024;
/// The shortest secret taken: 32 bytes, HMAC-SHA256's own strength.
pub const MIN_SECRET: usize = 32;
pub const TIMESTAMP: &str = "pg-auth-timestamp";
pub const NONCE: &str = "pg-auth-nonce";
pub const SIGNATURE: &str = "pg-auth-signature";
/// The endpoint's name in the signature: fixed, so a proxy that rewrites paths changes nothing.
const ENDPOINT: &str = "/v1/ask";
/// A TLS handshake, or a request's body, that takes longer is dropped.
const SLOW: Duration = Duration::from_secs(10);
/// Signatures remembered to refuse a request seen before: past this many, the expired are dropped,
/// and past ten times as many, requests are refused until some expire.
const SEEN: usize = 100_000;

/// The secrets a request may be signed with: one per line of PG_AUTH_SECRET_FILE, so a secret is
/// rotated by adding the new one, moving Postgres to it, then removing the old one.
pub struct Secrets(Vec<Vec<u8>>);

impl Secrets {
    pub fn parse(text: &str) -> Result<Secrets, String> {
        let secrets: Vec<Vec<u8>> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| l.as_bytes().to_vec()).collect();
        if secrets.is_empty() {
            return Err("no secret in it".into());
        }
        if secrets.iter().any(|s| s.len() < MIN_SECRET) {
            return Err(format!("every secret must be at least {MIN_SECRET} bytes"));
        }
        Ok(Secrets(secrets))
    }

    /// The secret that signed this request, if any; `now` in seconds since the epoch.
    pub fn verify(&self, timestamp: &str, nonce: &str, signature: &str, body: &[u8], now: u64) -> Result<&[u8], &'static str> {
        heard(timestamp, nonce, signature, now)?;
        let given = signature.strip_prefix("v1=").and_then(unhex).ok_or("a malformed signature")?;
        self.0
            .iter()
            .find(|secret| mac(secret, &request_text(timestamp, nonce, body)).verify_slice(&given).is_ok())
            .map(Vec::as_slice)
            .ok_or("a wrong signature")
    }
}

/// What a request's headers say, checked before its body is read: on time, a nonce, a signature
/// that could be one.
fn heard(timestamp: &str, nonce: &str, signature: &str, now: u64) -> Result<(), &'static str> {
    let at: u64 = timestamp.parse().map_err(|_| "a bad timestamp")?;
    if at.abs_diff(now) > SKEW.as_secs() {
        return Err("a timestamp too far from now");
    }
    if nonce.is_empty() || nonce.len() > 64 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("a bad nonce");
    }
    if !signature.starts_with("v1=") || signature.len() != 3 + 64 {
        return Err("a malformed signature");
    }
    Ok(())
}

/// Signatures taken in the last minute: a request seen before is refused. Only verified
/// signatures are kept, so no one without the secret can fill it.
#[derive(Default)]
pub struct Seen(Mutex<HashMap<String, u64>>);

impl Seen {
    /// Whether this signature is new (and now seen), `now` in seconds since the epoch.
    fn first(&self, signature: &str, now: u64) -> Result<(), &'static str> {
        let Ok(mut seen) = self.0.lock() else { return Err("a busy sidecar") };
        if seen.len() > SEEN {
            seen.retain(|_, until| *until > now);
        }
        if seen.len() > SEEN * 10 {
            return Err("too many requests in the last minute");
        }
        match seen.insert(signature.to_owned(), now + 2 * SKEW.as_secs()) {
            None => Ok(()),
            Some(_) => Err("a request seen before"),
        }
    }
}

#[expect(clippy::expect_used, reason = "HMAC takes a key of any length")]
fn mac(secret: &[u8], text: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(text);
    mac
}

fn request_text(timestamp: &str, nonce: &str, body: &[u8]) -> Vec<u8> {
    [format!("v1\n{timestamp}\n{nonce}\nPOST\n{ENDPOINT}\n").as_bytes(), body].concat()
}

fn response_text(request_signature: &str, status: u16, body: &[u8]) -> Vec<u8> {
    [format!("v1 response\n{request_signature}\n{status}\n").as_bytes(), body].concat()
}

/// The signature of a request, as the extension sends it (src/sidecar.rs there).
#[cfg(test)]
pub fn sign_request(secret: &[u8], timestamp: &str, nonce: &str, body: &[u8]) -> String {
    format!("v1={}", hex(&mac(secret, &request_text(timestamp, nonce, body)).finalize().into_bytes()))
}

/// The signature of an answer to the request signed `request_signature`.
pub fn sign_response(secret: &[u8], request_signature: &str, status: u16, body: &[u8]) -> String {
    format!("v1={}", hex(&mac(secret, &response_text(request_signature, status, body)).finalize().into_bytes()))
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

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Answer over HTTPS on PG_AUTH_LISTEN when it is set: refuses to start without the secrets, or
/// without TLS unless PG_AUTH_TLS_TERMINATED=1 says a platform in front terminates it.
pub async fn start(state: &Arc<State>) -> Result<(), String> {
    let Some(addr) = env("PG_AUTH_LISTEN") else { return Ok(()) };
    let file = env("PG_AUTH_SECRET_FILE").ok_or("PG_AUTH_LISTEN needs PG_AUTH_SECRET_FILE: requests are signed")?;
    let text = std::fs::read_to_string(&file).map_err(|e| format!("PG_AUTH_SECRET_FILE {file}: {e}"))?;
    let guard = Arc::new(Guard::new(Secrets::parse(&text).map_err(|e| format!("PG_AUTH_SECRET_FILE {file}: {e}"))?));
    let max = env("PG_AUTH_MAX_CONNECTIONS").and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(512);
    let tls = match (env("PG_AUTH_TLS_CERT"), env("PG_AUTH_TLS_KEY")) {
        (Some(cert), Some(key)) => Some(tls(&cert, &key)?),
        (None, None) if env("PG_AUTH_TLS_TERMINATED").as_deref() == Some("1") => None,
        _ => {
            return Err("PG_AUTH_LISTEN needs PG_AUTH_TLS_CERT and PG_AUTH_TLS_KEY, or PG_AUTH_TLS_TERMINATED=1 behind a platform that \
                        terminates TLS"
                .into());
        }
    };
    let listener = TcpListener::bind(&addr).await.map_err(|e| format!("PG_AUTH_LISTEN {addr}: {e}"))?;
    let how = if tls.is_some() { "https" } else { "TLS terminated in front" };
    eprintln!("pg-auth-sidecar: listening on {addr} ({how}, signed requests)");
    tokio::spawn(serve(listener, tls, guard, state.clone(), max));
    Ok(())
}

/// TLS from a certificate chain and its key (PEM), or none when a platform in front terminates it.
pub fn tls(cert: &str, key: &str) -> Result<TlsAcceptor, String> {
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("PG_AUTH_TLS_CERT {cert}: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("PG_AUTH_TLS_CERT {cert}: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("PG_AUTH_TLS_KEY {key}: {e}"))?;
    let config = ServerConfig::builder().with_no_client_auth().with_single_cert(certs, key).map_err(|e| format!("TLS: {e}"))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// What a request is checked against: the secrets, the signatures seen, and the refusals logged.
pub struct Guard {
    secrets: Secrets,
    seen: Seen,
    /// refusals in the current second, after the first, logged when the next second starts
    refused: Mutex<(u64, u64)>,
}

impl Guard {
    pub fn new(secrets: Secrets) -> Guard {
        Guard { secrets, seen: Seen::default(), refused: Mutex::new((0, 0)) }
    }

    /// A refusal, logged at most once a second (a flood of them would fill the log).
    fn refuse(&self, why: &str, now: u64) {
        let Ok(mut r) = self.refused.lock() else { return };
        if r.0 == now {
            r.1 += 1;
            return;
        }
        if r.1 > 0 {
            eprintln!("pg-auth-sidecar: and refused {} more requests in that second", r.1);
        }
        *r = (now, 0);
        eprintln!("pg-auth-sidecar: refused a request: {why}");
    }
}

/// Accept connections on `listener` and answer them, over TLS unless `tls` is None; at most
/// `max` at once (a connection past it is closed at once).
pub async fn serve(listener: TcpListener, tls: Option<TlsAcceptor>, guard: Arc<Guard>, state: Arc<State>, max: usize) {
    let slots = Arc::new(tokio::sync::Semaphore::new(max));
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
        let (tls, guard, state) = (tls.clone(), guard.clone(), state.clone());
        tokio::spawn(async move {
            let _slot = slot;
            let service = hyper::service::service_fn(move |req| handle(req, guard.clone(), state.clone()));
            let http = hyper::server::conn::http1::Builder::new();
            let _ = match tls {
                Some(tls) => match tokio::time::timeout(SLOW, tls.accept(stream)).await {
                    Ok(Ok(stream)) => http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await,
                    _ => return,
                },
                None => http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await,
            };
        });
    }
}

type Answer = hyper::Response<Full<Bytes>>;

async fn handle(req: hyper::Request<Incoming>, guard: Arc<Guard>, state: Arc<State>) -> Result<Answer, std::convert::Infallible> {
    let plain = |status: StatusCode, text: &'static str| {
        let mut r = hyper::Response::new(Full::new(Bytes::from_static(text.as_bytes())));
        *r.status_mut() = status;
        r
    };
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/healthz") => return Ok(plain(StatusCode::OK, "ok\n")),
        (&Method::POST, ENDPOINT) => {}
        _ => return Ok(plain(StatusCode::NOT_FOUND, "not found\n")),
    }
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned).unwrap_or_default();
    let (timestamp, nonce, signature) = (header(TIMESTAMP), header(NONCE), header(SIGNATURE));
    let at = now();
    // the headers first: no body is read for a request that cannot be signed or is not on time
    if let Err(why) = heard(&timestamp, &nonce, &signature, at) {
        guard.refuse(why, at);
        return Ok(plain(StatusCode::UNAUTHORIZED, "refused\n"));
    }
    let body = match tokio::time::timeout(SLOW, Limited::new(req.into_body(), MAX_BODY).collect()).await {
        Ok(Ok(b)) => b.to_bytes(),
        Ok(Err(_)) => return Ok(plain(StatusCode::PAYLOAD_TOO_LARGE, "too large\n")),
        Err(_) => return Ok(plain(StatusCode::REQUEST_TIMEOUT, "too slow\n")),
    };
    let secret = match guard.secrets.verify(&timestamp, &nonce, &signature, &body, at).map(<[u8]>::to_vec) {
        Ok(secret) => secret,
        Err(why) => {
            guard.refuse(why, at);
            return Ok(plain(StatusCode::UNAUTHORIZED, "refused\n"));
        }
    };
    if let Err(why) = guard.seen.first(&signature, at) {
        guard.refuse(why, at);
        return Ok(plain(StatusCode::UNAUTHORIZED, "refused\n"));
    }
    let mut engine = Duration::ZERO;
    let line = String::from_utf8_lossy(&body);
    let mut asked = None;
    let response = match protocol::parse(line.trim_end()) {
        Ok(request) => {
            asked = (state.log && request != Request::Ping).then(|| asked_line(&request));
            answer(&state, request, &mut engine).await
        }
        Err(e) => Response::Error { error: e },
    };
    if let Some(asked) = asked {
        eprintln!("{asked} → {}", told_line(&response, state.engine.name(), engine));
    }
    let text = protocol::encode(&response, engine);
    let mut r = hyper::Response::new(Full::new(Bytes::from(text.clone())));
    let signed = sign_response(&secret, &signature, 200, text.as_bytes());
    // hex always makes a header value; were it not, the answer goes unsigned and is refused
    if let Ok(signed) = hyper::header::HeaderValue::from_str(&signed) {
        r.headers_mut().insert(SIGNATURE, signed);
    }
    r.headers_mut().insert(hyper::header::CONTENT_TYPE, hyper::header::HeaderValue::from_static("application/json"));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "a-secret-of-at-least-thirty-two-bytes-long";
    const B: &str = "another-secret-of-at-least-thirty-two-bytes";

    #[test]
    fn secrets_are_one_per_line_and_long_enough() {
        assert_eq!(Secrets::parse(&format!("{A}\n\n{B}\n")).unwrap().0.len(), 2);
        assert!(Secrets::parse("\n").is_err());
        assert!(Secrets::parse("short").err().unwrap().contains("32 bytes"));
    }

    #[test]
    fn a_request_is_taken_only_signed_with_a_secret_and_on_time() {
        let secrets = Secrets::parse(&format!("{A}\n{B}")).unwrap();
        let body = br#"{"op":"ping"}"#;
        let signed = sign_request(B.as_bytes(), "1000", "n-1", body);
        assert_eq!(secrets.verify("1000", "n-1", &signed, body, 1010), Ok(B.as_bytes()), "either secret, while rotating");
        assert_eq!(secrets.verify("1000", "n-1", &signed, br#"{"op":"list"}"#, 1010), Err("a wrong signature"), "body changed");
        assert_eq!(secrets.verify("1000", "n-2", &signed, body, 1010), Err("a wrong signature"), "nonce changed");
        assert_eq!(secrets.verify("1001", "n-1", &signed, body, 1010), Err("a wrong signature"), "timestamp changed");
        assert_eq!(secrets.verify("1000", "n-1", &signed, body, 1031), Err("a timestamp too far from now"), "sent again later");
        let other = sign_request(b"not-one-of-the-secrets-the-sidecar-holds", "1000", "n-1", body);
        assert_eq!(secrets.verify("1000", "n-1", &other, body, 1000), Err("a wrong signature"));
        assert_eq!(secrets.verify("1000", "n-1", "v1=zz", body, 1000), Err("a malformed signature"));
        assert_eq!(secrets.verify("1000", "n-1", &signed.replace("v1=", "v2="), body, 1000), Err("a malformed signature"));
    }

    #[test]
    fn headers_are_checked_before_a_body_is_read() {
        let signature = format!("v1={}", "0".repeat(64));
        assert_eq!(heard("1000", "n-1", &signature, 1000), Ok(()));
        assert_eq!(heard("", "n-1", &signature, 1000), Err("a bad timestamp"), "unsigned");
        assert_eq!(heard("1000", "", &signature, 1000), Err("a bad nonce"));
        assert_eq!(heard("1000", "a b", &signature, 1000), Err("a bad nonce"));
        assert_eq!(heard("1000", &"n".repeat(65), &signature, 1000), Err("a bad nonce"));
        assert_eq!(heard("1000", "n-1", "", 1000), Err("a malformed signature"));
        assert_eq!(heard("1000", "n-1", &signature, 1100), Err("a timestamp too far from now"));
    }

    #[test]
    fn a_request_is_taken_once() {
        let seen = Seen::default();
        assert_eq!(seen.first("v1=aa", 1000), Ok(()));
        assert_eq!(seen.first("v1=aa", 1010), Err("a request seen before"));
        assert_eq!(seen.first("v1=bb", 1010), Ok(()), "another request");
    }

    /// The same values the extension's tests check (src/remote.rs), computed independently: both
    /// ends must sign the same bytes.
    #[test]
    fn signatures_match_the_known_values() {
        assert_eq!(
            sign_request(A.as_bytes(), "1793350000", "n-1", br#"{"op":"ping"}"#),
            "v1=f9aa57cc61b19dc0ac58708ab8396f66f185271eba0bcc25b0db698a72d95a7e"
        );
        assert_eq!(
            sign_response(A.as_bytes(), "v1=aa", 200, b"{\"engine\":\"openfga\"}\n"),
            "v1=ba7fa6cc1f05797639848c9025512d781ed4d8a1a3dc341890697fbde393e149"
        );
    }

    #[test]
    fn an_answer_is_bound_to_its_question() {
        let one = sign_response(A.as_bytes(), "v1=aa", 200, b"{}");
        assert_ne!(one, sign_response(A.as_bytes(), "v1=bb", 200, b"{}"), "another question");
        assert_ne!(one, sign_response(A.as_bytes(), "v1=aa", 200, b"{\"ids\":[]}"), "another answer");
        assert_ne!(one, sign_response(B.as_bytes(), "v1=aa", 200, b"{}"), "another secret");
    }
}
