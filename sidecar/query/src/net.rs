//! The network listener (PG_AUTH_LISTEN), for an extension that reaches the sidecar by URL
//! (pg_auth.sidecar_url) rather than on its Unix socket: the same protocol, one request per
//! `POST /v1/ask`, over HTTPS. Every request and every answer is signed with a secret both ends
//! hold (HMAC-SHA256), so a caller without it is refused and an answer is bound to the question
//! it answers. A captured request is refused once its timestamp is 30 s old; before that, it can
//! only ask the same question again (docs/proposals/external-providers.md).
//!
//! ```text
//! pg-auth-timestamp: 1793350000
//! pg-auth-signature: v1=<hex HMAC(secret, "v1\n<timestamp>\nPOST\n<path>\n<body>")>
//!   response:        v1=<hex HMAC(secret, "v1 response\n<request signature>\n<status>\n<body>")>
//! ```

use crate::protocol::{self, Request, Response};
use crate::{State, answer, asked_line, env, told_line};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::{Method, StatusCode};
use sha2::Sha256;
use std::sync::Arc;
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
pub const SIGNATURE: &str = "pg-auth-signature";

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
    pub fn verify(&self, timestamp: &str, signature: &str, path: &str, body: &[u8], now: u64) -> Result<&[u8], &'static str> {
        let at: u64 = timestamp.parse().map_err(|_| "a bad timestamp")?;
        if at.abs_diff(now) > SKEW.as_secs() {
            return Err("a timestamp too far from now");
        }
        let given = signature.strip_prefix("v1=").and_then(unhex).ok_or("a malformed signature")?;
        self.0
            .iter()
            .find(|secret| mac(secret, &request_text(timestamp, path, body)).verify_slice(&given).is_ok())
            .map(Vec::as_slice)
            .ok_or("a wrong signature")
    }
}

#[expect(clippy::expect_used, reason = "HMAC takes a key of any length")]
fn mac(secret: &[u8], text: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(text);
    mac
}

fn request_text(timestamp: &str, path: &str, body: &[u8]) -> Vec<u8> {
    [format!("v1\n{timestamp}\nPOST\n{path}\n").as_bytes(), body].concat()
}

fn response_text(request_signature: &str, status: u16, body: &[u8]) -> Vec<u8> {
    [format!("v1 response\n{request_signature}\n{status}\n").as_bytes(), body].concat()
}

/// The signature of a request, as the extension sends it (src/sidecar.rs there).
#[cfg(test)]
pub fn sign_request(secret: &[u8], timestamp: &str, path: &str, body: &[u8]) -> String {
    format!("v1={}", hex(&mac(secret, &request_text(timestamp, path, body)).finalize().into_bytes()))
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
    let secrets = Arc::new(Secrets::parse(&text).map_err(|e| format!("PG_AUTH_SECRET_FILE {file}: {e}"))?);
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
    tokio::spawn(serve(listener, tls, secrets, state.clone()));
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

/// Accept connections on `listener` and answer them, over TLS unless `tls` is None.
pub async fn serve(listener: TcpListener, tls: Option<TlsAcceptor>, secrets: Arc<Secrets>, state: Arc<State>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let (tls, secrets, state) = (tls.clone(), secrets.clone(), state.clone());
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| handle(req, secrets.clone(), state.clone()));
            let http = hyper::server::conn::http1::Builder::new();
            let _ = match tls {
                Some(tls) => match tls.accept(stream).await {
                    Ok(stream) => http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await,
                    Err(_) => return,
                },
                None => http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await,
            };
        });
    }
}

type Answer = hyper::Response<Full<Bytes>>;

async fn handle(req: hyper::Request<Incoming>, secrets: Arc<Secrets>, state: Arc<State>) -> Result<Answer, std::convert::Infallible> {
    let plain = |status: StatusCode, text: &'static str| {
        let mut r = hyper::Response::new(Full::new(Bytes::from_static(text.as_bytes())));
        *r.status_mut() = status;
        r
    };
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/healthz") => return Ok(plain(StatusCode::OK, "ok\n")),
        (&Method::POST, "/v1/ask") => {}
        _ => return Ok(plain(StatusCode::NOT_FOUND, "not found\n")),
    }
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let (timestamp, signature) = (header(TIMESTAMP), header(SIGNATURE));
    let path = req.uri().path().to_owned();
    let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(plain(StatusCode::PAYLOAD_TOO_LARGE, "too large\n")),
    };
    let (Some(timestamp), Some(signature)) = (timestamp, signature) else {
        return Ok(plain(StatusCode::UNAUTHORIZED, "unsigned\n"));
    };
    let secret = match secrets.verify(&timestamp, &signature, &path, &body, now()) {
        Ok(secret) => secret.to_vec(),
        Err(why) => {
            eprintln!("pg-auth-sidecar: refused a request: {why}");
            return Ok(plain(StatusCode::UNAUTHORIZED, "refused\n"));
        }
    };
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
        let signed = sign_request(B.as_bytes(), "1000", "/v1/ask", body);
        assert_eq!(secrets.verify("1000", &signed, "/v1/ask", body, 1010), Ok(B.as_bytes()), "either secret, while rotating");
        assert_eq!(secrets.verify("1000", &signed, "/v1/ask", br#"{"op":"list"}"#, 1010), Err("a wrong signature"), "body changed");
        assert_eq!(secrets.verify("1000", &signed, "/other", body, 1010), Err("a wrong signature"), "path changed");
        assert_eq!(secrets.verify("1001", &signed, "/v1/ask", body, 1010), Err("a wrong signature"), "timestamp changed");
        assert_eq!(secrets.verify("1000", &signed, "/v1/ask", body, 1031), Err("a timestamp too far from now"), "replayed later");
        let other = sign_request(b"not-one-of-the-secrets-the-sidecar-holds", "1000", "/v1/ask", body);
        assert_eq!(secrets.verify("1000", &other, "/v1/ask", body, 1000), Err("a wrong signature"));
        assert_eq!(secrets.verify("1000", "v1=zz", "/v1/ask", body, 1000), Err("a malformed signature"));
        assert_eq!(secrets.verify("1000", &signed.replace("v1=", "v2="), "/v1/ask", body, 1000), Err("a malformed signature"));
    }

    /// The same values the extension's tests check (src/sidecar.rs), computed independently: both
    /// ends must sign the same bytes.
    #[test]
    fn signatures_match_the_known_values() {
        assert_eq!(
            sign_request(A.as_bytes(), "1793350000", "/v1/ask", br#"{"op":"ping"}"#),
            "v1=cf7fdf96f7f1fa1155874c98dbe4fa3c88858340dfebb10b961fb74e23f00c9b"
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
