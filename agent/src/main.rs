#![forbid(unsafe_code)]
#[cfg(windows)]
mod winservice;

use agent::drivers::ClientKind;
use agent::pair::{pair, PairOptions};
use clap::{Args, Parser, Subcommand};
use std::future::Future;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "agent", version)]
struct Cli {
    /// Répertoire d'identité (clé, certificat, CA épinglée).
    #[arg(long, env = "AGENT_DATA_DIR", default_value = "./agent-data")]
    data_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Réglages du pilotage du client torrent (partagés par `run` et le service Windows).
#[derive(Args, Clone)]
struct RunArgs {
    /// Racine locale des téléchargements (tous les chemins du master y sont relatifs).
    #[arg(long, env = "AGENT_ROOT")]
    root: PathBuf,
    /// Client torrent piloté.
    #[arg(long, value_enum, default_value = "qbittorrent", env = "AGENT_CLIENT")]
    client: ClientKind,
    /// Interface du client (doit être sur la boucle locale ; défaut selon le client).
    #[arg(long, env = "AGENT_CLIENT_URL")]
    client_url: Option<String>,
    /// Utilisateur du client (défaut selon le client ; Deluge n'en a pas).
    #[arg(long, env = "AGENT_CLIENT_USER")]
    client_user: Option<String>,
    /// Quitter (pour que Docker/systemd relance l'agent) si le master reste injoignable aussi longtemps, en secondes.
    /// Utile quand l'agent partage la pile réseau d'un autre conteneur (perdue si celui-ci est recréé).
    #[arg(long, env = "AGENT_RESTART_AFTER_DISCONNECTED_SECS")]
    restart_after_disconnected_secs: Option<u64>,
    // Le mot de passe se lit dans AGENT_CLIENT_PASSWORD, sinon dans <data-dir>/client_password :
    // jamais en argument (visible dans `ps` / dans la configuration du service).
}

#[cfg(unix)]
#[derive(Subcommand)]
enum SelfUpdateCmd {
    Enable,
    Disable,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mise à jour automatique (Linux + systemd) : installe ou retire le service root. À lancer une fois, en root.
    #[cfg(unix)]
    SelfUpdate {
        #[arg(long, default_value = "nxt-agent.service")]
        service: String,
        #[command(subcommand)]
        cmd: SelfUpdateCmd,
    },
    /// Interne : exécuté par le service root d'installation.
    #[cfg(unix)]
    #[command(hide = true)]
    ApplyUpdate {
        #[arg(long, default_value = "nxt-agent.service")]
        service: String,
    },
    /// Démarre le tunnel vers le master (connexion sortante uniquement).
    Run(RunArgs),
    /// Appairage unique avec le master.
    Pair {
        /// host:port du master
        addr: String,
        /// Code CODE-XXXX fourni par le master
        code: String,
        /// Nom affiché pour ce nœud (défaut : nom de la machine ; dans Docker, ce serait l'identifiant du conteneur).
        #[arg(long, env = "AGENT_HOSTNAME")]
        hostname: Option<String>,
        /// Remplace l'identité existante (certificat expiré ou corrompu). Les labels sont fixés
        /// par l'admin lors de la création du code, pas par l'agent.
        #[arg(long)]
        force: bool,
    },
    /// Crée un fichier .torrent à partir de fichiers locaux (aucun master ni client torrent requis).
    CreateTorrent {
        /// Fichier ou dossier à partager.
        path: std::path::PathBuf,
        /// Fichier .torrent à écrire.
        #[arg(short, long)]
        out: std::path::PathBuf,
        /// Adresse du tracker (http, https ou udp). Répétable : chaque adresse forme un palier, essayés dans l'ordre.
        #[arg(long = "tracker")]
        trackers: Vec<String>,
        /// Torrent privé (pas de DHT ni d'échange de pairs) ; exige au moins un tracker.
        #[arg(long)]
        private: bool,
        /// Étiquette `source` (trackers privés : distingue l'infohash d'un tracker à l'autre).
        #[arg(long)]
        source: Option<String>,
        /// Taille d'une pièce en octets (puissance de deux de 16 Kio à 16 Mio) ; défaut : choisie selon la taille.
        #[arg(long)]
        piece_length: Option<u32>,
        /// Nom du torrent (défaut : celui du fichier ou du dossier).
        #[arg(long)]
        name: Option<String>,
        /// Commentaire.
        #[arg(long)]
        comment: Option<String>,
        /// Remplace le fichier de sortie s'il existe.
        #[arg(long)]
        force: bool,
    },
    /// Service Windows natif (installation, démarrage, arrêt).
    #[cfg(windows)]
    Service {
        /// Nom du service (changer uniquement pour avoir plusieurs instances ou pour les tests).
        #[arg(long, default_value = "nxt-agent")]
        service_name: String,
        #[command(subcommand)]
        cmd: winservice::ServiceCmd,
    },
}

/// `agent create-torrent` : crée un .torrent (hachage des pièces) et l'écrit en 0600 (l'adresse d'un tracker privé
/// peut contenir un passkey).
#[allow(clippy::too_many_arguments)]
fn create_torrent_cmd(
    path: &std::path::Path,
    out: &std::path::Path,
    trackers: &[String],
    private: bool,
    source: Option<String>,
    piece_length: Option<u32>,
    name: Option<String>,
    comment: Option<String>,
    force: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        force || !out.exists(),
        "{} existe déjà : choisissez un autre nom ou ajoutez --force",
        out.display()
    );
    let opts = proto::create::CreateOptions {
        piece_length,
        private,
        source,
        name,
    };
    // Valide les adresses AVANT de lire des gigaoctets.
    anyhow::ensure!(
        trackers.len() <= proto::metainfo::MAX_TRACKERS_CREATED
            && trackers
                .iter()
                .all(|t| proto::metainfo::valid_tracker_url(t)),
        "adresse de tracker invalide (http, https ou udp ; {} au plus)",
        proto::metainfo::MAX_TRACKERS_CREATED
    );
    anyhow::ensure!(
        !(private && trackers.is_empty()),
        "un torrent privé exige au moins un tracker (--tracker)"
    );
    let tiers: Vec<Vec<String>> = trackers.iter().map(|t| vec![t.clone()]).collect();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut last = 101u64;
    let base = proto::create::create(path, &opts, &cancel, &mut |done, total| {
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        if pct != last && (pct % 5 == 0 || pct == 100) {
            last = pct;
            eprintln!("hachage : {pct} %");
        }
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok());
    let full = proto::metainfo::compose(
        &base,
        &tiers,
        comment.as_deref(),
        concat!("nxtTorrent ", env!("CARGO_PKG_VERSION")),
        now,
    )?;
    let meta = proto::metainfo::parse(&full)?;
    proto::fsutil::write_secret(out, &full)?;
    println!(
        "{} : {} ({} fichier{}, {} octets), infohash {}",
        out.display(),
        meta.name,
        meta.file_count,
        if meta.file_count > 1 { "s" } else { "" },
        meta.size,
        meta.infohash
    );
    println!("magnet:?xt=urn:btih:{}", meta.infohash);
    Ok(())
}

fn init_console_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

/// Pilote le client torrent et maintient le tunnel jusqu'à `shutdown`.
async fn run_agent(
    data_dir: &Path,
    a: RunArgs,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    tokio::pin!(shutdown);
    // Non appairé : on ATTEND (au lieu de quitter) que l'identité apparaisse dans le dossier de données.
    // `agent pair`, lancé à côté (ex. `docker compose run --rm agent pair …`), l'y écrit ; l'agent démarre seul.
    let (key, ..) = agent::pair::identity_paths(data_dir);
    if !key.exists() {
        tracing::info!(
            "agent non appairé : en attente de `agent pair <host:port> <CODE>` (dossier {})",
            data_dir.display()
        );
        let mut waited = 0u32;
        while !key.exists() {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
            }
            waited += 2;
            if waited.is_multiple_of(60) {
                tracing::info!("toujours en attente d'appairage");
            }
        }
        tracing::info!("identité détectée : démarrage");
    }
    let password =
        proto::fsutil::read_secret("AGENT_CLIENT_PASSWORD", &data_dir.join("client_password"))
            .unwrap_or_default();
    let url = a
        .client_url
        .unwrap_or_else(|| a.client.default_url().to_owned());
    let user = a
        .client_user
        .unwrap_or_else(|| a.client.default_user().to_owned());
    let client = agent::drivers::build(a.client, &url, &user, &password, a.root.clone())?;
    let (tx, _rx) = tokio::sync::watch::channel(agent::tunnel::TunnelStatus::Connecting);
    let mut cfg = agent::tunnel::TunnelConfig::new(data_dir.to_owned());
    cfg.client = Some(client);
    cfg.root = a.root;
    // Posé par `self-update enable` (Linux) : un service root installe les releases signées.
    if cfg!(target_os = "linux") && std::env::var("AGENT_SELF_UPDATE").is_ok_and(|v| v == "1") {
        cfg.self_update = Some(agent::update::SelfUpdate::production());
    }
    cfg.exit_after_disconnected = a
        .restart_after_disconnected_secs
        .map(std::time::Duration::from_secs);
    agent::tunnel::run(cfg, tx, shutdown.as_mut()).await?;
    Ok(())
}

