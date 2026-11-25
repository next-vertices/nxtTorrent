//! Faux services HTTP pour les tests : qBittorrent WebUI API v2 et récepteur de webhook.
#![forbid(unsafe_code)]
pub mod deluge;
pub mod qbit;
pub mod releases;
pub mod rtorrent;
pub mod signed;
pub mod torrent;
pub mod torznab;
pub mod transmission;
pub mod webhook;
