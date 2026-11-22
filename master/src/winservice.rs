//! Service Windows du master : `master service install|uninstall|start|stop|status`.
//! Le gestionnaire de services lance `master service run …` (sous-commande cachée).
use crate::{run_master, runtime, ServeArgs};
use clap::Subcommand;
use proto::fsutil::{open_log, write_secret_atomic};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const LOG_MAX: u64 = 10 * 1024 * 1024;

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Installe le service (démarrage automatique différé, redémarrage après plantage). Administrateur requis.
    Install(ServeArgs),
    /// Arrête et supprime le service.
    Uninstall,
    Start,
    Stop,
    Status,
    /// Lancé par le gestionnaire de services : ne pas utiliser à la main.
    #[command(hide = true)]
    Run(ServeArgs),
}

pub fn handle(data_dir: PathBuf, name: &str, cmd: ServiceCmd) -> anyhow::Result<()> {
    match cmd {
        ServiceCmd::Install(a) => install(&data_dir, name, a),
        ServiceCmd::Uninstall => {
            winsvc::uninstall(name)?;
            println!("Service {name} supprimé.");
            Ok(())
        }
        ServiceCmd::Start => {
            winsvc::start(name)?;
            println!("Service {name} démarré.");
            Ok(())
        }
        ServiceCmd::Stop => {
            winsvc::stop(name)?;
            println!("Service {name} arrêté.");
            Ok(())
        }
        ServiceCmd::Status => {
            println!("{name}: {}", winsvc::status(name)?);
            Ok(())
        }
        ServiceCmd::Run(a) => run_service(data_dir, name, a),
    }
}

/// Arguments de lancement : chemin ABSOLU, jamais l'URL du webhook (secret possible ; la configuration
/// d'un service est lisible par les comptes locaux) — elle est placée dans un fichier à ACL restreinte.
pub fn launch_args(data_dir: &Path, name: &str, a: &ServeArgs) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        "--data-dir".into(),
        data_dir.into(),
        "service".into(),
        "--service-name".into(),
        name.into(),
        "run".into(),
        "--listen".into(),
        a.listen.clone().into(),
    ];
    for s in &a.sans {
        v.extend(["--san".into(), s.into()]);
    }
    v.extend(["--admin-listen".into(), a.admin_listen.clone().into()]);
    if a.admin_allow_remote {
        v.push("--admin-allow-remote".into());
    }
    if !a.tracker_listen.is_empty() {
        v.extend(["--tracker-listen".into(), a.tracker_listen.clone().into()]);
        if !a.tracker_url.is_empty() {
            v.extend(["--tracker-url".into(), a.tracker_url.clone().into()]);
        }
    }
    v
}

fn install(data_dir: &Path, name: &str, a: ServeArgs) -> anyhow::Result<()> {
    let data_dir = std::path::absolute(data_dir)?;
    std::fs::create_dir_all(&data_dir)?;
    // Crée la CA tout de suite (échoue clairement si le dossier est inutilisable) et fixe les droits.
    master::ca::Ca::load_or_create(&data_dir)?;
    if let Some(url) = a.webhook_url.clone().filter(|u| !u.is_empty()) {
        write_secret_atomic(&data_dir.join("webhook_url"), url.as_bytes())?;
        println!("URL de webhook enregistrée dans un fichier protégé.");
    }
    winsvc::install(&winsvc::InstallSpec {
        name: name.to_owned(),
        display_name: "nxtTorrent Master".to_owned(),
        description: "Master de l'orchestrateur de clients torrent (CA, appairage, tunnel mTLS, ordonnancement)."
            .to_owned(),
        exe: std::env::current_exe()?,
        args: launch_args(&data_dir, name, &a),
    })?;
    println!("Service {name} installé. Démarrez-le avec : master service start");
    println!(
        "Pensez à ouvrir le port d'écoute dans le pare-feu, ex. : New-NetFirewallRule -DisplayName nxt-master -Direction Inbound -Protocol TCP -LocalPort {} -Action Allow",
        a.listen.rsplit(':').next().unwrap_or("7443")
    );
    Ok(())
}

fn run_service(data_dir: PathBuf, name: &str, a: ServeArgs) -> anyhow::Result<()> {
    if let Ok(f) = open_log(&data_dir.join("master.log"), LOG_MAX) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init();
    }
    tracing::info!("service démarré");
    winsvc::run(name, move |stop| {
        let rt = runtime().map_err(|e| e.to_string())?;
        let res = rt.block_on(run_master(&data_dir, a, async move {
            stop.notified().await;
        }));
        match &res {
            Ok(()) => tracing::info!("service arrêté"),
            Err(e) => tracing::error!(error = %e, "le service s'arrête sur erreur"),
        }
        res.map_err(|e| e.to_string())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_args_never_contain_the_webhook_secret() {
        let a = ServeArgs {
            listen: "0.0.0.0:7443".into(),
            sans: vec!["master.example.org".into(), "10.0.0.5".into()],
            admin_listen: "127.0.0.1:7080".into(),
            admin_allow_remote: false,
            admin_allow_remote_setup: false,
            agent_binary_url: String::new(),
            agent_binary_sha256: String::new(),
            tracker_listen: String::new(),
            tracker_url: String::new(),
            webhook_url: Some("https://ntfy.example/secret-topic-xyz".into()),
        };
        let s: Vec<String> = launch_args(Path::new("C:\\nxt\\master"), "nxt-master", &a)
            .iter()
            .map(|x| x.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            s,
            [
                "--data-dir",
                "C:\\nxt\\master",
                "service",
                "--service-name",
                "nxt-master",
                "run",
                "--listen",
                "0.0.0.0:7443",
                "--san",
                "master.example.org",
                "--san",
                "10.0.0.5",
                "--admin-listen",
                "127.0.0.1:7080"
            ]
        );
        assert!(!s.join(" ").contains("secret-topic"));
    }
}
