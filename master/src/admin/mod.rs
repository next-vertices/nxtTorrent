//! API d'administration (JSON, HTTP) : listener SÉPARÉ de celui des agents, lié à 127.0.0.1 par défaut.
//!
//! Sécurité : compte créé au premier démarrage (depuis la machine locale uniquement), mot de passe
//! Argon2id, anti brute-force par IP et par compte, sessions courtes en cookie `HttpOnly`/`SameSite=Strict`,
//! protection CSRF (en-tête personnalisé + contrôle de l'origine), audit de chaque action.
pub mod api;
pub mod describe;
pub mod install;
pub mod password;
pub mod session;
pub mod totp;
pub mod ui;

use crate::Core;
use std::future::Future;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct AdminSettings {
    /// Échecs de connexion tolérés par IP dans la fenêtre avant blocage.
    pub max_failures_per_ip: usize,
    /// Échecs tolérés par compte (toutes IP confondues) : plus haut que par IP, pour qu'un attaquant
    /// ne puisse pas verrouiller durablement l'administrateur légitime depuis n'importe où.
    pub max_failures_per_user: usize,
    pub lockout_window: Duration,
    /// Déconnexion après cette durée sans activité.
    pub idle_timeout: Duration,
    /// Durée de vie maximale d'une session, activité ou non.
    pub absolute_timeout: Duration,
    /// Autorise une adresse d'écoute non locale (à placer derrière un proxy TLS).
    pub allow_remote: bool,
    /// Autorise la création du premier compte depuis une adresse non locale. Nécessaire dans Docker, où
    /// l'hôte apparaît au conteneur comme une machine distante ; à n'activer que si le port est publié
    /// sur 127.0.0.1 de l'hôte (ou protégé autrement) : tant qu'aucun compte n'existe, QUICONQUE atteint
    /// le port peut le créer.
    pub allow_remote_setup: bool,
    /// Port et noms sous lesquels les agents joignent ce master (pour pré-remplir le script d'installation).
    pub agent_port: u16,
    pub agent_hosts: Vec<String>,
    /// Binaire de l'agent téléchargeable par le script d'installation (https + SHA-256) ; vide = à fournir à la main.
    pub agent_binary_url: String,
    pub agent_binary_sha256: String,
    /// Dossier de données du master (sauvegarde depuis l'interface) ; `None` = indisponible (tests en mémoire).
    pub data_dir: Option<std::path::PathBuf>,
}

impl Default for AdminSettings {
    fn default() -> Self {
        Self {
            max_failures_per_ip: 5,
            max_failures_per_user: 10,
            lockout_window: Duration::from_secs(15 * 60),
            idle_timeout: Duration::from_secs(15 * 60),
            absolute_timeout: Duration::from_secs(8 * 3600),
            allow_remote: false,
            allow_remote_setup: false,
            agent_port: 7443,
            agent_hosts: Vec::new(),
            agent_binary_url: String::new(),
            agent_binary_sha256: String::new(),
            data_dir: None,
        }
    }
}

/// Refuse par défaut une adresse d'écoute non locale : l'API d'admin ne doit pas être exposée au
/// réseau par accident. `allow_remote` est un choix explicite (ex. conteneur, derrière un proxy TLS).
pub fn check_bind(addr: &str, allow_remote: bool) -> anyhow::Result<SocketAddr> {
    let sa = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("adresse d'écoute de l'API admin invalide : {addr}"))?;
    let local = match sa.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    };
    anyhow::ensure!(
        local || allow_remote,
        "l'API d'administration n'écoute par défaut que sur la boucle locale ; {addr} l'exposerait au réseau. \
         Utilisez --admin-allow-remote (MASTER_ADMIN_ALLOW_REMOTE=1) en connaissance de cause, derrière un proxy TLS."
    );
    Ok(sa)
}

pub async fn serve(
    core: Arc<Core>,
    listener: tokio::net::TcpListener,
    settings: AdminSettings,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let app = api::router(api::AdminState::new(core, settings));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_bind_is_refused_unless_explicitly_allowed() {
        assert!(check_bind("127.0.0.1:7080", false).is_ok());
        assert!(check_bind("[::1]:7080", false).is_ok());
        assert!(check_bind("0.0.0.0:7080", false).is_err());
        assert!(check_bind("192.168.1.10:7080", false).is_err());
        assert!(check_bind("0.0.0.0:7080", true).is_ok());
        assert!(check_bind("pas-une-adresse", true).is_err());
    }
}
