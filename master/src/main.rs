#![forbid(unsafe_code)]
#[cfg(windows)]
mod winservice;

use clap::{Args, Parser, Subcommand};
use master::{service, Core, PAIRING_CODE_TTL};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "master", version)]
struct Cli {
    /// Répertoire de données (CA, base SQLite).
    #[arg(long, env = "MASTER_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Réglages du listener agents (partagés par `serve` et le service Windows).
#[derive(Args, Clone)]
struct ServeArgs {
    #[arg(long, env = "MASTER_LISTEN", default_value = "0.0.0.0:7443")]
    listen: String,
    /// Noms/IP sous lesquels les agents joignent ce master (SAN du certificat serveur).
    /// Variable d'environnement : MASTER_SANS, valeurs séparées par des virgules.
    #[arg(
        long = "san",
        env = "MASTER_SANS",
        value_delimiter = ',',
        required = true
    )]
    sans: Vec<String>,
    /// Adresse de l'API d'administration (listener séparé de celui des agents). Boucle locale par défaut.
    #[arg(long, env = "MASTER_ADMIN_LISTEN", default_value = "127.0.0.1:7080")]
    admin_listen: String,
    /// Autorise une adresse d'administration non locale (conteneur, derrière un proxy TLS). À éviter sinon.
    /// Autorise la création du premier compte depuis une adresse non locale (Docker : l'hôte est « distant »).
    /// À réserver aux ports publiés sur 127.0.0.1 : tant qu'aucun compte n'existe, tout accès au port suffit.
    #[arg(
        long,
        env = "MASTER_ADMIN_ALLOW_REMOTE_SETUP",
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_missing_value = "true",
        default_value = "false"
    )]
    admin_allow_remote_setup: bool,
    /// URL https d'un binaire d'agent (linux) que le script d'installation téléchargera ; exige l'empreinte SHA-256.
    /// Parc mixte (amd64 et arm64) : mettez `{arch}` dans l'URL (remplacé par amd64 ou arm64 selon la machine).
    #[arg(long, env = "MASTER_AGENT_BINARY_URL", default_value = "")]
    agent_binary_url: String,
    /// Empreinte SHA-256 (64 hexadécimaux, minuscules) du binaire ci-dessus : le script refuse un binaire différent.
    /// Avec `{arch}` : une empreinte PAR architecture, `amd64=<sha>,arm64=<sha>` (voir deploy/release/master-env.sh).
    #[arg(long, env = "MASTER_AGENT_BINARY_SHA256", default_value = "")]
    agent_binary_sha256: String,
    /// Active la réplication entre nœuds par le protocole BitTorrent : adresse d'écoute du tracker PRIVÉ (ex.
    /// 0.0.0.0:7444). Les nœuds doivent pouvoir la joindre. Vide = désactivé (stockage partagé supposé pour les copies).
    #[arg(long, env = "MASTER_TRACKER_LISTEN", default_value = "")]
    tracker_listen: String,
    /// Adresse `http://hôte[:port]` du tracker telle que les NŒUDS la joignent. Par défaut : premier `--san` et le
    /// port de `--tracker-listen`.
    #[arg(long, env = "MASTER_TRACKER_URL", default_value = "")]
    tracker_url: String,
    // Accepte 1/true/yes/on (clap n'accepte que true/false par défaut pour une variable d'environnement).
    #[arg(
        long,
        env = "MASTER_ADMIN_ALLOW_REMOTE",
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_missing_value = "true",
        default_value = "false"
    )]
    admin_allow_remote: bool,
    /// URL de webhook notifiée des transitions online/offline confirmées. Peut contenir un secret :
    /// préférer MASTER_WEBHOOK_URL ou le fichier <data-dir>/webhook_url (jamais loggée).
    #[arg(long, env = "MASTER_WEBHOOK_URL", hide_env_values = true)]
    webhook_url: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Démarre le listener agents (mTLS).
    Serve(ServeArgs),
    /// Génère un code d'appairage à usage unique (valable 15 min).
    PairCode {
        /// Label(s) attribué(s) au futur nœud (ex. vpn-ch, ssd). Fixés par l'admin, jamais par l'agent.
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Gestion locale du compte administrateur (récupération : accès au dossier de données = confiance).
    Admin {
        #[command(subcommand)]
        cmd: AdminCmd,
    },
    /// Sonde de santé (HEALTHCHECK Docker, supervision) : code 0 si `GET /api/health` répond 200.
    /// Une image sans shell ni curl ne peut pas faire autrement. Elle interroge l'API d'administration (HTTP) et
    /// non le port des agents : une connexion TCP nue sur un port TLS est un échec de poignée de main.
    Healthcheck {
        #[arg(long, env = "MASTER_ADMIN_LISTEN", default_value = "127.0.0.1:7080")]
        admin_listen: String,
    },
    /// Mise à jour automatique du master (Linux + systemd) : installe ou retire le petit service root qui installe les
    /// releases signées. À lancer une fois, en root.
    #[cfg(unix)]
    SelfUpdate {
        /// Nom de l'unité systemd du master.
        #[arg(long, default_value = "nxt-master.service")]
        service: String,
        #[command(subcommand)]
        cmd: SelfUpdateCmd,
    },
    /// Interne : exécuté par le service root d'installation.
    #[cfg(unix)]
    #[command(hide = true)]
    ApplyUpdate {
        #[arg(long, default_value = "nxt-master.service")]
        service: String,
    },
    /// Interne : remet la base prise avant une mise à jour (master arrêté).
    #[command(hide = true)]
    RestoreSnapshot,
    /// Service Windows natif (installation, démarrage, arrêt).
    #[cfg(windows)]
    Service {
        /// Nom du service (changer uniquement pour avoir plusieurs instances ou pour les tests).
        #[arg(long, default_value = "nxt-master")]
        service_name: String,
        #[command(subcommand)]
        cmd: winservice::ServiceCmd,
    },
}

