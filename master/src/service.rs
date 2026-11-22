//! Service gRPC : `Pair` (sans certificat client) ; `Connect` arrive en phase 2.
use crate::tlsaccept::{self, ConnInfo};
use crate::{store::NodeRecord, Core};
use proto::pairing::{secret_hash, PairingCode};
use proto::pb::orchestrator_server::{Orchestrator, OrchestratorServer};
use proto::pb::{AgentMsg, MasterMsg, PairRequest, PairResponse};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

const MAX_CSR: usize = 4096;
const MAX_CODE: usize = 128;
const MAX_HOSTNAME: usize = 253;
const MAX_LABELS: usize = 16;
const MAX_LABEL_LEN: usize = 64;
const MAX_VERSION: usize = 32;

pub struct Svc {
    core: Arc<Core>,
}

impl Svc {
    pub fn new(core: Arc<Core>) -> Self {
        Self { core }
    }
}

fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= MAX_LABEL_LEN
        && l.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Validation stricte d'une demande d'appairage (un agent compromis ne doit pas pouvoir nuire).
fn validate_pair(r: &PairRequest) -> Result<(), Status> {
    let bad = |m: &str| Err(Status::invalid_argument(m.to_owned()));
    if r.code.is_empty() || r.code.len() > MAX_CODE {
        return bad("code invalide");
    }
    if r.csr_pem.is_empty() || r.csr_pem.len() > MAX_CSR {
        return bad("csr invalide");
    }
    if r.hostname.is_empty()
        || r.hostname.len() > MAX_HOSTNAME
        || r.hostname.chars().any(char::is_control)
    {
        return bad("hostname invalide");
    }
    if r.agent_version.is_empty()
        || r.agent_version.len() > MAX_VERSION
        || r.agent_version.chars().any(char::is_control)
    {
        return bad("agent_version invalide");
    }
    if r.labels.len() > MAX_LABELS || !r.labels.iter().all(|l| valid_label(l)) {
        return bad("labels invalides");
    }
    Ok(())
}

type ConnectStream = Pin<Box<dyn Stream<Item = Result<MasterMsg, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Orchestrator for Svc {
    type ConnectStream = ConnectStream;

    async fn connect(
        &self,
        req: Request<Streaming<AgentMsg>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        // L'identité vient EXCLUSIVEMENT du SAN du certificat client validé par TLS.
        let leaf = req
            .extensions()
            .get::<ConnInfo>()
            .and_then(|i| i.peer_certs.as_ref())
            .and_then(|c| c.first().cloned())
            .ok_or_else(|| Status::unauthenticated("certificat client requis"))?;
        let cert = proto::certinfo::parse_der(leaf.as_ref());
        let Some((info, node_id)) = cert.and_then(|c| c.node_id.clone().map(|n| (c, n))) else {
            self.core
                .store
                .audit("anonymous", "mtls_rejected", "SAN node-<uuid> absent");
            return Err(Status::unauthenticated(
                "identité de nœud absente du certificat",
            ));
        };
        // Adresse source du tunnel : affichée à l'admin (aide à repérer un nœud), jamais utilisée pour décider.
        if let Some(ip) = req
            .extensions()
            .get::<ConnInfo>()
            .and_then(|i| i.remote)
            .map(|a| a.ip())
        {
            let _ = self.core.store.set_node_ip(&node_id, &ip.to_string());
            // Adresse sous laquelle les autres nœuds contacteront celui-ci pour la réplication.
            if let Some(sw) = self.core.swarm() {
                sw.tracker.set_node_ip(&node_id, ip);
            }
        }
        let inbound = req.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(crate::tunnel::run_session(
            self.core.clone(),
            node_id,
            info,
            inbound,
            tx,
        ));
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }

    async fn pair(&self, req: Request<PairRequest>) -> Result<Response<PairResponse>, Status> {
        let ip: Option<IpAddr> = req
            .extensions()
            .get::<ConnInfo>()
            .and_then(|i| i.remote)
            .map(|a: SocketAddr| a.ip());
        let peer = ip.map_or_else(|| "?".to_owned(), |i| i.to_string());
        if let Some(ip) = ip {
            if self.core.pair_limiter.is_blocked(ip) {
                self.core
                    .store
                    .audit("anonymous", "pair_rate_limited", &peer);
                return Err(Status::resource_exhausted(
                    "trop d'échecs, réessayez plus tard",
                ));
            }
        }
        let r = req.into_inner();
        validate_pair(&r)?;

        let reject = |why: &str| {
            if let Some(ip) = ip {
                self.core.pair_limiter.record_failure(ip);
            }
            self.core
                .store
                .audit("anonymous", "pair_rejected", &format!("{peer}: {why}"));
            tracing::warn!(peer = %peer, reason = why, "appairage refusé");
            Status::unauthenticated("appairage refusé")
        };

        let Ok(code) = PairingCode::parse(&r.code) else {
            return Err(reject("format de code"));
        };
        let grant = self
            .core
            .store
            .consume_code(&secret_hash(&code.secret), crate::store::unix_now())
            .map_err(|e| {
                tracing::error!(error = %e, "base de données");
                Status::internal("erreur interne")
            })?;
        let Some(grant) = grant else {
            return Err(reject("code inconnu, expiré ou déjà utilisé"));
        };
        let labels = grant.labels;
        if !r.labels.is_empty() && r.labels != labels {
            // Les labels déclarés par l'agent sont informatifs : seuls ceux du code comptent.
            tracing::info!("labels déclarés par l'agent ignorés");
        }

        let node_id = uuid::Uuid::new_v4().to_string();
        let cert_pem = self
            .core
            .ca
            .sign_node_csr(&r.csr_pem, &node_id, self.core.settings.cert_validity)
            .map_err(|e| {
                tracing::warn!(error = %e, "CSR refusé");
                Status::invalid_argument("csr invalide")
            })?;
        self.core
            .store
            .insert_node(&NodeRecord {
                id: node_id.clone(),
                hostname: r.hostname.clone(),
                agent_version: r.agent_version.clone(),
                labels: labels.clone(),
                name: grant.name,
            })
            .map_err(|e| {
                tracing::error!(error = %e, "base de données");
                Status::internal("erreur interne")
            })?;
        self.core
            .store
            .audit("anonymous", "node_paired", &format!("{node_id} {peer}"));
        tracing::info!(node_id = %node_id, peer = %peer, "nœud appairé");
        Ok(Response::new(PairResponse {
            cert_pem,
            ca_pem: self.core.ca.cert_pem.clone(),
            node_id,
        }))
    }
}

/// Listener agents : TLS obligatoire ; certificat client optionnel au niveau TLS
/// (requis par `Connect`, absent pour `Pair`).
pub async fn serve(
    core: Arc<Core>,
    listener: tokio::net::TcpListener,
    sans: &[String],
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (chain, key) = core.ca.server_identity(sans)?;
    let acceptor = tlsaccept::acceptor(&chain, &key, &core.ca.cert_pem)?;
    // La poignée de main TLS est faite ICI (pas par tonic) pour pouvoir auditer ses échecs.
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let accept = tokio::spawn(tlsaccept::accept_loop(core.clone(), listener, acceptor, tx));
    let res = Server::builder()
        .add_service(
            OrchestratorServer::new(Svc::new(core)).max_decoding_message_size(8 * 1024 * 1024),
        )
        .serve_with_incoming_shutdown(tokio_stream::wrappers::ReceiverStream::new(rx), shutdown)
        .await;
    accept.abort();
    res?;
    Ok(())
}
