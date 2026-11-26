//! Binaires d'agent à distribuer : le master télécharge ceux de SA PROPRE version, les vérifie, les garde en cache et
//! les sert aux nœuds par la connexion mTLS (`FetchUpdate`). L'agent ne contacte jamais GitHub, et revérifie tout lui-même :
//! le master n'est qu'un relais.
//!
//! La préparation (seul appel sortant) est déclenchée par un administrateur, jamais par la demande d'un nœud.
use crate::selfupdate::{download, read_bounded, DOWNLOAD_BASE};
use crate::store::unix_now;
use crate::Core;
use proto::release::{self, TrustedKey, MAX_MANIFEST_BYTES, SIGNATURE_BYTES};
use proto::version::Version;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;

const MAX_AGENT_BINARY: u64 = 128 * 1024 * 1024;
/// Durée pendant laquelle l'absence d'un nœud qui se met à jour est attendue.
const UPDATE_WINDOW_SECS: i64 = 600;
/// Transferts simultanés vers les nœuds.
const MAX_TRANSFERS: usize = 4;

/// Réglages. En production : [`DistConfig::production`] ; les tests injectent adresse, clés et version.
#[derive(Clone)]
pub struct DistConfig {
    pub dir: PathBuf,
    pub download_base: String,
    pub keys: Vec<TrustedKey>,
    pub version: Version,
}

impl DistConfig {
    pub fn production(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("agent-bin"),
            download_base: DOWNLOAD_BASE.to_owned(),
            keys: release::TRUSTED_KEYS.to_vec(),
            version: crate::updates::current_version(),
        }
    }
}

pub struct AgentDist {
    pub(crate) cfg: DistConfig,
    preparing: tokio::sync::Mutex<()>,
    pub(crate) transfers: Arc<Semaphore>,
}

/// Une release d'agent vérifiée, prête à être envoyée.
pub struct Bundle {
    pub manifest: Vec<u8>,
    pub signature: Vec<u8>,
    pub binary: Vec<u8>,
}

/// Nom du binaire de release d'une plateforme connue.
pub fn asset_for(platform: &str) -> Option<String> {
    let known = proto::PLATFORMS.iter().find(|p| **p == platform)?;
    let ext = if known.starts_with("windows") {
        ".exe"
    } else {
        ""
    };
    Some(format!("nxt-agent-{known}{ext}"))
}

fn check(cfg: &DistConfig, asset: &str, b: &Bundle) -> Result<(), &'static str> {
    let v = release::verify_with(&cfg.keys, &b.manifest, &b.signature, unix_now())
        .map_err(|_| "verify")?;
    if v.version != cfg.version {
        return Err("verify");
    }
    v.check_asset(asset, &b.binary).map_err(|_| "verify")
}

/// Relit le cache et le REVÉRIFIE (un fichier altéré sur le disque n'est pas servi).
fn load(cfg: &DistConfig, asset: &str) -> Option<Bundle> {
    let dir = cfg.dir.join(cfg.version.to_string());
    let read = |name: &str, max: u64| read_bounded(&dir.join(name), max).ok();
    let b = Bundle {
        manifest: read("manifest.json", MAX_MANIFEST_BYTES as u64)?,
        signature: read("manifest.json.sig", SIGNATURE_BYTES as u64)?,
        binary: read(asset, MAX_AGENT_BINARY)?,
    };
    check(cfg, asset, &b).ok()?;
    Some(b)
}

async fn fetch(cfg: &DistConfig, asset: &str) -> Result<(), &'static str> {
    let base = &cfg.download_base;
    let url = |name: &str| format!("{base}v{}/{name}", cfg.version);
    let manifest = download(base, &url("manifest.json"), MAX_MANIFEST_BYTES as u64).await?;
    let signature = download(base, &url("manifest.json.sig"), SIGNATURE_BYTES as u64).await?;
    let verified =
        release::verify_with(&cfg.keys, &manifest, &signature, unix_now()).map_err(|_| "verify")?;
    let size = verified.asset(asset).ok_or("verify")?.size;
    if size > MAX_AGENT_BINARY {
        return Err("verify");
    }
    let binary = download(base, &url(asset), size).await?;
    let b = Bundle {
        manifest,
        signature,
        binary,
    };
    check(cfg, asset, &b)?;
    let dir = cfg.dir.join(cfg.version.to_string());
    std::fs::create_dir_all(&dir).map_err(|_| "io")?;
    // Écriture puis renommage : un transfert interrompu ne laisse jamais un fichier à moitié écrit.
    for (name, data) in [
        (asset, &b.binary),
        ("manifest.json", &b.manifest),
        ("manifest.json.sig", &b.signature),
    ] {
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, data)
            .and_then(|()| std::fs::rename(&tmp, dir.join(name)))
            .map_err(|_| "io")?;
    }
    Ok(())
}

