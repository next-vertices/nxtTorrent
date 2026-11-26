//! Installateur d'une mise à jour signée, commun au master et à l'agent. Il est exécuté par un petit service **root** et
//! ne fait CONFIANCE À RIEN du dossier de données (il appartient à un compte sans privilège) : il revérifie signature,
//! version (jamais de retour en arrière) et empreinte sur les octets qu'il a lus puis écrits lui-même, remplace le
//! binaire, redémarre, et **revient en arrière** si la nouvelle version ne se déclare pas saine à temps.
use crate::release::{self, TrustedKey, MAX_MANIFEST_BYTES, SIGNATURE_BYTES};
use crate::version::Version;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const MAX_BINARY: u64 = 256 * 1024 * 1024;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Emplacements dans le dossier de données.
pub struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("update"),
        }
    }
    pub fn staged(&self) -> PathBuf {
        self.dir.join("staged")
    }
    pub fn manifest(&self) -> PathBuf {
        self.staged().join("manifest.json")
    }
    pub fn signature(&self) -> PathBuf {
        self.staged().join("manifest.json.sig")
    }
    pub fn binary(&self, name: &str) -> PathBuf {
        self.staged().join(name)
    }
    pub fn request(&self) -> PathBuf {
        self.dir.join("request")
    }
    pub fn healthy(&self) -> PathBuf {
        self.dir.join("healthy")
    }
    pub fn snapshot(&self) -> PathBuf {
        self.dir.join("snapshot.db")
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
    /// Nom du binaire déposé dans `update/staged/`.
    pub binary_name: String,
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
pub fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
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
        "ts": now(),
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
        eprintln!("résultat de la mise à jour non écrit : {e}");
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
    let verified = match release::verify_with(&cfg.keys, &manifest, &sig, now()) {
        Ok(v) => v,
        Err(e) => return reject(unknown(), e.to_string()),
    };
    let version = verified.version.to_string();
    if let Err(e) = verified.require_newer_than(&cfg.installed) {
        return reject(version, e.to_string());
    }
    let binary = match read_bounded(&layout.binary(&cfg.binary_name), MAX_BINARY) {
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

/// Ce qui distingue les programmes installables.
pub struct App {
    /// Préfixe du binaire de release (`nxt-master`, `nxt-agent`).
    pub binary_name: &'static str,
    /// Dossier du service root (résultat, binaire précédent).
    pub state_dir: &'static str,
    /// Variable posée sur le service : « un installateur est en place ».
    pub env: &'static str,
    /// Sous-commande qui remet les données d'avant la mise à jour, s'il y en a.
    pub restore_command: Option<&'static str>,
    pub health_timeout: Duration,
}

impl App {
    /// Nom du binaire de release pour cette machine Linux.
    pub fn asset(&self) -> Option<String> {
        crate::platform()
            .filter(|p| p.starts_with("linux"))
            .map(|p| format!("{}-{p}", self.binary_name))
    }
}

#[cfg(unix)]
pub mod linux {
    //! Implémentation réelle (systemd) et installation des unités.
    use super::{apply, App, ApplyConfig, Host, Outcome};
    use crate::version::Version;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    const UNIT_DIR: &str = "/etc/systemd/system";

    struct Systemd {
        service: String,
        data_dir: PathBuf,
        restore: Option<&'static str>,
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
            c.uid(m.uid()).gid(m.gid()).current_dir("/");
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
                .output()
                .map_err(|e| e.to_string())?;
            if o.status.success() {
                Ok(String::from_utf8_lossy(&o.stdout).into_owned())
            } else {
                Err("--version a échoué".to_owned())
            }
        }
        fn restore_snapshot(&mut self, binary: &Path) -> Result<(), String> {
            let Some(command) = self.restore else {
                return Ok(());
            };
            let s = self
                .as_owner(binary)?
                .arg("--data-dir")
                .arg(&self.data_dir)
                .arg(command)
                .status()
                .map_err(|e| e.to_string())?;
            if s.success() {
                Ok(())
            } else {
                Err("restauration des données impossible".to_owned())
            }
        }
    }

    /// `apply-update` : appelé par le service root.
    pub fn run_apply(
        app: &App,
        installed: Version,
        data_dir: &Path,
        service: &str,
    ) -> Result<(), String> {
        let cfg = ApplyConfig {
            data_dir: data_dir.to_path_buf(),
            target: std::env::current_exe().map_err(|e| e.to_string())?,
            state_dir: PathBuf::from(app.state_dir),
            keys: crate::release::TRUSTED_KEYS.to_vec(),
            installed,
            asset: app.asset().ok_or("architecture non prise en charge")?,
            binary_name: app.binary_name.to_owned(),
            health_timeout: app.health_timeout,
            poll: Duration::from_secs(2),
        };
        let mut host = Systemd {
            service: service.to_owned(),
            data_dir: data_dir.to_path_buf(),
            restore: app.restore_command,
        };
        match apply(&cfg, &mut host) {
            Outcome::Installed => Ok(()),
            other => Err(format!("mise à jour non installée : {other:?}")),
        }
    }

    fn safe_path(p: &Path) -> Result<&str, String> {
        p.to_str()
            .filter(|s| {
                p.is_absolute()
                    && s.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')
                    })
            })
            .ok_or_else(|| {
                format!(
                    "chemin non pris en charge dans une unité systemd : {}",
                    p.display()
                )
            })
    }

    fn root_only(p: &Path) -> Result<(), String> {
        let m = std::fs::metadata(p).map_err(|e| e.to_string())?;
        if m.uid() == 0 && m.permissions().mode() & 0o022 == 0 {
            Ok(())
        } else {
            Err(format!(
                "{} doit appartenir à root et n'être modifiable que par root",
                p.display()
            ))
        }
    }

    fn systemctl(args: &[&str]) -> Result<(), String> {
        let ok = Command::new("systemctl")
            .args(args)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if ok {
            Ok(())
        } else {
            Err(format!("systemctl {} a échoué", args.join(" ")))
        }
    }

    fn require_root() -> Result<(), String> {
        let uid = std::fs::metadata("/proc/self")
            .map_err(|e| e.to_string())?
            .uid();
        if uid == 0 {
            Ok(())
        } else {
            Err("à lancer en root (sudo)".to_owned())
        }
    }

    /// Les trois fichiers installés par `self-update enable`.
    fn unit_files(app: &App, binary: &str, data_dir: &str, service: &str) -> [(String, String); 3] {
        let name = service.trim_end_matches(".service");
        let bin_dir = Path::new(binary)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or("/usr/local/bin");
        let state = app.state_dir;
        let env = app.env;
        [
            (
                format!("{name}-update.path"),
                format!(
                    "[Unit]\nDescription=nxtTorrent - demande de mise à jour ({name})\n\n[Path]\nPathChanged={data_dir}/update/request\nUnit={name}-update.service\n\n[Install]\nWantedBy=multi-user.target\n"
                ),
            ),
            (
                format!("{name}-update.service"),
                format!(
                    "[Unit]\nDescription=nxtTorrent - installation d'une mise à jour signée ({name})\n\n[Service]\nType=oneshot\nExecStart={binary} --data-dir {data_dir} apply-update --service {name}.service\nTimeoutStartSec=600\nNoNewPrivileges=yes\nPrivateTmp=yes\nProtectHome=yes\nProtectSystem=strict\nReadWritePaths={bin_dir} {data_dir} {state}\n"
                ),
            ),
            (
                format!("{name}.service.d/self-update.conf"),
                format!("[Service]\nEnvironment={env}=1\n"),
            ),
        ]
    }

    /// `self-update enable` : à lancer en root, une fois.
    pub fn enable(app: &App, data_dir: &Path, service: &str) -> Result<(), String> {
        require_root()?;
        app.asset().ok_or("architecture non prise en charge")?;
        let binary = std::env::current_exe().map_err(|e| e.to_string())?;
        // Le service root exécute ce binaire : personne d'autre ne doit pouvoir le modifier.
        root_only(&binary)?;
        root_only(binary.parent().ok_or("binaire sans dossier")?)?;
        if !data_dir.is_dir() {
            return Err(format!(
                "dossier de données introuvable : {} (voir --data-dir)",
                data_dir.display()
            ));
        }
        systemctl(&["cat", service])
            .map_err(|_| format!("service {service} introuvable (voir --service)"))?;
        std::fs::create_dir_all(app.state_dir).map_err(|e| e.to_string())?;
        std::fs::set_permissions(app.state_dir, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let files = unit_files(app, safe_path(&binary)?, safe_path(data_dir)?, service);
        for (name, content) in &files {
            let p = Path::new(UNIT_DIR).join(name);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, content)
                .and_then(|()| std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)))
                .map_err(|e| e.to_string())?;
            println!("écrit : {}", p.display());
        }
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", &files[0].0])?;
        println!(
            "\nMise à jour automatique activée. Redémarrez le service : systemctl restart {service}"
        );
        Ok(())
    }

    /// `self-update disable`.
    pub fn disable(app: &App, data_dir: &Path, service: &str) -> Result<(), String> {
        require_root()?;
        let files = unit_files(app, "/usr/local/bin/x", safe_path(data_dir)?, service);
        let _ = systemctl(&["disable", "--now", &files[0].0]);
        for (name, _) in &files {
            let _ = std::fs::remove_file(Path::new(UNIT_DIR).join(name));
        }
        systemctl(&["daemon-reload"])?;
        println!("Mise à jour automatique désactivée. Redémarrez le service : systemctl restart {service}");
        Ok(())
    }
}
