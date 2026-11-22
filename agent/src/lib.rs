//! Agent : appairage, (phase 2+) tunnel, réconciliation, drivers.
#![forbid(unsafe_code)]

pub mod client;
pub mod deluge;
pub mod drivers;
pub mod exec;
pub mod jobs;
pub mod links;
pub mod outbox;
pub mod pair;
pub mod paths;
pub mod qbit;
pub mod rutorrent;
pub mod tls;
pub mod transmission;
pub mod tunnel;
pub mod watcher;
