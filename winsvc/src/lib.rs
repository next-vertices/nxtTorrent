//! Service Windows natif : installation, désinstallation, démarrage/arrêt et exécution sous le
//! gestionnaire de services (SCM). Sur les autres plateformes, toutes les fonctions renvoient
//! [`Error::Unsupported`].
//!
//! Le reste du projet est `forbid(unsafe_code)` : seul le point d'entrée FFI exigé par le SCM
//! (macro `define_windows_service!`) est `unsafe`, et il est confiné au module `imp`.
#![deny(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("les services Windows ne sont disponibles que sous Windows")]
    Unsupported,
    #[error("{0}")]
    Windows(String),
    #[error("le service doit être lancé par le gestionnaire de services Windows, pas depuis une console (utilisez `service install` puis `service start`)")]
    NotUnderScm,
}

/// Description d'un service à installer.
#[derive(Debug, Clone)]
pub struct InstallSpec {
    pub name: String,
    pub display_name: String,
    pub description: String,
    /// Exécutable lancé par le SCM (chemin absolu).
    pub exe: PathBuf,
    /// Arguments passés à l'exécutable (le SCM n'a pas de répertoire courant utile : chemins absolus).
    pub args: Vec<OsString>,
}

#[cfg(windows)]
mod imp;
#[cfg(not(windows))]
mod imp {
    use super::{Arc, Error, InstallSpec, Notify};
    pub fn install(_: &InstallSpec) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    pub fn uninstall(_: &str) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    pub fn start(_: &str) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    pub fn stop(_: &str) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    pub fn status(_: &str) -> Result<String, Error> {
        Err(Error::Unsupported)
    }
    pub fn run(
        _: &str,
        _: Box<dyn FnOnce(Arc<Notify>) -> Result<(), String> + Send>,
    ) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// Installe le service : démarrage automatique (différé), compte LocalSystem, redémarrage
/// automatique en cas de plantage (5 s, 30 s, 60 s).
pub fn install(spec: &InstallSpec) -> Result<(), Error> {
    imp::install(spec)
}

/// Arrête (si besoin) puis supprime le service.
pub fn uninstall(name: &str) -> Result<(), Error> {
    imp::uninstall(name)
}

pub fn start(name: &str) -> Result<(), Error> {
    imp::start(name)
}

/// Demande l'arrêt et attend qu'il soit effectif (30 s au plus).
pub fn stop(name: &str) -> Result<(), Error> {
    imp::stop(name)
}

/// État courant, ex. `Running (pid 1234)`, `Stopped (pid -)`, `StartPending (pid 812)`.
pub fn status(name: &str) -> Result<String, Error> {
    imp::status(name)
}

/// Point d'entrée sous le SCM. `entry` reçoit un `Notify` déclenché à la demande d'arrêt ou d'extinction
/// de Windows ; il doit rendre la main une fois l'arrêt effectué. Bloque jusqu'à la fin du service.
pub fn run<F>(name: &str, entry: F) -> Result<(), Error>
where
    F: FnOnce(Arc<Notify>) -> Result<(), String> + Send + 'static,
{
    imp::run(name, Box::new(entry))
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_off_windows() {
        assert!(matches!(status("x"), Err(Error::Unsupported)));
        assert!(matches!(run("x", |_| Ok(())), Err(Error::Unsupported)));
    }
}
