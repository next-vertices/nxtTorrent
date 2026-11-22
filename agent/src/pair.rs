//! Appairage unique : `agent pair <host:port> <CODE>`.
use crate::tls::{connect_channel, pinned_client_config};
use proto::fsutil::write_secret_atomic;
use proto::pairing::{ca_fingerprint, PairingCode};
use proto::pb::orchestrator_client::OrchestratorClient;
use proto::pb::PairRequest;
use rcgen::{CertificateParams, KeyPair};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("code d'appairage invalide")]
    BadCode,
    #[error("ce nœud est déjà appairé ({0} existe)")]
    AlreadyPaired(PathBuf),
    #[error("connexion/vérification TLS impossible: {0}")]
    Transport(String),
    #[error("appairage refusé par le master: {0}")]
    Rejected(String),
    #[error("la CA reçue ne correspond pas à l'empreinte du code")]
    CaMismatch,
    #[error("E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("génération de clé/CSR: {0}")]
    Keygen(#[from] rcgen::Error),
}

pub struct PairOptions {
    /// Remplace une identité existante (certificat expiré/corrompu, machine réinstallée).
    /// Le nœud reçoit une NOUVELLE identité ; l'ancienne se révoque côté master.
    pub force: bool,
    pub addr: String,
    pub code: String,
    pub data_dir: PathBuf,
    pub hostname: String,
    pub labels: Vec<String>,
}

#[derive(Debug)]
pub struct Paired {
    pub node_id: String,
}

pub fn identity_paths(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        dir.join("agent.key"),
        dir.join("agent.crt"),
        dir.join("ca.crt"),
        dir.join("node_id"),
    )
}

pub async fn pair(opts: PairOptions) -> Result<Paired, PairError> {
    let code = PairingCode::parse(&opts.code).map_err(|_| PairError::BadCode)?;
    let (key_p, crt_p, ca_p, id_p) = identity_paths(&opts.data_dir);
    if key_p.exists() && !opts.force {
        return Err(PairError::AlreadyPaired(key_p));
    }

    // La clé privée est générée localement et ne quitte jamais l'agent.
    let kp = KeyPair::generate()?;
    let csr = CertificateParams::new(Vec::<String>::new())?
        .serialize_request(&kp)?
        .pem()?;

    let chan = connect_channel(
        &opts.addr,
        pinned_client_config(code.ca_fp).map_err(|e| PairError::Transport(e.to_string()))?,
    )
    .await
    .map_err(|e| PairError::Transport(e.to_string()))?;
    let resp = OrchestratorClient::new(chan)
        .pair(PairRequest {
            code: opts.code.trim().to_owned(),
            csr_pem: csr,
            hostname: opts.hostname,
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            labels: opts.labels,
        })
        .await
        .map_err(|s| PairError::Rejected(s.message().to_owned()))?
        .into_inner();

    // Défense en profondeur : la CA stockée doit être celle qui a été épinglée.
    let ca_der = pem_first_der(&resp.ca_pem).ok_or(PairError::CaMismatch)?;
    if ca_fingerprint(&ca_der) != code.ca_fp {
        return Err(PairError::CaMismatch);
    }

    proto::fsutil::ensure_private_dir(&opts.data_dir)?;
    // Écritures atomiques ; la clé en dernier pour que `AlreadyPaired` ne soit vrai qu'une fois
    // le reste en place. Une identité forcée invalide l'état local lié à l'ancien nœud.
    write_secret_atomic(&ca_p, resp.ca_pem.as_bytes())?;
    write_secret_atomic(&id_p, resp.node_id.as_bytes())?;
    write_secret_atomic(&opts.data_dir.join("master_addr"), opts.addr.as_bytes())?;
    write_secret_atomic(&crt_p, resp.cert_pem.as_bytes())?;
    if opts.force {
        let _ = std::fs::remove_file(opts.data_dir.join("outbox.log"));
        let _ = std::fs::remove_file(opts.data_dir.join("applied_rev"));
    }
    write_secret_atomic(&key_p, kp.serialize_pem().as_bytes())?;
    Ok(Paired {
        node_id: resp.node_id,
    })
}

fn pem_first_der(pem: &str) -> Option<Vec<u8>> {
    let start = pem.find("-----BEGIN CERTIFICATE-----")? + "-----BEGIN CERTIFICATE-----".len();
    let end = pem[start..].find("-----END CERTIFICATE-----")? + start;
    let body: String = pem[start..end].split_whitespace().collect();
    data_encoding::BASE64.decode(body.as_bytes()).ok()
}
