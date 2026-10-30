//! The sidecar's connections to Postgres (PG_AUTH_DATABASE_URL), encrypted as libpq makes them,
//! from the URL's `sslmode`: `disable`; `prefer` (the default: TLS when the server offers it) and
//! `require` (always TLS), neither checking the certificate, as libpq does without a root file;
//! `verify-ca`, the chain checked against `sslrootcert` or else the public roots; `verify-full`,
//! the server's name too. tokio-postgres itself takes only disable, prefer and require.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};
use tokio_postgres::{Client, Config, Connection, Socket};
use tokio_rustls::rustls::client::WebPkiServerVerifier;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{self, CertificateError, DigitallySignedStruct, SignatureScheme};

/// What the server's certificate is checked for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Check {
    Nothing,
    Chain,
    Name,
}

/// Connect to PG_AUTH_DATABASE_URL: the client, and the connection to poll (for notifications).
pub async fn connect(url: &str) -> Result<(Client, Connection<Socket, Stream>), String> {
    let (config, check, root) = parse(url)?;
    let tls = Tls(Arc::new(client_config(check, root.as_deref())?));
    // with its causes: a refused certificate is only in the handshake error's source
    config.connect(tls).await.map_err(|e| format!("postgres: {}", crate::engine::causes(&e)))
}

/// The URL as tokio-postgres takes it, the check its `sslmode` asks for, and its `sslrootcert`.
fn parse(url: &str) -> Result<(Config, Check, Option<String>), String> {
    let (rest, params, url_form) = split(url);
    let (mut mode, mut root, mut kept) = (String::from("prefer"), None, Vec::new());
    for (key, raw) in params {
        match key.as_str() {
            "sslmode" => mode = decoded(&raw, url_form),
            "sslrootcert" => root = Some(decoded(&raw, url_form)),
            _ => kept.push(format!("{key}={raw}")),
        }
    }
    let (ssl, check) = match mode.as_str() {
        "disable" => (SslMode::Disable, Check::Nothing),
        "allow" | "prefer" => (SslMode::Prefer, Check::Nothing),
        "require" => (SslMode::Require, Check::Nothing),
        "verify-ca" => (SslMode::Require, Check::Chain),
        "verify-full" => (SslMode::Require, Check::Name),
        other => return Err(format!("PG_AUTH_DATABASE_URL: sslmode {other} is not disable, prefer, require, verify-ca or verify-full")),
    };
    let joined = match (url_form, kept.is_empty()) {
        (true, true) => rest,
        (true, false) => format!("{rest}?{}", kept.join("&")),
        (false, _) => [rest].into_iter().chain(kept).collect::<Vec<_>>().join(" "),
    };
    let mut config: Config = joined.parse().map_err(|e| format!("PG_AUTH_DATABASE_URL: {e}"))?;
    config.ssl_mode(ssl);
    Ok((config, check, root))
}

/// A URL (`postgres://...?a=b&c=d`) or key=value pairs (`host=... sslmode=...`): what is not a
/// parameter, the parameters (raw), and which form it was.
fn split(url: &str) -> (String, Vec<(String, String)>, bool) {
    let pair = |p: &str| p.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned()));
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        (base.to_owned(), query.split('&').filter(|p| !p.is_empty()).filter_map(pair).collect(), true)
    } else {
        let (params, rest): (Vec<&str>, Vec<&str>) =
            url.split_whitespace().partition(|t| t.starts_with("sslmode=") || t.starts_with("sslrootcert="));
        (rest.join(" "), params.into_iter().filter_map(pair).collect(), false)
    }
}

