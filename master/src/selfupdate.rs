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
use proto::release::{self, TrustedKey, MAX_MANIFEST_BYTES, SIGNATURE_BYTES};
use proto::version::Version;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Adresse de téléchargement des releases. Constante (aucune option ni variable d'environnement) ; les tests injectent la
/// leur par l'API Rust.
pub const DOWNLOAD_BASE: &str = "https://github.com/next-vertices/nxtTorrent/releases/download/";
/// Dossier du service root : résultat de la dernière installation et binaire précédent.
pub const STATE_DIR: &str = "/var/lib/nxt-update";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Une demande restée sans réponse du service root au-delà de ce délai peut être relancée.
const REQUEST_STALE_SECS: i64 = 600;
const MAX_BINARY: u64 = 256 * 1024 * 1024;
const MAX_STATUS: u64 = 8 * 1024;
const BINARY_NAME: &str = "nxt-master";
/// Temps de fonctionnement après lequel le master se déclare sain.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(15);

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

/// Emplacements dans le dossier de données.
struct Layout {
    dir: PathBuf,
}

impl Layout {
    fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("update"),
        }
    }
    fn staged(&self) -> PathBuf {
        self.dir.join("staged")
    }
    fn manifest(&self) -> PathBuf {
        self.staged().join("manifest.json")
    }
    fn signature(&self) -> PathBuf {
        self.staged().join("manifest.json.sig")
    }
    fn binary(&self) -> PathBuf {
        self.staged().join(BINARY_NAME)
    }
    fn request(&self) -> PathBuf {
        self.dir.join("request")
    }
    fn healthy(&self) -> PathBuf {
        self.dir.join("healthy")
    }
    fn snapshot(&self) -> PathBuf {
        self.dir.join("snapshot.db")
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

async fn download(base: &str, url: &str, max: u64) -> Result<Vec<u8>, &'static str> {
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
    std::fs::write(layout.binary(), &binary).map_err(|_| "io")?;
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

// ---------- Service d'installation (root) ----------

/// Ce que l'installateur fait sur la machine (remplacé par un faux dans les tests).
pub trait Host {
    fn stop(&mut self) -> Result<(), String>;
    fn start(&mut self) -> Result<(), String>;
    /// Sortie de `<binaire> --version`.
    fn probe_version(&mut self, binary: &Path) -> Result<String, String>;
    /// Remet la base d'avant la mise à jour (exécuté sous le compte du master).
    fn restore_snapshot(&mut self, binary: &Path) -> Result<(), String>;
}

pub struct ApplyConfig {
    pub data_dir: PathBuf,
    pub target: PathBuf,
    pub state_dir: PathBuf,
    pub keys: Vec<TrustedKey>,
    pub installed: Version,
    pub asset: String,
    pub health_timeout: Duration,
    pub poll: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    /// Refusée avant de toucher au service.
    Rejected(String),
    /// La nouvelle version n'a pas fonctionné : l'ancienne est revenue.
    RolledBack(String),
    /// Ni l'une ni l'autre : intervention nécessaire.
    Failed(String),
}

/// Lit un fichier ordinaire de taille bornée (ni lien symbolique, ni tube, ni périphérique).
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > max {
        return Err("fichier refusé".to_owned());
    }
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("fichier refusé".to_owned());
    }
    let mut out = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > max {
        return Err("fichier refusé".to_owned());
    }
    Ok(out)
}

/// Écrit un exécutable neuf (jamais à travers un fichier ou un lien existant).
fn write_executable(path: &Path, data: &[u8]) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(data).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn healthy_since(cfg: &ApplyConfig, version: &str, since: SystemTime) -> bool {
    let p = Layout::new(&cfg.data_dir).healthy();
    let fresh = std::fs::symlink_metadata(&p)
        .and_then(|m| m.modified())
        // Marge pour la granularité des dates de fichiers.
        .is_ok_and(|m| m + Duration::from_secs(2) >= since);
    fresh && read_bounded(&p, 128).is_ok_and(|b| String::from_utf8_lossy(&b).trim() == version)
}

