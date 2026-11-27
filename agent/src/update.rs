//! Mise à jour de l'agent : demandée au master par la connexion mTLS, vérifiée ICI avec les clés compilées dans l'agent,
//! puis déposée pour le service root (`proto::install`) qui revérifie tout et installe.
use crate::tls::connect_channel;
use crate::tunnel::{client_config, load_identity};
use proto::install::{read_bounded, App, Layout};
use proto::pb::orchestrator_client::OrchestratorClient;
use proto::pb::{UpdateRequest, UpdateStatus};
use proto::release::{self, TrustedKey, Verified};
use proto::version::Version;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Durée maximale d'une mise à jour, tentatives comprises (égale à la fenêtre d'attente du master).
const TIMEOUT: Duration = Duration::from_secs(600);
/// Sans nouvelle donnée pendant ce délai, la connexion est considérée comme perdue : une coupure ne parvient pas
/// toujours au nœud, qui attendrait indéfiniment.
const STALL: Duration = Duration::from_secs(30);
const ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(5);
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
    #[error("transfert bloqué : plus aucune donnée du master")]
    Stalled,
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
    let attempts = retry(ATTEMPTS, RETRY_DELAY, || {
        fetch_inner(data_dir, version, installed, keys)
    });
    tokio::time::timeout(TIMEOUT, attempts)
        .await
        .map_err(|_| UpdateError::Transfer)?
}

/// Recommence, sur une connexion neuve, après une panne de réseau. Un refus du master ou une release invalide
/// sont définitifs.
async fn retry<T, F, Fut>(attempts: u32, delay: Duration, mut run: F) -> Result<T, UpdateError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, UpdateError>>,
{
    let mut attempt = 1;
    loop {
        match run().await {
            Err(error @ (UpdateError::Connect | UpdateError::Transfer | UpdateError::Stalled))
                if attempt < attempts =>
            {
                tracing::warn!(%error, attempt, attempts, "téléchargement de la mise à jour : nouvel essai");
                attempt += 1;
                tokio::time::sleep(delay).await;
            }
            other => return other,
        }
    }
}

/// Une étape réseau du téléchargement, bornée par `limit`.
async fn within<T>(limit: Duration, step: impl Future<Output = T>) -> Result<T, UpdateError> {
    tokio::time::timeout(limit, step)
        .await
        .map_err(|_| UpdateError::Stalled)
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
    tracing::info!(version, "téléchargement de la mise à jour depuis le master");
    let channel = within(STALL, connect_channel(&id.addr, tls))
        .await?
        .map_err(|_| UpdateError::Connect)?;
    let mut client = OrchestratorClient::new(channel);
    let request = client.fetch_update(UpdateRequest {
        version: version.to_owned(),
        platform: platform.to_owned(),
    });
    let mut stream = within(STALL, request)
        .await?
        .map_err(|s| UpdateError::Refused(s.message().to_owned()))?
        .into_inner();
    let head = within(STALL, stream.message())
        .await?
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
    loop {
        let next = within(STALL, stream.message()).await.and_then(|m| {
            m.map_err(|status| {
                tracing::warn!(%status, "flux de la mise à jour en erreur");
                UpdateError::Transfer
            })
        });
        let chunk = match next {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    received = binary.len(),
                    expected = size,
                    "téléchargement incomplet"
                );
                return Err(e);
            }
        };
        if binary.len() as u64 + chunk.data.len() as u64 > size {
            return Err(UpdateError::Verify("plus d'octets qu'annoncé".to_owned()));
        }
        binary.extend_from_slice(&chunk.data);
    }
    // Flux terminé trop tôt : panne de réseau (nouvel essai), pas release invalide.
    if (binary.len() as u64) < size {
        tracing::warn!(
            received = binary.len(),
            expected = size,
            "téléchargement incomplet"
        );
        return Err(UpdateError::Transfer);
    }
    verified.check_asset(&asset, &binary).map_err(verify_err)?;
    tracing::info!(
        version,
        bytes = binary.len(),
        "mise à jour reçue et vérifiée"
    );
    Ok(Downloaded {
        manifest: head.manifest,
        signature: head.signature,
        binary,
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn tries(fail: fn() -> UpdateError, failures: u32) -> (bool, u32) {
        let n = AtomicU32::new(0);
        let r = retry(3, Duration::ZERO, || {
            let i = n.fetch_add(1, Ordering::SeqCst);
            async move {
                if i < failures {
                    Err(fail())
                } else {
                    Ok(())
                }
            }
        })
        .await;
        (r.is_ok(), n.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn network_failures_are_retried_a_bounded_number_of_times() {
        assert_eq!(tries(|| UpdateError::Stalled, 2).await, (true, 3));
        assert_eq!(tries(|| UpdateError::Connect, 9).await, (false, 3));
    }

    #[tokio::test]
    async fn refusals_and_invalid_releases_are_final() {
        let refused = || UpdateError::Refused("non".to_owned());
        assert_eq!(tries(refused, 9).await, (false, 1));
        let invalid = || UpdateError::Verify("signature".to_owned());
        assert_eq!(tries(invalid, 9).await, (false, 1));
    }

    #[tokio::test]
    async fn a_silent_step_is_reported_as_stalled() {
        let r = within(Duration::from_millis(20), std::future::pending::<()>()).await;
        assert!(matches!(r, Err(UpdateError::Stalled)));
    }
}