/// A URL's percent-encoding undone (`%2F` in a root file's path); key=value values are as written.
fn decoded(raw: &str, url_form: bool) -> String {
    if !url_form {
        return raw.to_owned();
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn client_config(check: Check, root: Option<&str>) -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier: Arc<dyn ServerCertVerifier> = match check {
        Check::Nothing => Arc::new(Unchecked(provider.clone())),
        Check::Chain | Check::Name => {
            let mut roots = rustls::RootCertStore::empty();
            match root {
                Some(path) => {
                    let certs: Vec<CertificateDer<'static>> =
                        CertificateDer::pem_file_iter(path).and_then(Iterator::collect).map_err(|e| format!("sslrootcert {path}: {e}"))?;
                    let (added, _) = roots.add_parsable_certificates(certs);
                    if added == 0 {
                        return Err(format!("sslrootcert {path}: no certificate in it"));
                    }
                }
                None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
            }
            let webpki =
                WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().map_err(|e| e.to_string())?;
            if check == Check::Name { webpki } else { Arc::new(AnyName(webpki)) }
        }
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// `prefer` and `require`: the certificate is not checked (libpq's behaviour without a root file);
/// the handshake's own signatures still are.
#[derive(Debug)]
struct Unchecked(Arc<CryptoProvider>);

impl ServerCertVerifier for Unchecked {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `verify-ca`: the chain checked, whatever name the certificate is for.
#[derive(Debug)]
struct AnyName(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for AnyName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.0.verify_server_cert(end_entity, intermediates, name, ocsp, now) {
            Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. })) => {
                Ok(ServerCertVerified::assertion())
            }
            checked => checked,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// tokio-postgres's TLS hook, over rustls.
#[derive(Clone)]
struct Tls(Arc<rustls::ClientConfig>);

impl MakeTlsConnect<Socket> for Tls {
    type Stream = Stream;
    type TlsConnect = Connect;
    type Error = io::Error;

    // a Unix socket's host is "": no name, and no TLS on it (the server refuses it)
    fn make_tls_connect(&mut self, domain: &str) -> Result<Connect, io::Error> {
        Ok(Connect { config: self.0.clone(), name: ServerName::try_from(domain.to_owned()).ok() })
    }
}

pub struct Connect {
    config: Arc<rustls::ClientConfig>,
    name: Option<ServerName<'static>>,
}

impl TlsConnect<Socket> for Connect {
    type Stream = Stream;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Stream>> + Send>>;

    fn connect(self, socket: Socket) -> Self::Future {
        Box::pin(async move {
            let name = self.name.ok_or_else(|| io::Error::other("TLS needs the server's host name"))?;
            tokio_rustls::TlsConnector::from(self.config).connect(name, socket).await.map(Stream)
        })
    }
}

pub struct Stream(tokio_rustls::client::TlsStream<Socket>);

impl tokio_postgres::tls::TlsStream for Stream {
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}

impl AsyncRead for Stream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(url: &str) -> (SslMode, Check, Option<String>) {
        let (config, check, root) = parse(url).unwrap();
        (config.get_ssl_mode(), check, root)
    }

    #[test]
    fn sslmode_reads_as_libpq_reads_it() {
        assert_eq!(mode("postgres://u@h/db"), (SslMode::Prefer, Check::Nothing, None));
        assert_eq!(mode("postgres://u@h/db?sslmode=disable"), (SslMode::Disable, Check::Nothing, None));
        assert_eq!(mode("postgres://u@h/db?sslmode=require&application_name=x"), (SslMode::Require, Check::Nothing, None));
        assert_eq!(
            mode("postgres://u@h/db?sslmode=verify-full&sslrootcert=%2Fetc%2Fca.pem"),
            (SslMode::Require, Check::Name, Some("/etc/ca.pem".into()))
        );
        assert_eq!(
            mode("host=/run/postgresql user=postgres sslmode=verify-ca sslrootcert=/etc/ca.pem"),
            (SslMode::Require, Check::Chain, Some("/etc/ca.pem".into()))
        );
        assert!(parse("postgres://u@h/db?sslmode=sometimes").err().unwrap().contains("sslmode sometimes"));
        // what is not about TLS goes on to tokio-postgres
        let (config, _, _) = parse("postgres://u@h/db?sslmode=verify-full&application_name=pg_auth").unwrap();
        assert_eq!(config.get_application_name(), Some("pg_auth"));
    }
}