fn wait_healthy(cfg: &ApplyConfig, version: &str, since: SystemTime) -> bool {
    let deadline = std::time::Instant::now() + cfg.health_timeout;
    loop {
        if healthy_since(cfg, version, since) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(cfg.poll);
    }
}

fn write_status(cfg: &ApplyConfig, version: &str, outcome: &Outcome) {
    let (state, reason) = match outcome {
        Outcome::Installed => ("installed", ""),
        Outcome::Rejected(r) => ("rejected", r.as_str()),
        Outcome::RolledBack(r) => ("rolled_back", r.as_str()),
        Outcome::Failed(r) => ("failed", r.as_str()),
    };
    let body = serde_json::json!({
        "state": state,
        "reason": reason,
        "version": version,
        "from": cfg.installed.to_string(),
        "ts": unix_now(),
    });
    let path = cfg.state_dir.join("status.json");
    let tmp = cfg.state_dir.join("status.json.tmp");
    let ok = std::fs::write(&tmp, body.to_string()).and_then(|()| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::rename(&tmp, &path)
    });
    if let Err(e) = ok {
        tracing::error!(error = %e, "résultat de la mise à jour non écrit");
    }
}

/// Installe la mise à jour déposée. Écrit toujours un résultat dans `state_dir/status.json`.
pub fn apply(cfg: &ApplyConfig, host: &mut dyn Host) -> Outcome {
    let (version, outcome) = apply_inner(cfg, host);
    write_status(cfg, &version, &outcome);
    outcome
}

