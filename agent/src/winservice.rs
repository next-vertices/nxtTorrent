//! Service Windows de l'agent : `agent service install|uninstall|start|stop|status`.
//! Le gestionnaire de services lance `agent service run …` (sous-commande cachée).
use crate::{run_agent, runtime, RunArgs};
use agent::pair::identity_paths;
use clap::{Subcommand, ValueEnum};
use proto::fsutil::{open_log, write_secret_atomic};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const LOG_MAX: u64 = 10 * 1024 * 1024;

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Installe le service (démarrage automatique différé, redémarrage après plantage). Administrateur requis.
    Install(RunArgs),
    /// Arrête et supprime le service.
    Uninstall,
    Start,
    Stop,
    Status,
    /// Lancé par le gestionnaire de services : ne pas utiliser à la main.
    #[command(hide = true)]
    Run(RunArgs),
}

pub fn handle(data_dir: PathBuf, name: &str, cmd: ServiceCmd) -> anyhow::Result<()> {
    match cmd {
        ServiceCmd::Install(a) => install(&data_dir, name, &a),
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

/// Arguments de lancement du service : chemins ABSOLUS (le SCM démarre dans System32) et aucun secret
/// (la configuration d'un service est lisible par les utilisateurs locaux).
pub fn launch_args(data_dir: &Path, name: &str, a: &RunArgs, root: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        "--data-dir".into(),
        data_dir.into(),
        "service".into(),
        "--service-name".into(),
        name.into(),
        "run".into(),
        "--root".into(),
        root.into(),
        "--client".into(),
        a.client
            .to_possible_value()
            .map(|p| p.get_name().to_owned())
            .unwrap_or_default()
            .into(),
    ];
    if let Some(u) = &a.client_url {
        v.extend(["--client-url".into(), u.into()]);
    }
    if let Some(u) = &a.client_user {
        v.extend(["--client-user".into(), u.into()]);
    }
    v
}

fn install(data_dir: &Path, name: &str, a: &RunArgs) -> anyhow::Result<()> {
    let data_dir = std::path::absolute(data_dir)?;
    let (key, ..) = identity_paths(&data_dir);
    anyhow::ensure!(
        key.exists(),
        "agent non appairé : lancez d'abord `agent pair <host:port> <CODE>` avec ce --data-dir"
    );
    let root = std::path::absolute(&a.root)?;
    anyhow::ensure!(
        root.is_dir(),
        "le dossier de téléchargement {} n'existe pas",
        root.display()
    );
    // Le mot de passe du client est mis dans un fichier à ACL restreinte (un service n'hérite pas de
    // l'environnement de l'utilisateur, et ses arguments sont lisibles par tous les comptes locaux).
    if let Ok(pw) = std::env::var("AGENT_CLIENT_PASSWORD") {
        write_secret_atomic(&data_dir.join("client_password"), pw.as_bytes())?;
        println!("Mot de passe du client enregistré dans un fichier protégé.");
    }
    winsvc::install(&winsvc::InstallSpec {
        name: name.to_owned(),
        display_name: "nxtTorrent Agent".to_owned(),
        description: "Agent de l'orchestrateur de clients torrent (tunnel mTLS vers le master)."
            .to_owned(),
        exe: std::env::current_exe()?,
        args: launch_args(&data_dir, name, a, &root),
    })?;
    println!("Service {name} installé. Démarrez-le avec : agent service start");
    Ok(())
}

fn run_service(data_dir: PathBuf, name: &str, a: RunArgs) -> anyhow::Result<()> {
    // Pas de console sous le SCM : journal dans un fichier du répertoire de données.
    if let Ok(f) = open_log(&data_dir.join("agent.log"), LOG_MAX) {
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
        let res = rt.block_on(run_agent(
            &data_dir,
            a,
            async move { stop.notified().await },
        ));
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
    use agent::drivers::ClientKind;

    #[test]
    fn launch_args_are_absolute_secret_free_and_complete() {
        let a = RunArgs {
            root: "D:\\Downloads".into(),
            client: ClientKind::Transmission,
            client_url: Some("http://127.0.0.1:9999".into()),
            client_user: Some("admin".into()),
            restart_after_disconnected_secs: None,
        };
        let v = launch_args(
            Path::new("C:\\nxt\\data"),
            "nxt-agent",
            &a,
            Path::new("D:\\Downloads"),
        );
        let s: Vec<String> = v.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert_eq!(
            s,
            [
                "--data-dir",
                "C:\\nxt\\data",
                "service",
                "--service-name",
                "nxt-agent",
                "run",
                "--root",
                "D:\\Downloads",
                "--client",
                "transmission",
                "--client-url",
                "http://127.0.0.1:9999",
                "--client-user",
                "admin"
            ]
        );
        assert!(
            !s.iter().any(|x| x.to_lowercase().contains("password")),
            "aucun secret en argument"
        );
    }
}
