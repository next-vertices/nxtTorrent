//! Mise à jour du master par lui-même (Linux + systemd), déclenchée par un administrateur.
//!
//! Deux rôles, deux niveaux de privilège :
//! - le **master** (compte sans privilège) télécharge la release, vérifie sa signature, prend un instantané de la base,
//!   dépose le tout dans `<data>/update/staged/` puis écrit `<data>/update/request` ;
//! - un petit service **root** (`apply-update`, déclenché par ce fichier) ne fait CONFIANCE À RIEN de ce dépôt : il
//!   revérifie lui-même signature, version (jamais de retour en arrière) et empreinte, sur les octets qu'il a lus en
//!   mémoire et qu'il écrit ensuite lui-même ; remplace le binaire ; redémarre ; et **revient en arrière** (binaire et
//!   base) si la nouvelle version ne se déclare pas saine à temps.
//!
//! Un master compromis peut donc demander une installation, mais seulement d'une release signée et plus récente.
//! Le service root n'écrit jamais dans le dossier de données (il appartient au compte non privilégié) : la restauration
//! de la base est faite par un processus fils abaissé à ce compte.
use crate::store::unix_now;
use crate::Core;
pub(crate) use proto::install::read_bounded;
pub use proto::install::{apply, ApplyConfig, Host, Outcome};
use proto::install::{App, Layout, MAX_BINARY};
use proto::release::{self, TrustedKey, MAX_MANIFEST_BYTES, SIGNATURE_BYTES};
use proto::version::Version;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Adresse de téléchargement des releases. Constante (aucune option ni variable d'environnement) ; les tests injectent la
/// leur par l'API Rust.
pub const DOWNLOAD_BASE: &str = "https://github.com/next-vertices/nxtTorrent/releases/download/";
/// Dossier du service root : résultat de la dernière installation et binaire précédent.
pub const STATE_DIR: &str = "/var/lib/nxt-update";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Une demande restée sans réponse du service root au-delà de ce délai peut être relancée.
const REQUEST_STALE_SECS: i64 = 600;
const MAX_STATUS: u64 = 8 * 1024;
/// Temps de fonctionnement après lequel le master se déclare sain.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(15);

/// Le master, pour l'installateur commun.
pub const APP: App = App {
    binary_name: "nxt-master",
    state_dir: STATE_DIR,
    env: "MASTER_SELF_UPDATE",
    restore_command: Some("restore-snapshot"),
    health_timeout: Duration::from_secs(120),
};

/// Nom du binaire de release pour cette machine ; `None` là où la mise à jour automatique n'est pas prise en charge.
pub fn asset_name() -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match std::env::consts::ARCH {
        "x86_64" => Some("nxt-master-linux-amd64"),
        "aarch64" => Some("nxt-master-linux-arm64"),
        _ => None,
    }
}

/// Réglages côté master. En production : [`Config::production`].
#[derive(Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub status_file: PathBuf,
    pub download_base: String,
    pub keys: Vec<TrustedKey>,
    pub asset: String,
    pub installed: Version,
}