fn apply_inner(cfg: &ApplyConfig, host: &mut dyn Host) -> (String, Outcome) {
    let layout = Layout::new(&cfg.data_dir);
    let unknown = String::new;
    let reject = |v: String, why: String| (v, Outcome::Rejected(why));

    // 1. Tout relire et revérifier : le dépôt vient d'un compte non privilégié.
    let manifest = match read_bounded(&layout.manifest(), MAX_MANIFEST_BYTES as u64) {
        Ok(m) => m,
        Err(e) => return reject(unknown(), format!("manifeste : {e}")),
    };
    let sig = match read_bounded(&layout.signature(), SIGNATURE_BYTES as u64) {
        Ok(s) => s,
        Err(e) => return reject(unknown(), format!("signature : {e}")),
    };
    let verified = match release::verify_with(&cfg.keys, &manifest, &sig, unix_now()) {
        Ok(v) => v,
        Err(e) => return reject(unknown(), e.to_string()),
    };
    let version = verified.version.to_string();
    if let Err(e) = verified.require_newer_than(&cfg.installed) {
        return reject(version, e.to_string());
    }
    let binary = match read_bounded(&layout.binary(), MAX_BINARY) {
        Ok(b) => b,
        Err(e) => return reject(version, format!("binaire : {e}")),
    };
    if let Err(e) = verified.check_asset(&cfg.asset, &binary) {
        return reject(version, e.to_string());
    }

    // 2. Écrire CES octets à côté de la cible et s'assurer que le programme démarre.
    let new = cfg.target.with_extension("new");
    let previous = cfg.state_dir.join("previous");
    if let Err(e) = write_executable(&new, &binary) {
        return reject(version, format!("écriture : {e}"));
    }
    let probed = host.probe_version(&new);
    if !probed
        .as_ref()
        .is_ok_and(|out| out.split_whitespace().any(|w| w == version))
    {
        let _ = std::fs::remove_file(&new);
        return reject(version, "le nouveau binaire ne démarre pas".to_owned());
    }
    let saved = std::fs::read(&cfg.target)
        .map_err(|e| e.to_string())
        .and_then(|old| write_executable(&previous, &old));
    if let Err(e) = saved {
        let _ = std::fs::remove_file(&new);
        return reject(version, format!("copie de l'ancien binaire : {e}"));
    }

    // 3. Basculer.
    if let Err(e) = host.stop() {
        let _ = std::fs::remove_file(&new);
        let _ = host.start();
        return reject(version, format!("arrêt du service : {e}"));
    }
    if let Err(e) = std::fs::rename(&new, &cfg.target) {
        let _ = std::fs::remove_file(&new);
        let _ = host.start();
        return reject(version, format!("remplacement : {e}"));
    }
    let since = SystemTime::now();
    let started = host.start();
    if started.is_ok() && wait_healthy(cfg, &version, since) {
        return (version, Outcome::Installed);
    }
    let why = match started {
        Err(e) => format!("démarrage : {e}"),
        Ok(()) => "la nouvelle version ne s'est pas déclarée saine à temps".to_owned(),
    };

    // 4. Retour arrière : binaire ET base.
    let back = (|| -> Result<(), String> {
        host.stop()?;
        let old = std::fs::read(&previous).map_err(|e| e.to_string())?;
        write_executable(&new, &old)?;
        std::fs::rename(&new, &cfg.target).map_err(|e| e.to_string())?;
        host.restore_snapshot(&cfg.target)?;
        host.start()
    })();
    match back {
        Ok(()) => (version, Outcome::RolledBack(why)),
        Err(e) => {
            let _ = host.start();
            (
                version,
                Outcome::Failed(format!("{why} ; retour arrière impossible : {e}")),
            )
        }
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
    //! Implémentation réelle (systemd) et installation des unités.
    use super::{apply, asset_name, ApplyConfig, Host, Outcome, STATE_DIR};
    use anyhow::Context;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    const UNIT_DIR: &str = "/etc/systemd/system";

    struct Systemd {
        service: String,
        data_dir: PathBuf,
    }

    impl Systemd {
        fn ctl(&self, verb: &str) -> Result<(), String> {
            let s = Command::new("systemctl")
                .args([verb, &self.service])
                .status()
                .map_err(|e| e.to_string())?;
            if s.success() {
                Ok(())
            } else {
                Err(format!("systemctl {verb} a échoué"))
            }
        }
        /// Commande abaissée au compte propriétaire du dossier de données.
        fn as_owner(&self, program: &Path) -> Result<Command, String> {
            let m = std::fs::metadata(&self.data_dir).map_err(|e| e.to_string())?;
            let mut c = Command::new(program);
            c.uid(m.uid()).gid(m.gid());
            Ok(c)
        }
    }

    impl Host for Systemd {
        fn stop(&mut self) -> Result<(), String> {
            self.ctl("stop")
        }
        fn start(&mut self) -> Result<(), String> {
            self.ctl("start")
        }
        fn probe_version(&mut self, binary: &Path) -> Result<String, String> {
            let o = self
                .as_owner(binary)?
                .arg("--version")
                .current_dir("/")
                .output()
                .map_err(|e| e.to_string())?;
            if o.status.success() {
                Ok(String::from_utf8_lossy(&o.stdout).into_owned())
            } else {
                Err("--version a échoué".to_owned())
            }
        }
        fn restore_snapshot(&mut self, binary: &Path) -> Result<(), String> {
            let s = self
                .as_owner(binary)?
                .arg("--data-dir")
                .arg(&self.data_dir)
                .arg("restore-snapshot")
                .current_dir("/")
                .status()
                .map_err(|e| e.to_string())?;
            if s.success() {
                Ok(())
            } else {
                Err("restauration de la base impossible".to_owned())
            }
        }
    }

    /// `apply-update` : appelé par le service root.
    pub fn run_apply(data_dir: &Path, service: &str) -> anyhow::Result<()> {
        let target = std::env::current_exe()?;
        let cfg = ApplyConfig {
            data_dir: data_dir.to_path_buf(),
            target,
            state_dir: PathBuf::from(STATE_DIR),
            keys: proto::release::TRUSTED_KEYS.to_vec(),
            installed: crate::updates::current_version(),
            asset: asset_name()
                .context("architecture non prise en charge")?
                .to_owned(),
            health_timeout: Duration::from_secs(120),
            poll: Duration::from_secs(2),
        };
        let mut host = Systemd {
            service: service.to_owned(),
            data_dir: data_dir.to_path_buf(),
        };
        match apply(&cfg, &mut host) {
            Outcome::Installed => {
                tracing::info!("mise à jour installée");
                Ok(())
            }
            other => anyhow::bail!("mise à jour non installée : {other:?}"),
        }
    }

    fn safe_path(p: &Path) -> anyhow::Result<&str> {
        let s = p.to_str().context("chemin illisible")?;
        anyhow::ensure!(
            p.is_absolute()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')),
            "chemin non pris en charge dans une unité systemd : {s}"
        );
        Ok(s)
    }

    fn root_only(p: &Path) -> anyhow::Result<()> {
        let m = std::fs::metadata(p)?;
        anyhow::ensure!(
            m.uid() == 0 && m.permissions().mode() & 0o022 == 0,
            "{} doit appartenir à root et n'être modifiable que par root",
            p.display()
        );
        Ok(())
    }

    fn systemctl(args: &[&str]) -> anyhow::Result<()> {
        anyhow::ensure!(
            Command::new("systemctl").args(args).status()?.success(),
            "systemctl {} a échoué",
            args.join(" ")
        );
        Ok(())
    }

    /// Les trois fichiers installés par `self-update enable`.
    pub fn unit_files(binary: &str, data_dir: &str, service: &str) -> [(String, String); 3] {
        let name = service.trim_end_matches(".service");
        let bin_dir = Path::new(binary)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or("/usr/local/bin");
        [
            (
                format!("{name}-update.path"),
                format!(
                    "[Unit]\nDescription=nxtTorrent - demande de mise à jour du master\n\n[Path]\nPathChanged={data_dir}/update/request\nUnit={name}-update.service\n\n[Install]\nWantedBy=multi-user.target\n"
                ),
            ),
            (
                format!("{name}-update.service"),
                format!(
                    "[Unit]\nDescription=nxtTorrent - installation d'une mise à jour signée du master\n\n[Service]\nType=oneshot\nExecStart={binary} --data-dir {data_dir} apply-update --service {name}.service\nTimeoutStartSec=600\nNoNewPrivileges=yes\nPrivateTmp=yes\nProtectHome=yes\nProtectSystem=strict\nReadWritePaths={bin_dir} {data_dir} {STATE_DIR}\n"
                ),
            ),
            (
                format!("{name}.service.d/self-update.conf"),
                "[Service]\nEnvironment=MASTER_SELF_UPDATE=1\n".to_owned(),
            ),
        ]
    }

    /// `self-update enable` : à lancer en root, une fois, sur la machine du master.
    pub fn enable(data_dir: &Path, service: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            std::fs::metadata("/proc/self")?.uid() == 0,
            "à lancer en root (sudo)"
        );
        asset_name().context("architecture non prise en charge")?;
        let binary = std::env::current_exe()?;
        // Le service root exécute ce binaire : personne d'autre ne doit pouvoir le modifier.
        root_only(&binary)?;
        root_only(binary.parent().context("binaire sans dossier")?)?;
        anyhow::ensure!(
            data_dir.is_dir(),
            "dossier de données introuvable : {} (voir --data-dir)",
            data_dir.display()
        );
        systemctl(&["cat", service]).context("service du master introuvable (voir --service)")?;
        std::fs::create_dir_all(STATE_DIR)?;
        std::fs::set_permissions(STATE_DIR, std::fs::Permissions::from_mode(0o755))?;
        let files = unit_files(safe_path(&binary)?, safe_path(data_dir)?, service);
        for (name, content) in &files {
            let p = Path::new(UNIT_DIR).join(name);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&p, content)?;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644))?;
            println!("écrit : {}", p.display());
        }
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", &files[0].0])?;
        println!(
            "\nMise à jour automatique activée. Redémarrez le master : systemctl restart {service}"
        );
        Ok(())
    }

    /// `self-update disable`.
    pub fn disable(data_dir: &Path, service: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            std::fs::metadata("/proc/self")?.uid() == 0,
            "à lancer en root (sudo)"
        );
        let files = unit_files("/usr/local/bin/nxt-master", safe_path(data_dir)?, service);
        let _ = systemctl(&["disable", "--now", &files[0].0]);
        for (name, _) in &files {
            let _ = std::fs::remove_file(Path::new(UNIT_DIR).join(name));
        }
        systemctl(&["daemon-reload"])?;
        println!("Mise à jour automatique désactivée. Redémarrez le master : systemctl restart {service}");
        Ok(())
    }
}
