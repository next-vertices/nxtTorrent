//! Réception d'une mise à jour : demandée au master par la connexion mTLS, vérifiée ICI avec les clés compilées dans
//! l'agent. Rien n'est installé par ce module.
use crate::tls::connect_channel;
use crate::tunnel::{client_config, load_identity};
use proto::pb::orchestrator_client::OrchestratorClient;
use proto::pb::UpdateRequest;
use proto::release::{self, TrustedKey, Verified};
use proto::version::Version;
use std::path::Path;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(600);
const MAX_BINARY: u64 = 128 * 1024 * 1024;

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