impl Config {
    /// `None` si cette plateforme n'est pas prise en charge.
    pub fn production(data_dir: &Path) -> Option<Self> {
        Some(Self {
            data_dir: data_dir.to_path_buf(),
            status_file: Path::new(STATE_DIR).join("status.json"),
            download_base: DOWNLOAD_BASE.to_owned(),
            keys: release::TRUSTED_KEYS.to_vec(),
            asset: asset_name()?.to_owned(),
            installed: crate::updates::current_version(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Progress {
    Idle,
    Downloading { version: String },
    Requested { version: String, at: i64 },
    Failed { version: String, code: &'static str },
}

pub struct SelfUpdate {
    cfg: Config,
    progress: Mutex<Progress>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StartError {
    #[error("la mise à jour automatique n'est pas activée sur cette machine")]
    Unsupported,
    #[error("cette version n'est pas proposée : relancez la vérification")]
    NotAvailable,
    #[error("une mise à jour est déjà en cours")]
    Busy,
}

impl SelfUpdate {
    fn progress(&self) -> Progress {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn set(&self, p: Progress) {
        *self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = p;
    }
    /// Résultat écrit par le service root.
    fn last_result(&self) -> Option<serde_json::Value> {
        let b = read_bounded(&self.cfg.status_file, MAX_STATUS).ok()?;
        serde_json::from_slice(&b).ok()
    }
}

pub(crate) async fn download(base: &str, url: &str, max: u64) -> Result<Vec<u8>, &'static str> {
    let http = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        // GitHub redirige vers son stockage : quelques sauts, et jamais de repli en clair si la base est en https.
        .redirect(reqwest::redirect::Policy::limited(5))
        .https_only(base.starts_with("https://"))
        .user_agent("nxtTorrent-update")
        .build()
        .map_err(|_| "download")?;
    let mut r = http.get(url).send().await.map_err(|_| "download")?;
    if r.status().as_u16() != 200 || r.content_length().is_some_and(|l| l > max) {
        return Err("download");
    }
    let mut out = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| "download")? {
        if (out.len() + chunk.len()) as u64 > max {
            return Err("download");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Télécharge, vérifie et dépose la mise à jour, puis demande son installation. `Err` : code affiché par l'interface.
async fn prepare(core: &Arc<Core>, cfg: &Config, version: &str) -> Result<(), &'static str> {
    let tag = format!("v{version}");
    let base = &cfg.download_base;
    let url = |name: &str| format!("{base}{tag}/{name}");
    let manifest = download(base, &url("manifest.json"), MAX_MANIFEST_BYTES as u64).await?;
    let sig = download(base, &url("manifest.json.sig"), SIGNATURE_BYTES as u64).await?;
    let verified = release::verify_with(&cfg.keys, &manifest, &sig, unix_now()).map_err(|e| {
        tracing::warn!(error = %e, "manifeste de mise à jour refusé");
        "verify"
    })?;
    // La version demandée, signée, et strictement plus récente que la nôtre.
    if verified.tag != tag || verified.require_newer_than(&cfg.installed).is_err() {
        return Err("verify");
    }
    let asset = verified.asset(&cfg.asset).ok_or("verify")?.clone();
    if asset.size > MAX_BINARY {
        return Err("verify");
    }
    let binary = download(base, &url(&cfg.asset), asset.size).await?;
    verified
        .check_asset(&cfg.asset, &binary)
        .map_err(|_| "verify")?;

    let layout = Layout::new(&cfg.data_dir);
    let _ = std::fs::remove_dir_all(layout.staged());
    std::fs::create_dir_all(layout.staged()).map_err(|_| "io")?;
    // Instantané de la base AVANT la mise à jour : une nouvelle version peut migrer le schéma.
    let _ = std::fs::remove_file(layout.snapshot());
    let (c, snap) = (core.clone(), layout.snapshot());
    tokio::task::spawn_blocking(move || c.store.snapshot_into(&snap))
        .await
        .map_err(|_| "snapshot")?
        .map_err(|_| "snapshot")?;
    private(&layout.snapshot()).map_err(|_| "snapshot")?;
    std::fs::write(layout.manifest(), &manifest).map_err(|_| "io")?;
    std::fs::write(layout.signature(), &sig).map_err(|_| "io")?;
    std::fs::write(layout.binary(APP.binary_name), &binary).map_err(|_| "io")?;
    // En DERNIER : c'est ce fichier qui réveille le service d'installation.
    std::fs::write(layout.request(), format!("{version}\n{}\n", unix_now())).map_err(|_| "io")
}

fn private(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

impl Core {
    /// Active la mise à jour automatique (le service root doit être installé : voir `self-update enable`).
    pub fn enable_self_update(&self, cfg: Config) {
        let _ = self.selfupdate.set(SelfUpdate {
            cfg,
            progress: Mutex::new(Progress::Idle),
        });
    }

    /// Lance en arrière-plan l'installation de `version`, qui doit être celle annoncée par la dernière vérification.
    pub fn self_update_start(self: &Arc<Self>, version: &str) -> Result<(), StartError> {
        let su = self.selfupdate.get().ok_or(StartError::Unsupported)?;
        let status = self.update_status();
        if status["available"] != true || status["latest"]["version"].as_str() != Some(version) {
            return Err(StartError::NotAvailable);
        }
        {
            let mut p = su
                .progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let busy = match &*p {
                Progress::Downloading { .. } => true,
                Progress::Requested { at, .. } => {
                    let answered = su
                        .last_result()
                        .and_then(|r| r["ts"].as_i64())
                        .is_some_and(|ts| ts >= *at);
                    !answered && unix_now() - at < REQUEST_STALE_SECS
                }
                _ => false,
            };
            if busy {
                return Err(StartError::Busy);
            }
            *p = Progress::Downloading {
                version: version.to_owned(),
            };
        }
        self.bump_live();
        let (core, version) = (self.clone(), version.to_owned());
        tokio::spawn(async move {
            let Some(su) = core.selfupdate.get() else {
                return;
            };
            let next = match prepare(&core, &su.cfg, &version).await {
                Ok(()) => {
                    tracing::info!(version, "mise à jour déposée, installation demandée");
                    Progress::Requested {
                        version,
                        at: unix_now(),
                    }
                }
                Err(code) => {
                    tracing::warn!(code, version, "préparation de la mise à jour impossible");
                    core.store
                        .audit("system", "update_failed", &format!("{version}:{code}"));
                    Progress::Failed { version, code }
                }
            };
            su.set(next);
            core.bump_live();
        });
        Ok(())
    }

    /// Reporte au journal le résultat du service root (une seule fois par installation).
    pub fn record_self_update_result(&self) {
        let Some(r) = self.selfupdate.get().and_then(SelfUpdate::last_result) else {
            return;
        };
        let (Some(ts), Some(state), Some(version)) =
            (r["ts"].as_i64(), r["state"].as_str(), r["version"].as_str())
        else {
            return;
        };
        let key = ts.to_string();
        if self.store.setting_get("update_recorded_ts").ok().flatten() == Some(key.clone()) {
            return;
        }
        let action = match state {
            "installed" => "update_installed",
            "rolled_back" => "update_rolled_back",
            "rejected" => "update_rejected",
            _ => "update_failed",
        };
        let detail = format!("{version}:{}", r["reason"].as_str().unwrap_or(""));
        self.store.audit("system", action, &detail);
        let _ = self.store.setting_set("update_recorded_ts", &key);
    }

    /// État de la mise à jour automatique pour l'interface.
    pub fn self_update_status(&self) -> serde_json::Value {
        let Some(su) = self.selfupdate.get() else {
            return serde_json::json!({ "supported": false });
        };
        self.record_self_update_result();
        let last = su.last_result();
        let (phase, version, error) = match su.progress() {
            Progress::Idle => ("idle", None, None),
            Progress::Downloading { version } => ("downloading", Some(version), None),
            Progress::Requested { version, at } => {
                if last
                    .as_ref()
                    .and_then(|r| r["ts"].as_i64())
                    .is_some_and(|ts| ts >= at)
                {
                    ("idle", None, None)
                } else {
                    ("installing", Some(version), None)
                }
            }
            Progress::Failed { version, code } => ("failed", Some(version), Some(code)),
        };
        serde_json::json!({
            "supported": true,
            "phase": phase,
            "version": version,
            "error": error,
            "last": last,
        })
    }

    /// Après quelques secondes de fonctionnement, déclare cette version saine : le service d'installation l'attend. Guette
    /// aussi son résultat, pour le reporter au journal sans attendre que quelqu'un ouvre l'interface.
    pub fn self_update_mark_healthy_later(self: &Arc<Self>, after: Duration) {
        // Le résultat est écrit par le service root vers le démarrage : juste APRÈS un retour arrière (l'ancienne version
        // redémarre), ou après que la nouvelle s'est déclarée saine. On le guette donc dès maintenant, quelques minutes.
        let watcher = self.clone();
        tokio::spawn(async move {
            for _ in 0..40 {
                watcher.record_self_update_result();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        let core = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let Some(su) = core.selfupdate.get() else {
                return;
            };
            let layout = Layout::new(&su.cfg.data_dir);
            let ok = std::fs::create_dir_all(&layout.dir)
                .and_then(|()| std::fs::write(layout.healthy(), format!("{}\n", su.cfg.installed)));
            if let Err(e) = ok {
                tracing::warn!(error = %e, "marqueur de bonne santé non écrit");
            }
        });
    }
}

/// `restore-snapshot` : remet la base prise avant la mise à jour. Le master doit être arrêté.
pub fn restore_snapshot(data_dir: &Path) -> anyhow::Result<()> {
    let snapshot = Layout::new(data_dir).snapshot();
    anyhow::ensure!(snapshot.is_file(), "aucun instantané à restaurer");
    let db = data_dir.join("master.db");
    let tmp = data_dir.join("master.db.restore");
    std::fs::copy(&snapshot, &tmp)?;
    private(&tmp)?;
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(data_dir.join(format!("master.db{ext}")));
    }
    std::fs::rename(&tmp, &db)?;
    Ok(())
}

#[cfg(unix)]
pub mod linux {
    //! Commandes `self-update` et `apply-update` du master.
    use super::APP;
    use proto::install::linux;
    use std::path::Path;

    pub fn run_apply(data_dir: &Path, service: &str) -> anyhow::Result<()> {
        linux::run_apply(&APP, crate::updates::current_version(), data_dir, service)
            .map_err(anyhow::Error::msg)
    }
    pub fn enable(data_dir: &Path, service: &str) -> anyhow::Result<()> {
        linux::enable(&APP, data_dir, service).map_err(anyhow::Error::msg)
    }
    pub fn disable(data_dir: &Path, service: &str) -> anyhow::Result<()> {
        linux::disable(&APP, data_dir, service).map_err(anyhow::Error::msg)
    }
}