/// Attend une demande d'arrêt : Ctrl+C (SIGINT) ou, sous Unix, SIGTERM (envoyé par systemd, Docker,
/// `kill`…). Sans SIGTERM, `systemctl stop` tuerait le processus sans fermeture propre.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        #[cfg(unix)]
        Cmd::SelfUpdate { service, cmd } => {
            use proto::install::linux;
            // Sans `--data-dir` : celui de l'unité systemd livrée.
            let dir = if cli.data_dir == Path::new("./agent-data") {
                PathBuf::from("/var/lib/nxt-agent")
            } else {
                cli.data_dir
            };
            match cmd {
                SelfUpdateCmd::Enable => linux::enable(&agent::update::APP, &dir, &service),
                SelfUpdateCmd::Disable => linux::disable(&agent::update::APP, &dir, &service),
            }
            .map_err(anyhow::Error::msg)?;
        }
        #[cfg(unix)]
        Cmd::ApplyUpdate { service } => {
            init_console_logging();
            let current = proto::version::Version::parse(env!("CARGO_PKG_VERSION"))
                .ok_or_else(|| anyhow::anyhow!("version illisible"))?;
            proto::install::linux::run_apply(&agent::update::APP, current, &cli.data_dir, &service)
                .map_err(anyhow::Error::msg)?;
        }
        Cmd::Run(a) => {
            init_console_logging();
            runtime()?.block_on(run_agent(&cli.data_dir, a, shutdown_signal()))?;
        }
        Cmd::CreateTorrent {
            path,
            out,
            trackers,
            private,
            source,
            piece_length,
            name,
            comment,
            force,
        } => create_torrent_cmd(
            &path,
            &out,
            &trackers,
            private,
            source,
            piece_length,
            name,
            comment,
            force,
        )?,
        Cmd::Pair {
            addr,
            code,
            hostname,
            force,
        } => {
            init_console_logging();
            let hostname = hostname
                .or_else(|| std::env::var("HOSTNAME").ok())
                .or_else(|| std::env::var("COMPUTERNAME").ok())
                .unwrap_or_else(|| "unknown".to_owned());
            let p = runtime()?.block_on(pair(PairOptions {
                addr,
                code,
                data_dir: cli.data_dir,
                hostname,
                labels: vec![],
                force,
            }))?;
            println!("Appairé. node_id = {}", p.node_id);
        }
        #[cfg(windows)]
        Cmd::Service { service_name, cmd } => winservice::handle(cli.data_dir, &service_name, cmd)?,
    }
    Ok(())
}
