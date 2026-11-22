//! Parcours de conformité du contrat `TorrentClient`, commun aux tests sur vrais clients (Docker)
//! et sur faux serveurs.
#![allow(dead_code)]
use agent::client::{AddRequest, AddSource, TorrentClient};
use proto::pb::TorrentStatus;
use std::path::PathBuf;
use std::time::Duration;

pub const HASH: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

pub fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_owned())
}

pub async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !f().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "délai dépassé: {what}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

pub async fn status_of(c: &dyn TorrentClient) -> Option<(i32, String)> {
    c.list()
        .await
        .ok()?
        .into_iter()
        .find(|t| t.infohash == HASH)
        .map(|t| (t.status, t.save_subdir))
}

/// Parcours complet du contrat `TorrentClient`, commun à tous les drivers.
pub async fn conformance(c: &dyn TorrentClient, delete_files_supported: bool) {
    println!(
        "client: {} {}",
        c.kind(),
        c.version().await.expect("version")
    );
    assert!(!c.version().await.unwrap().is_empty());
    match c.disk_free().await {
        Ok(n) => println!("  espace libre: {n}"),
        Err(e) => println!("  espace libre indisponible: {e}"),
    }
    // Nettoyage d'un éventuel reste d'un run précédent (et idempotence de remove).
    c.remove(HASH, false)
        .await
        .expect("remove préalable idempotent");

    let magnet = format!("magnet:?xt=urn:btih:{HASH}&dn=conformance");
    c.add(AddRequest {
        source: AddSource::Magnet(magnet),
        save_path: PathBuf::from("/downloads/conf"),
        paused: true,
    })
    .await
    .expect("add");
    eventually("torrent listé", || async { status_of(c).await.is_some() }).await;
    let (st, subdir) = status_of(c).await.unwrap();
    assert_eq!(subdir, "conf", "save_subdir relatif à la racine /downloads");
    println!(
        "  ajouté en pause -> statut {:?}",
        TorrentStatus::try_from(st).unwrap()
    );
    eventually("ajouté EN PAUSE", || async {
        status_of(c)
            .await
            .is_some_and(|(s, _)| s == TorrentStatus::Paused as i32)
    })
    .await;

    // Ajout dupliqué : idempotent côté exécuteur ; le driver ne doit pas planter.
    let _ = c
        .add(AddRequest {
            source: AddSource::Magnet(format!("magnet:?xt=urn:btih:{HASH}")),
            save_path: PathBuf::from("/downloads/conf"),
            paused: true,
        })
        .await;

    c.resume(HASH).await.expect("resume");
    eventually("repris", || async {
        status_of(c)
            .await
            .is_some_and(|(s, _)| s != TorrentStatus::Paused as i32)
    })
    .await;
    c.pause(HASH).await.expect("pause");
    eventually("pausé", || async {
        status_of(c)
            .await
            .is_some_and(|(s, _)| s == TorrentStatus::Paused as i32)
    })
    .await;
    c.recheck(HASH).await.expect("recheck");
    c.set_limits(500_000, 250_000).await.expect("set_limits");
    c.set_limits(0, 0).await.expect("set_limits illimité");

    if delete_files_supported {
        c.remove(HASH, true).await.expect("remove avec fichiers");
    } else {
        assert!(
            c.remove(HASH, true).await.is_err(),
            "suppression de fichiers non supportée : erreur franche"
        );
        c.remove(HASH, false).await.expect("remove");
    }
    eventually("supprimé", || async { status_of(c).await.is_none() }).await;
    c.remove(HASH, false).await.expect("remove idempotent");
    println!("  OK");
}

/// Ajout d'un FICHIER .torrent (indispensable pour les trackers privés), en pause, puis suppression.
pub async fn file_conformance(c: &dyn TorrentClient) {
    let bytes = testkit::torrent::sample("seedbox-file", 4096, 0x42);
    let ih = proto::metainfo::parse(&bytes)
        .expect("torrent de test valide")
        .infohash;
    c.remove(&ih, false)
        .await
        .expect("remove préalable idempotent");
    let req = || AddRequest {
        source: AddSource::File(bytes.clone()),
        save_path: PathBuf::from("/downloads/conf"),
        paused: true,
    };
    c.add(req()).await.expect("add fichier");
    let find = || async {
        c.list()
            .await
            .ok()?
            .into_iter()
            .find(|t| t.infohash == ih)
            .map(|t| (t.status, t.save_subdir))
    };
    eventually("fichier listé", || async { find().await.is_some() }).await;
    let (_, subdir) = find().await.unwrap();
    assert_eq!(subdir, "conf", "save_subdir relatif à la racine");
    // Un client peut passer un instant par « vérification » (qBittorrent 5 : contrôle des données de reprise) avant de se
    // poser en pause : on attend l'état stable, comme pour l'ajout par magnet ci-dessus.
    eventually("ajouté EN PAUSE", || async {
        find()
            .await
            .is_some_and(|(s, _)| s == TorrentStatus::Paused as i32)
    })
    .await;
    let _ = c.add(req()).await; // doublon : ne doit pas planter
    c.remove(&ih, false).await.expect("remove");
    eventually("fichier supprimé", || async { find().await.is_none() }).await;
    println!("  fichier .torrent OK ({})", c.kind());
}
