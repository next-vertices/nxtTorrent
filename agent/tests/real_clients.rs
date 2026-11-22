//! Conformité des drivers contre de VRAIS clients torrent (conteneurs Docker).
//!
//! ```text
//! docker compose -f testkit/docker/compose.yml up -d
//! QBIT_PASS=<mot de passe temporaire des logs> cargo test -p agent --test real_clients -- --ignored --test-threads=1
//! ```
//! Variables : QBIT_URL/QBIT_USER/QBIT_PASS, TRANSMISSION_URL/_USER/_PASS, DELUGE_URL/DELUGE_PASS,
//! RUTORRENT_URL/_USER/_PASS. Les chemins sont ceux vus par le conteneur (`/downloads`).
mod common;
use common::{conformance, counters_conformance, env, file_conformance};
use std::path::PathBuf;

#[tokio::test]
#[ignore = "nécessite le conteneur qBittorrent (testkit/docker/compose.yml)"]
async fn real_qbittorrent() {
    let c = agent::qbit::QbitClient::new(
        &env("QBIT_URL", "http://127.0.0.1:18080"),
        &env("QBIT_USER", "admin"),
        &env("QBIT_PASS", ""),
        PathBuf::from("/downloads"),
    )
    .unwrap();
    conformance(&c, true).await;
    file_conformance(&c).await;
    counters_conformance(&c).await;
}

#[tokio::test]
#[ignore = "nécessite le conteneur Transmission (testkit/docker/compose.yml)"]
async fn real_transmission() {
    let c = agent::transmission::TransmissionClient::new(
        &env("TRANSMISSION_URL", "http://127.0.0.1:19092"),
        &env("TRANSMISSION_USER", "admin"),
        &env("TRANSMISSION_PASS", "adminadmin"),
        PathBuf::from("/downloads"),
    )
    .unwrap();
    conformance(&c, true).await;
    file_conformance(&c).await;
    counters_conformance(&c).await;
}

#[tokio::test]
#[ignore = "nécessite le conteneur Deluge (testkit/docker/compose.yml)"]
async fn real_deluge() {
    let c = agent::deluge::DelugeClient::new(
        &env("DELUGE_URL", "http://127.0.0.1:18112"),
        &env("DELUGE_PASS", "deluge"),
        PathBuf::from("/downloads"),
    )
    .unwrap();
    conformance(&c, true).await;
    file_conformance(&c).await;
    counters_conformance(&c).await;
}

#[tokio::test]
#[ignore = "nécessite le conteneur ruTorrent (testkit/docker/compose.yml)"]
async fn real_rutorrent() {
    let c = agent::rutorrent::RutorrentClient::new(
        &env("RUTORRENT_URL", "http://127.0.0.1:18081"),
        &env("RUTORRENT_USER", ""),
        &env("RUTORRENT_PASS", ""),
        PathBuf::from("/downloads"),
    )
    .unwrap();
    // rTorrent ne supprime pas les fichiers : l'agent refuse explicitement plutôt que d'exécuter une commande.
    conformance(&c, false).await;
    file_conformance(&c).await;
    counters_conformance(&c).await;
}