impl Core {
    pub fn enable_agent_dist(&self, cfg: DistConfig) {
        let _ = self.agentdist.set(AgentDist {
            cfg,
            preparing: tokio::sync::Mutex::new(()),
            transfers: Arc::new(Semaphore::new(MAX_TRANSFERS)),
        });
    }

    /// Met en cache l'agent de la version du master pour `platform`. `Err` : code (`download`, `verify`, `io`…).
    pub async fn agent_dist_prepare(&self, platform: &str) -> Result<(), &'static str> {
        let d = self.agentdist.get().ok_or("unsupported")?;
        let asset = asset_for(platform).ok_or("unsupported")?;
        let _one_at_a_time = d.preparing.lock().await;
        if load(&d.cfg, &asset).is_some() {
            return Ok(());
        }
        fetch(&d.cfg, &asset).await
    }

    /// La release en cache pour cette plateforme, revérifiée ; `None` si elle n'a pas été préparée.
    pub fn agent_dist_load(&self, platform: &str) -> Option<Bundle> {
        let d = self.agentdist.get()?;
        load(&d.cfg, &asset_for(platform)?)
    }
}

/// États qu'un nœud peut rapporter ; les quatre derniers closent la mise à jour (journalisés une fois).
const FINAL: [&str; 4] = ["installed", "rolled_back", "rejected", "failed"];

impl Core {
    /// Demande à un nœud connecté et équipé de passer à la version du master. `Err` : message pour l'administrateur.
    pub async fn request_agent_update(&self, node_id: &str) -> Result<String, &'static str> {
        let target = self
            .agentdist
            .get()
            .ok_or("distribution indisponible")?
            .cfg
            .version
            .clone();
        let (mut platform, mut capable) = (String::new(), false);
        self.record(node_id, |t| {
            platform = t.platform.clone();
            capable = t.update_capable;
        });
        if !self.sessions.is_connected(node_id) {
            return Err("ce nœud est hors ligne");
        }
        if !capable {
            return Err("ce nœud se met à jour manuellement");
        }
        let current = self
            .store
            .node_details()
            .map_err(|_| "base de données indisponible")?
            .into_iter()
            .find(|n| n.id == node_id)
            .and_then(|n| Version::parse(&n.agent_version));
        if !current.is_some_and(|v| v < target) {
            return Err("ce nœud est déjà à la version du master (ou plus récent)");
        }
        self.agent_dist_prepare(&platform)
            .await
            .map_err(|_| "release de l'agent introuvable ou refusée (est-elle signée ?)")?;
        let cmd = proto::pb::UpdateAgentCmd {
            version: target.to_string(),
        };
        if !self
            .sessions
            .send(node_id, proto::pb::master_msg::M::Update(cmd))
            .await
        {
            return Err("ce nœud est hors ligne");
        }
        self.record(node_id, |t| {
            t.update = None;
            t.updating_until = unix_now() + UPDATE_WINDOW_SECS;
        });
        Ok(target.to_string())
    }

    pub fn is_updating(&self, node_id: &str) -> bool {
        self.telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(node_id)
            .is_some_and(|t| t.updating_until > unix_now())
    }

    /// Ce que l'interface affiche pour la mise à jour d'un nœud.
    pub fn node_update_info(&self, node_id: &str, agent_version: &str) -> serde_json::Value {
        let (capable, last) = {
            let m = self
                .telemetry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            m.get(node_id)
                .map_or((false, None), |t| (t.update_capable, t.update.clone()))
        };
        // Proposée seulement si le nœud est en retard sur la version du master.
        let available = self.agentdist.get().and_then(|d| {
            Version::parse(agent_version)
                .filter(|v| *v < d.cfg.version)
                .map(|_| d.cfg.version.to_string())
        });
        serde_json::json!({
            "capable": capable,
            "available": available,
            "updating": self.is_updating(node_id),
            "last": last.map(|u| serde_json::json!({ "state": u.state, "version": u.version, "reason": u.reason })),
        })
    }

    /// Progression rapportée par un nœud : valeurs bornées, résultat final journalisé une fois.
    pub(crate) fn on_update_status(&self, node_id: &str, mut u: proto::pb::UpdateStatus) {
        let known = ["downloading", "installing"].contains(&u.state.as_str())
            || FINAL.contains(&u.state.as_str());
        if !known || Version::parse(&u.version).is_none() || u.version.len() > 32 {
            return;
        }
        u.reason = u
            .reason
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        if FINAL.contains(&u.state.as_str()) {
            let key = format!("node_update_ts:{node_id}");
            let ts = u.ts.to_string();
            if self.store.setting_get(&key).ok().flatten() != Some(ts.clone()) {
                self.store.audit(
                    "system",
                    &format!("node_update_{}", u.state),
                    &format!("{node_id} {}", u.version),
                );
                let _ = self.store.setting_set(&key, &ts);
            }
        }
        let done = FINAL.contains(&u.state.as_str());
        self.record(node_id, |t| {
            if done {
                t.updating_until = 0;
            }
            t.update = Some(u);
        });
    }
}