#[cfg(unix)]
#[derive(Subcommand)]
enum SelfUpdateCmd {
    /// Active la mise à jour depuis l'interface.
    Enable,
    /// La désactive.
    Disable,
}

#[derive(Subcommand)]
enum AdminCmd {
    /// Crée le compte ou remplace son mot de passe. Le mot de passe est lu dans MASTER_ADMIN_PASSWORD, sinon
    /// sur l'entrée standard (jamais en argument de ligne de commande).
    SetPassword {
        #[arg(long, default_value = "admin")]
        username: String,
    },
    /// Désactive la double authentification d'un compte (téléphone perdu ET codes de secours perdus).
    /// Exige un accès local à la machine du master : c'est la voie de récupération.
    DisableTotp {
        #[arg(long, default_value = "admin")]
        username: String,
    },
    /// Sauvegarde chiffrée (base, CA, clés). La phrase de passe est lue dans MASTER_BACKUP_PASSPHRASE, sinon sur
    /// l'entrée standard (jamais en argument). Peut être lancée pendant que le master tourne.
    Backup {
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Restaure une sauvegarde dans le dossier de données. Le master doit être ARRÊTÉ.
    Restore {
        #[arg(long)]
        from: std::path::PathBuf,
        /// Remplace des données existantes.
        #[arg(long)]
        force: bool,
    },
}

fn passphrase() -> anyhow::Result<String> {
    if let Some(p) = std::env::var("MASTER_BACKUP_PASSPHRASE")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Ok(p);
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn backup_cmd(data_dir: &Path, out: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        !out.exists(),
        "{} existe déjà : choisissez un autre nom",
        out.display()
    );
    let store = master::store::Store::open(&data_dir.join("master.db"))?;
    let bytes = master::backup::create(data_dir, &store, &passphrase()?)?;
    proto::fsutil::write_secret(out, &bytes)?;
    println!("Sauvegarde chiffrée écrite : {} ({} octets). Conservez-la HORS de cette machine, avec sa phrase de passe.", out.display(), bytes.len());
    Ok(())
}

fn restore_cmd(data_dir: &Path, from: &Path, force: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(from)?;
    let written = master::backup::restore(data_dir, &bytes, &passphrase()?, force)?;
    println!(
        "Restauré dans {} : {}. Vous pouvez démarrer le master.",
        data_dir.display(),
        written.join(", ")
    );
    Ok(())
}

fn disable_totp(data_dir: &Path, username: &str) -> anyhow::Result<()> {
    let store = master::store::Store::open(&data_dir.join("master.db"))?;
    anyhow::ensure!(
        store.admin_totp_enabled(username)?,
        "la double authentification n'est pas activée pour « {username} »"
    );
    store.admin_totp_disable(username)?;
    store.audit("local-cli", "totp_disabled", username);
    println!("Double authentification désactivée pour {username}.");
    Ok(())
}

fn set_admin_password(data_dir: &Path, username: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        master::admin::api::valid_username(username),
        "nom d'utilisateur invalide (3 à 32 caractères : lettres, chiffres, _ . -)"
    );
    let pw = match std::env::var("MASTER_ADMIN_PASSWORD")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(p) => p,
        None => {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_owned()
        }
    };
    master::admin::password::validate_strength(&pw, username).map_err(|m| anyhow::anyhow!(m))?;
    proto::fsutil::ensure_private_dir(data_dir)?;
    let store = master::store::Store::open(&data_dir.join("master.db"))?;
    let hash = master::admin::password::hash(&pw).map_err(|e| anyhow::anyhow!(e))?;
    store.upsert_admin(username, &hash)?;
    store.audit("local-cli", "admin_password_set", username);
    println!("Mot de passe du compte {username} enregistré.");
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

