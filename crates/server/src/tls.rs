//! Native TLS (rustls) for the server, with certificate hot-reload.
//!
//! [`TlsListener`] implements `axum::serve::Listener`. A background task
//! accepts TCP connections and runs TLS handshakes concurrently, each bounded
//! by [`HANDSHAKE_TIMEOUT`]. Only finished handshakes are handed to axum, so a
//! slow or malicious client cannot stall the accept loop.
//!
//! The certificate is served by a [`ReloadableCert`] resolver. `reload()`
//! swaps it atomically, and new handshakes see the new certificate, while
//! existing connections keep theirs. The server calls this on SIGHUP.

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tracing::{debug, warn};

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Loads a PEM certificate chain and private key into a `CertifiedKey`.
pub fn load_certified_key(cert: &Path, key: &Path) -> Result<CertifiedKey, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("{}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert.display()))?;
    if chain.is_empty() {
        return Err(format!("{}: no certificates found", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{}: {e}", key.display()))?;
    let provider = rustls::crypto::ring::default_provider();
    CertifiedKey::from_der(chain, key, &provider)
        .map_err(|e| format!("certificate/key mismatch or unsupported key: {e}"))
}

/// A certificate resolver whose certificate can be replaced at runtime.
#[derive(Debug)]
pub struct ReloadableCert {
    cert_path: PathBuf,
    key_path: PathBuf,
    current: RwLock<Arc<CertifiedKey>>,
}

impl ReloadableCert {
    pub fn load(cert_path: PathBuf, key_path: PathBuf) -> Result<Arc<ReloadableCert>, String> {
        let ck = load_certified_key(&cert_path, &key_path)?;
        Ok(Arc::new(ReloadableCert { cert_path, key_path, current: RwLock::new(Arc::new(ck)) }))
    }

    /// Re-reads the files. On error the old certificate stays in use.
    pub fn reload(&self) -> Result<(), String> {
        let ck = load_certified_key(&self.cert_path, &self.key_path)?;
        let mut g = self.current.write().map_err(|_| "poisoned".to_string())?;
        *g = Arc::new(ck);
        Ok(())
    }
}

impl ResolvesServerCert for ReloadableCert {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.read().ok().map(|g| g.clone())
    }
}

/// Builds the rustls acceptor (TLS 1.2+ and ALPN h2 + http/1.1).
pub fn acceptor(cert: Arc<ReloadableCert>) -> Result<TlsAcceptor, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(cert);
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(cfg)))
}

/// A listener yielding completed TLS connections.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, acceptor: TlsAcceptor) -> std::io::Result<TlsListener> {
        let local = tcp.local_addr()?;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let (sock, addr) = match tcp.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        // EMFILE etc.: back off instead of spinning.
                        warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let _ = sock.set_nodelay(true);
                if tx.is_closed() {
                    return;
                }
                let acc = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acc.accept(sock)).await {
                        Ok(Ok(s)) => {
                            let _ = tx.send((s, addr)).await;
                        }
                        Ok(Err(e)) => debug!(%addr, error = %e, "TLS handshake failed"),
                        Err(_) => debug!(%addr, "TLS handshake timed out"),
                    }
                });
            }
        });
        Ok(TlsListener { rx, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(x) => x,
            // The accept task only ends with the runtime; park forever.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}
