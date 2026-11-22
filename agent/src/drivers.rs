//! Fabrique de drivers : choisit l'implémentation de `TorrentClient` selon le client configuré.
use crate::client::{ClientError, TorrentClient};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ClientKind {
    Qbittorrent,
    Transmission,
    Rutorrent,
    Deluge,
}

impl ClientKind {
    /// URL locale par défaut de l'interface du client.
    pub fn default_url(self) -> &'static str {
        match self {
            Self::Qbittorrent | Self::Rutorrent => "http://127.0.0.1:8080",
            Self::Transmission => "http://127.0.0.1:9091",
            Self::Deluge => "http://127.0.0.1:8112",
        }
    }

    pub fn default_user(self) -> &'static str {
        match self {
            Self::Qbittorrent => "admin",
            // Deluge n'a pas de compte ; Transmission et ruTorrent : authentification HTTP optionnelle.
            Self::Transmission | Self::Rutorrent | Self::Deluge => "",
        }
    }
}

/// Construit le driver. L'URL doit être sur la boucle locale (vérifié par chaque driver).
pub fn build(
    kind: ClientKind,
    url: &str,
    user: &str,
    password: &str,
    root: PathBuf,
) -> Result<Arc<dyn TorrentClient>, ClientError> {
    Ok(match kind {
        ClientKind::Qbittorrent => {
            Arc::new(crate::qbit::QbitClient::new(url, user, password, root)?)
        }
        ClientKind::Transmission => Arc::new(crate::transmission::TransmissionClient::new(
            url, user, password, root,
        )?),
        ClientKind::Rutorrent => Arc::new(crate::rutorrent::RutorrentClient::new(
            url, user, password, root,
        )?),
        ClientKind::Deluge => Arc::new(crate::deluge::DelugeClient::new(url, password, root)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_every_kind_with_its_default_url() {
        for k in [
            ClientKind::Qbittorrent,
            ClientKind::Transmission,
            ClientKind::Rutorrent,
            ClientKind::Deluge,
        ] {
            let c = build(k, k.default_url(), k.default_user(), "x", ".".into()).unwrap();
            let want = match k {
                ClientKind::Qbittorrent => "qbittorrent",
                ClientKind::Transmission => "transmission",
                ClientKind::Rutorrent => "rutorrent",
                ClientKind::Deluge => "deluge",
            };
            assert_eq!(c.kind(), want);
        }
    }

    #[test]
    fn no_driver_accepts_a_remote_url() {
        for k in [
            ClientKind::Qbittorrent,
            ClientKind::Transmission,
            ClientKind::Rutorrent,
            ClientKind::Deluge,
        ] {
            assert!(
                build(k, "http://203.0.113.7:8080", "", "x", ".".into()).is_err(),
                "{k:?}"
            );
        }
    }
}