/// Sert les agents jusqu'à `shutdown`.
async fn run_master(
    data_dir: &Path,
    a: ServeArgs,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let webhook = a.webhook_url.or_else(|| {
        proto::fsutil::read_secret("MASTER_WEBHOOK_URL", &data_dir.join("webhook_url"))
    });
    let core = match webhook {
        Some(u) => {
            let ca = master::ca::Ca::load_or_create(data_dir)?;
            let store = master::store::Store::open(&data_dir.join("master.db"))?;
            let hook = Arc::new(master::webhook::WebhookNotifier::new(&u)?);
            let log = Arc::new(master::notify::LogNotifier);
            Core::with_settings(ca, store, master::Settings::default(), vec![log, hook])
        }
        None => Core::open(data_dir)?,
    };
    // Réglage avancé : période de l'historique du tableau de bord (secondes ; 60 par défaut).
    if let Some(secs) = std::env::var("MASTER_METRICS_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        core.set_metrics_interval(std::time::Duration::from_secs(secs));
    }
    // Réplication par swarm : tracker privé (HTTP clair : les clients torrent ne connaissent pas notre CA ; chaque
    // URL d'annonce est liée à un torrent et un nœud par un jeton).
    let tracker_listener = if a.tracker_listen.is_empty() {
        None
    } else {
        let port = a
            .tracker_listen
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .ok_or_else(|| {
                anyhow::anyhow!("MASTER_TRACKER_LISTEN invalide : {}", a.tracker_listen)
            })?;
        let url = if a.tracker_url.is_empty() {
            let host = a.sans.first().map(String::as_str).unwrap_or("127.0.0.1");
            if host.contains(':') {
                format!("http://[{host}]:{port}")
            } else {
                format!("http://{host}:{port}")
            }
        } else {
            a.tracker_url.clone()
        };
        let tracker = core.enable_swarm(&url)?;
        let l = tokio::net::TcpListener::bind(&a.tracker_listen).await?;
        tracing::info!(listen = %a.tracker_listen, "tracker privé de réplication démarré");
        Some((l, tracker))
    };
    let _sweeper = core.start_background();
    core.enable_agent_dist(master::agentdist::DistConfig::production(data_dir));
    // Mise à jour automatique : seulement si le service d'installation a été mis en place (`self-update enable`).
    if std::env::var("MASTER_SELF_UPDATE").is_ok_and(|v| v == "1") {
        match master::selfupdate::Config::production(data_dir) {
            Some(cfg) => core.enable_self_update(cfg),
            None => {
                tracing::warn!("mise à jour automatique non prise en charge sur cette plateforme");
            }
        }
    }
    // Adresse d'administration vérifiée AVANT d'ouvrir quoi que ce soit : refus franc si non locale.
    let admin_addr = master::admin::check_bind(&a.admin_listen, a.admin_allow_remote)?;
    let listener = tokio::net::TcpListener::bind(&a.listen).await?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
    tracing::info!(listen = %a.listen, "listener agents démarré");
    tracing::info!(admin = %admin_addr, "API d'administration démarrée");
    // Les deux écoutes sont ouvertes : après quelques secondes sans accroc, cette version est déclarée saine.
    core.self_update_mark_healthy_later(master::selfupdate::HEALTHY_AFTER);
    // Un seul signal d'arrêt pour les deux listeners.
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = tx.send(true);
    });
    let stop = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|v| *v).await;
    };
    anyhow::ensure!(
        master::admin::install::valid_binary(&a.agent_binary_url, &a.agent_binary_sha256),
        "MASTER_AGENT_BINARY_URL doit être une URL https accompagnée de MASTER_AGENT_BINARY_SHA256 (64 hexadécimaux minuscules, ou amd64=<sha>,arm64=<sha> si l'URL contient {{arch}})"
    );
    let admin_settings = master::admin::AdminSettings {
        allow_remote: a.admin_allow_remote,
        allow_remote_setup: a.admin_allow_remote_setup,
        agent_port: a
            .listen
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(7443),
        agent_hosts: a.sans.clone(),
        agent_binary_url: a.agent_binary_url.clone(),
        agent_binary_sha256: a.agent_binary_sha256.clone(),
        data_dir: Some(data_dir.to_path_buf()),
        ..master::admin::AdminSettings::default()
    };
    let tracker_task = {
        let stop_t = stop(rx.clone());
        async move {
            match tracker_listener {
                Some((l, t)) => tokio::select! {
                    r = master::tracker::serve(l, t) => r.map_err(anyhow::Error::from),
                    () = stop_t => Ok(()),
                },
                None => Ok(()),
            }
        }
    };
    tokio::try_join!(
        service::serve(core.clone(), listener, &a.sans, stop(rx.clone())),
        master::admin::serve(core, admin_listener, admin_settings, stop(rx)),
        tracker_task,
    )?;
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

