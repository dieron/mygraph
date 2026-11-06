//! HTTPS. The server side: the listener's certificate, read again when its files change. The
//! client side: how this binary's own clients (the stdio MCP bridge and `mygraph health`) trust
//! a server, with the public roots and, for a private CA or a self-signed certificate, the
//! certificates in `MYGRAPH_CA_CERT`.
//!
//! The crypto provider is ring, for the server and the clients alike.

use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How often the server checks its certificate files for a change.
pub const RELOAD_EVERY: Duration = Duration::from_secs(60);

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Makes ring the process's default crypto provider; later calls do nothing.
pub fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------------------------------------------------------------------------------------------
// Server

/// The listener's TLS configuration from a PEM certificate chain and key.
pub async fn server_config(cert: &Path, key: &Path) -> io::Result<RustlsConfig> {
    install_provider();
    RustlsConfig::from_pem_file(cert, key).await.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "TLS certificate {} and key {}: {e}",
                cert.display(),
                key.display()
            ),
        )
    })
}

/// The acceptor of the HTTPS listener: [`PlainHttpCheck`], then the TLS handshake.
pub fn acceptor(config: RustlsConfig) -> RustlsAcceptor<PlainHttpCheck> {
    RustlsAcceptor::new(config).acceptor(PlainHttpCheck)
}

/// The first byte of a TLS connection: a handshake record.
const TLS_HANDSHAKE: u8 = 0x16;

/// [API-HTTP-4] Answers a plain HTTP request on the HTTPS port with `api.https_required`, instead
/// of the TLS alert that clients show as garbage, and closes the connection. TLS connections go
/// on to the handshake.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainHttpCheck;

impl<S: Send + 'static> Accept<TcpStream, S> for PlainHttpCheck {
    type Stream = TcpStream;
    type Service = S;
    type Future = Pin<Box<dyn Future<Output = io::Result<(TcpStream, S)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        Box::pin(async move {
            let mut first = [0u8; 1];
            let n = tokio::time::timeout(Duration::from_secs(10), stream.peek(&mut first))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "nothing received"))??;
            if n == 0 || first[0] == TLS_HANDSHAKE {
                return Ok((stream, service));
            }
            refuse_plain_http(stream).await;
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plain HTTP on the HTTPS port",
            ))
        })
    }
}

async fn refuse_plain_http(mut stream: TcpStream) {
    // Read what the client sent first: closing with unread data resets the connection, and the
    // client may then lose the answer.
    let mut buf = vec![0u8; 64 << 10];
    let mut read = 0;
    while read < buf.len() {
        match tokio::time::timeout(Duration::from_millis(200), stream.read(&mut buf[read..])).await
        {
            Ok(Ok(n)) if n > 0 => read += n,
            _ => break,
        }
    }
    let code = "api.https_required";
    let body = serde_json::json!({ "error": {
        "code": code,
        "message": "this server speaks HTTPS, and the request came over plain HTTP",
        "section": null, "index": null, "details": null,
        "hint": crate::error::hint_of(code),
    }})
    .to_string();
    let head = [
        "HTTP/1.1 400 Bad Request",
        "content-type: application/json; charset=utf-8",
        &format!("content-length: {}", body.len()),
        "connection: close",
    ]
    .join("\r\n");
    let reply = format!("{head}\r\n\r\n{body}");
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Reads the certificate and key again whenever either file changes (checked `every` so often),
/// so a renewed certificate is served without a restart. A pair that does not load (a renewal half written) keeps the
/// old one and is tried again at the next check.
pub fn spawn_reload(config: RustlsConfig, cert: PathBuf, key: PathBuf, every: Duration) {
    tokio::spawn(async move {
        let stamp = |p: &Path| -> Option<SystemTime> { std::fs::metadata(p).ok()?.modified().ok() };
        let mut loaded = (stamp(&cert), stamp(&key));
        loop {
            tokio::time::sleep(every).await;
            let now = (stamp(&cert), stamp(&key));
            if now == loaded {
                continue;
            }
            match config.reload_from_pem_file(&cert, &key).await {
                Ok(()) => {
                    tracing::info!(cert = %cert.display(), "TLS certificate reloaded");
                    loaded = now;
                }
                Err(e) => tracing::warn!(
                    cert = %cert.display(),
                    "TLS certificate changed but does not load, keeping the old one: {e}"
                ),
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Clients

static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// Sets up the HTTP client of this process, trusting the public roots and the certificates in
/// the PEM file `ca` (a private CA, or a self-signed server certificate). Call once, before
/// [`agent`]; without it the client trusts the public roots only.
pub fn init_client(ca: Option<&Path>) -> Result<(), String> {
    let agent = client(ca)?;
    AGENT
        .set(agent)
        .map_err(|_| "the HTTP client is already set up".to_owned())
}

/// The HTTP client of this process (see [`init_client`]).
pub fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| client(None).expect("the public roots load"))
}

/// A client trusting the public roots and the certificates in `ca`.
pub fn client(ca: Option<&Path>) -> Result<ureq::Agent, String> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    if let Some(path) = ca {
        let certs = CertificateDer::pem_file_iter(path)
            .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
            .map_err(|e| format!("CA certificates {}: {e}", path.display()))?;
        if certs.is_empty() {
            return Err(format!(
                "CA certificates {}: no certificate found",
                path.display()
            ));
        }
        for cert in certs {
            roots
                .add(cert)
                .map_err(|e| format!("CA certificates {}: {e}", path.display()))?;
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(ureq::AgentBuilder::new()
        .tls_config(Arc::new(config))
        .build())
}

/// A client error, with what to do for the self-signed certificates that TLS refuses most often.
pub fn explain(error: &str) -> String {
    if error.contains("CaUsedAsEndEntity") {
        format!(
            "{error} (the server's certificate is marked as a CA; make one with              basicConstraints CA:FALSE, or issue it from a CA such as mkcert's)"
        )
    } else {
        error.to_owned()
    }
}

/// A client that accepts any server certificate. Only for `mygraph health` asking the server
/// on this machine whether it is up: it sends nothing secret, and the certificate is usually
/// issued for a name other than the loopback address.
pub fn loopback_client() -> ureq::Agent {
    let provider = provider();
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    ureq::AgentBuilder::new()
        .tls_config(Arc::new(config))
        .build()
}

/// Accepts any certificate, still checking that the server holds its key.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
