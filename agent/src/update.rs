//! Mise à jour de l'agent : demandée au master par la connexion mTLS, vérifiée ICI avec les clés compilées dans l'agent,
//! puis déposée pour le service root (`proto::install`) qui revérifie tout et installe.
use crate::tls::connect_channel;
use crate::tunnel::{client_config, load_identity};
use proto::install::{read_bounded, App, Layout};
use proto::pb::orchestrator_client::OrchestratorClient;
use proto::pb::{UpdateRequest, UpdateStatus};
use proto::release::{self, TrustedKey, Verified};
use proto::version::Version;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(600);
const MAX_BINARY: u64 = 128 * 1024 * 1024;
const STATE_DIR: &str = "/var/lib/nxt-agent-update";

/// L'agent, pour l'installateur commun. « Sain » = reconnecté au master (cf. tunnel).
pub const APP: App = App {
    binary_name: "nxt-agent",
    state_dir: STATE_DIR,
    env: "AGENT_SELF_UPDATE",
    restore_command: None,
    health_timeout: Duration::from_secs(180),
};

/// Présent quand un installateur est en place sur ce nœud.
#[derive(Clone, Debug)]
pub struct SelfUpdate {
    pub keys: Vec<TrustedKey>,
    /// Résultat écrit par le service root.
    pub status_file: PathBuf,
    /// Durée de connexion au master après laquelle cette version se déclare saine.
    pub healthy_after: Duration,
}

impl SelfUpdate {
    pub fn production() -> Self {
        Self {
            keys: release::TRUSTED_KEYS.to_vec(),
            status_file: Path::new(STATE_DIR).join("status.json"),
            healthy_after: Duration::from_secs(15),
        }
    }
}

/// Dépose une mise à jour vérifiée ; le fichier `request`, écrit en dernier, réveille le service root.
pub fn stage(data_dir: &Path, d: &Downloaded) -> std::io::Result<()> {
    let l = Layout::new(data_dir);
    let _ = std::fs::remove_dir_all(l.staged());
    std::fs::create_dir_all(l.staged())?;
    std::fs::write(l.manifest(), &d.manifest)?;
    std::fs::write(l.signature(), &d.signature)?;
    std::fs::write(l.binary(APP.binary_name), &d.binary)?;
    std::fs::write(l.request(), format!("{}\n", d.verified.version))
}

/// Marqueur attendu par le service root.
pub fn mark_healthy(data_dir: &Path) {
    let l = Layout::new(data_dir);
    let r = std::fs::create_dir_all(&l.dir)
        .and_then(|()| std::fs::write(l.healthy(), concat!(env!("CARGO_PKG_VERSION"), "\n")));
    if let Err(e) = r {
        tracing::warn!(error = %e, "marqueur de bonne santé non écrit");
    }
}

/// Dernier résultat du service root, à rapporter au master.
pub fn last_result(status_file: &Path) -> Option<UpdateStatus> {
    let b = read_bounded(status_file, 8 * 1024).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
    Some(UpdateStatus {
        version: v["version"].as_str()?.to_owned(),
        state: v["state"].as_str()?.to_owned(),
        reason: v["reason"].as_str().unwrap_or_default().to_owned(),
        ts: v["ts"].as_i64()?,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("identité de l'agent : {0}")]
    Identity(String),
    #[error("master injoignable")]
    Connect,
    #[error("le master refuse : {0}")]
    Refused(String),
    #[error("transfert interrompu")]
    Transfer,
    #[error("release refusée : {0}")]
    Verify(String),
}

/// Une mise à jour reçue ET vérifiée (signature, version plus récente, taille et empreinte).
#[derive(Debug)]
pub struct Downloaded {
    pub manifest: Vec<u8>,
    pub signature: Vec<u8>,
    pub binary: Vec<u8>,
    pub verified: Verified,
}

fn verify_err(e: release::ReleaseError) -> UpdateError {
    UpdateError::Verify(e.to_string())
}

/// Demande la version `version` au master. `keys` : les clés compilées (`release::TRUSTED_KEYS`) en production.
pub async fn fetch(
    data_dir: &Path,
    version: &str,
    installed: &Version,
    keys: &[TrustedKey],
) -> Result<Downloaded, UpdateError> {
    tokio::time::timeout(TIMEOUT, fetch_inner(data_dir, version, installed, keys))
        .await
        .map_err(|_| UpdateError::Transfer)?
}

async fn fetch_inner(
    data_dir: &Path,
    version: &str,
    installed: &Version,
    keys: &[TrustedKey],
) -> Result<Downloaded, UpdateError> {
    let platform = proto::platform()
        .ok_or_else(|| UpdateError::Verify("plateforme sans release".to_owned()))?;
    let asset = format!(
        "nxt-agent-{platform}{}",
        if cfg!(windows) { ".exe" } else { "" }
    );
    let id = load_identity(data_dir).map_err(|e| UpdateError::Identity(e.to_string()))?;
    let tls = client_config(&id).map_err(UpdateError::Identity)?;
    let channel = connect_channel(&id.addr, tls)
        .await
        .map_err(|_| UpdateError::Connect)?;
    let mut stream = OrchestratorClient::new(channel)
        .fetch_update(UpdateRequest {
            version: version.to_owned(),
            platform: platform.to_owned(),
        })
        .await
        .map_err(|s| UpdateError::Refused(s.message().to_owned()))?
        .into_inner();
    let head = stream
        .message()
        .await
        .map_err(|_| UpdateError::Transfer)?
        .ok_or(UpdateError::Transfer)?;
    // Le manifeste est authentifié AVANT de recevoir le binaire.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let verified =
        release::verify_with(keys, &head.manifest, &head.signature, now).map_err(verify_err)?;
    if verified.tag != format!("v{version}") {
        return Err(UpdateError::Verify("autre version que demandée".to_owned()));
    }
    verified.require_newer_than(installed).map_err(verify_err)?;
    let size = verified
        .asset(&asset)
        .map(|a| a.size)
        .filter(|s| *s <= MAX_BINARY)
        .ok_or_else(|| UpdateError::Verify("binaire absent ou démesuré".to_owned()))?;
    let mut binary = head.data;
    while let Some(chunk) = stream.message().await.map_err(|_| UpdateError::Transfer)? {
        if binary.len() as u64 + chunk.data.len() as u64 > size {
            return Err(UpdateError::Verify("plus d'octets qu'annoncé".to_owned()));
        }
        binary.extend_from_slice(&chunk.data);
    }
    verified.check_asset(&asset, &binary).map_err(verify_err)?;
    Ok(Downloaded {
        manifest: head.manifest,
        signature: head.signature,
        binary,
        verified,
    })
}
