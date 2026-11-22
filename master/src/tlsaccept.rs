//! Acceptation TLS faite par le master (et non par tonic) : c'est la seule façon de JOURNALISER les
//! échecs de poignée de main mTLS (certificat expiré, CA inconnue, scan non-TLS…), qui sinon se
//! produisent dans la couche TLS avant tout code applicatif.
use crate::Core;
use rustls::crypto::ring;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;
use tonic::transport::server::Connected;

/// Poignées de main TLS simultanées : au-delà, les nouvelles connexions sont refusées (anti-saturation).
const MAX_PENDING_HANDSHAKES: usize = 256;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Informations de connexion transmises aux handlers gRPC (via les extensions de requête).
#[derive(Clone, Debug)]
pub struct ConnInfo {
    pub remote: Option<SocketAddr>,
    /// Chaîne de certificats client présentée ET validée par TLS (vide si le client n'en a pas présenté).
    pub peer_certs: Option<Arc<Vec<CertificateDer<'static>>>>,
}

pub struct TlsConn {
    inner: TlsStream<TcpStream>,
    info: ConnInfo,
}

impl Connected for TlsConn {
    type ConnectInfo = ConnInfo;
    fn connect_info(&self) -> ConnInfo {
        self.info.clone()
    }
}

impl AsyncRead for TlsConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Configuration TLS : certificat serveur signé par notre CA ; certificat client FACULTATIF au niveau
/// TLS (requis par `Connect`, absent pour `Pair`) mais, s'il est présenté, validé contre la CA.
pub fn acceptor(chain_pem: &str, key_pem: &str, ca_pem: &str) -> anyhow::Result<TlsAcceptor> {
    let provider = Arc::new(ring::default_provider());
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c?)?;
    }
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .allow_unauthenticated()
        .build()?;
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(chain_pem.as_bytes()).collect::<Result<_, _>>()?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())?;
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)?;
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(cfg)))
}

/// Raison d'échec nettoyée avant d'entrer dans l'audit (longueur, caractères de contrôle).
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(160).collect()
}

fn audit_failure(core: &Core, peer: SocketAddr, reason: &str) {
    // Un scanner peut ouvrir des milliers de connexions : une seule entrée par IP et par fenêtre.
    if core.mtls_limiter.is_blocked(peer.ip()) {
        return;
    }
    core.mtls_limiter.record_failure(peer.ip());
    core.store.audit(
        "anonymous",
        "mtls_failed",
        &format!("{}: {}", peer.ip(), clean(reason)),
    );
    tracing::warn!(peer = %peer.ip(), reason = %clean(reason), "poignée de main mTLS refusée");
}

/// Accepte les connexions TCP, fait la poignée de main TLS, audite les échecs et transmet les flux
/// réussis (avec leurs informations de connexion) au serveur gRPC.
pub async fn accept_loop(
    core: Arc<Core>,
    listener: tokio::net::TcpListener,
    acceptor: TlsAcceptor,
    out: mpsc::Sender<Result<TlsConn, std::io::Error>>,
) {
    let slots = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "accept() a échoué");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue; // trop de poignées de main en cours : on abandonne cette connexion
        };
        let (acc, out, core) = (acceptor.clone(), out.clone(), core.clone());
        tokio::spawn(async move {
            let _slot = slot;
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, acc.accept(tcp)).await {
                Ok(Ok(tls)) => {
                    let peer_certs = tls.get_ref().1.peer_certificates().map(|c| {
                        Arc::new(c.iter().map(|x| x.clone().into_owned()).collect::<Vec<_>>())
                    });
                    let conn = TlsConn {
                        inner: tls,
                        info: ConnInfo {
                            remote: Some(peer),
                            peer_certs,
                        },
                    };
                    let _ = out.send(Ok(conn)).await;
                }
                // Connexion TCP fermée sans un octet (sonde de supervision, test de port) : ni attaque ni erreur.
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    tracing::debug!(peer = %peer.ip(), "connexion fermée avant la poignée de main TLS");
                }
                Ok(Err(e)) => audit_failure(&core, peer, &e.to_string()),
                Err(_) => audit_failure(&core, peer, "délai de la poignée de main TLS dépassé"),
            }
        });
    }
}