/// `GET /api/health` sur l'API d'administration, via la boucle locale (0.0.0.0 / :: sont joints en 127.0.0.1).
fn healthcheck(admin_listen: &str) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
    let addr = admin_listen
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("adresse invalide : {admin_listen}"))?;
    let ip = if addr.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        addr.ip()
    };
    let timeout = std::time::Duration::from_secs(2);
    let mut s = std::net::TcpStream::connect_timeout(&SocketAddr::new(ip, addr.port()), timeout)
        .map_err(|e| anyhow::anyhow!("le master ne répond pas : {e}"))?;
    s.set_read_timeout(Some(timeout))?;
    s.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut head = [0u8; 12];
    s.read_exact(&mut head)?;
    anyhow::ensure!(
        &head == b"HTTP/1.0 200" || &head == b"HTTP/1.1 200",
        "réponse inattendue"
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Admin {
            cmd: AdminCmd::SetPassword { username },
        } => set_admin_password(&cli.data_dir, &username)?,
        Cmd::Admin {
            cmd: AdminCmd::DisableTotp { username },
        } => disable_totp(&cli.data_dir, &username)?,
        Cmd::Admin {
            cmd: AdminCmd::Backup { out },
        } => backup_cmd(&cli.data_dir, &out)?,
        Cmd::Admin {
            cmd: AdminCmd::Restore { from, force },
        } => restore_cmd(&cli.data_dir, &from, force)?,
        Cmd::Healthcheck { admin_listen } => healthcheck(&admin_listen)?,
        Cmd::PairCode { labels } => {
            init_console_logging();
            let core = Core::open(&cli.data_dir)?;
            // Affiché volontairement sur stdout : jamais passé par tracing.
            println!(
                "{}",
                core.create_pairing_code_with_labels(PAIRING_CODE_TTL, &labels)?
            );
        }
        Cmd::Serve(a) => {
            init_console_logging();
            runtime()?.block_on(run_master(&cli.data_dir, a, shutdown_signal()))?;
        }
        #[cfg(unix)]
        Cmd::SelfUpdate { service, cmd } => {
            // Sans `--data-dir` : celui de l'unité systemd livrée, pas le dossier relatif par défaut.
            let data_dir = if cli.data_dir == Path::new("./data") {
                PathBuf::from("/var/lib/nxt-master")
            } else {
                cli.data_dir
            };
            match cmd {
                SelfUpdateCmd::Enable => master::selfupdate::linux::enable(&data_dir, &service)?,
                SelfUpdateCmd::Disable => master::selfupdate::linux::disable(&data_dir, &service)?,
            }
        }
        #[cfg(unix)]
        Cmd::ApplyUpdate { service } => {
            init_console_logging();
            master::selfupdate::linux::run_apply(&cli.data_dir, &service)?;
        }
        Cmd::RestoreSnapshot => master::selfupdate::restore_snapshot(&cli.data_dir)?,
        #[cfg(windows)]
        Cmd::Service { service_name, cmd } => winservice::handle(cli.data_dir, &service_name, cmd)?,
    }
    Ok(())
}
