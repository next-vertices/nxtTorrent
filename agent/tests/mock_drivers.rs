//! Les quatre drivers contre des faux serveurs (aucun Docker requis) : même parcours de conformité
//! que sur les vrais clients, plus les particularités de chaque protocole.
mod common;
use agent::client::{AddRequest, AddSource, ClientError, TorrentClient};
use agent::deluge::DelugeClient;
use agent::exec::Executor;
use agent::rutorrent::RutorrentClient;
use agent::transmission::TransmissionClient;
use common::{conformance, file_conformance, HASH};
use proto::pb::command::Kind;
use proto::pb::{add_cmd::Source, AddCmd, Command};
use std::path::PathBuf;
use std::sync::Arc;
use testkit::deluge::MockDeluge;
use testkit::rtorrent::MockRutorrent;
use testkit::transmission::MockTransmission;

fn root() -> PathBuf {
    PathBuf::from("/downloads")
}

fn req(paused: bool) -> AddRequest {
    AddRequest {
        source: AddSource::Magnet(format!("magnet:?xt=urn:btih:{HASH}")),
        save_path: PathBuf::from("/downloads/conf"),
        paused,
    }
}

#[tokio::test]
async fn transmission_conformance_and_protocol_quirks() {
    let m = MockTransmission::start().await;
    let c = TransmissionClient::new(&m.url(), "admin", "adminadmin", root()).unwrap();
    conformance(&c, true).await;
    file_conformance(&c).await;

    // Pairs, essaim, tracker (hôte seul : jamais le passkey) et santé réseau.
    c.add(req(true)).await.unwrap();
    let l = c.list().await.unwrap().remove(0);
    assert_eq!((l.peers_connected, l.peers_active), (14, 3));
    assert_eq!((l.swarm_seeds, l.swarm_leechers), (100, 20));
    assert_eq!(l.tracker, "tracker.example.org");
    assert!(!format!("{l:?}").contains("SECRET"));
    // Compteurs cumulés (statistiques) : envoyé, téléchargé, date d'ajout.
    assert_eq!(
        (l.uploaded, l.downloaded, l.added_on),
        (5000, 2000, 1_700_000_000)
    );
    let h = c.health().await.unwrap();
    assert!(h.known);
    assert_eq!(
        h.incoming,
        proto::pb::Reachability::Open as i32,
        "connexions entrantes observées"
    );
    assert_eq!(h.listen_port, 51413);

    // Le serveur change d'identifiant de session : le driver reprend le nouveau (409) sans erreur.
    c.add(req(true)).await.unwrap();
    m.rotate_session();
    assert_eq!(c.list().await.unwrap().len(), 1);
    // Ajout dupliqué = succès (idempotence) ; unités : kB/s, 0 = illimité.
    c.add(req(true)).await.unwrap();
    c.set_limits(500_000, 250_000).await.unwrap();
    assert_eq!(m.limits(), (500, true, 250, true));
    c.set_limits(0, 0).await.unwrap();
    assert_eq!(m.limits(), (0, false, 0, false));
    // Mauvais identifiants -> Auth.
    let bad = TransmissionClient::new(&m.url(), "admin", "mauvais", root()).unwrap();
    assert!(matches!(bad.list().await, Err(ClientError::Auth)));
}

#[tokio::test]
async fn deluge_conformance_login_and_reconnect() {
    let m = MockDeluge::start().await;
    let c = DelugeClient::new(&m.url(), "deluge", root()).unwrap();
    conformance(&c, true).await;
    file_conformance(&c).await;
    assert_eq!(m.logins(), 1, "login + web.connect une seule fois");

    // Session expirée : ré-authentification automatique.
    m.expire_session();
    c.list().await.unwrap();
    assert_eq!(m.logins(), 2);

    // Progression en pourcentage côté Deluge, ratio côté protocole ; limites en KiB/s (-1 = illimité).
    c.add(req(false)).await.unwrap();
    assert_eq!(m.get(HASH).unwrap().state, "Downloading");
    let l = c.list().await.unwrap().remove(0);
    assert_eq!(
        (l.uploaded, l.downloaded, l.added_on),
        (7000, 3000, 1_700_000_001)
    );
    c.set_limits(2048, 0).await.unwrap();
    assert_eq!(m.config()["max_download_speed"], 2.0);
    assert_eq!(m.config()["max_upload_speed"], -1.0);
    let bad = DelugeClient::new(&m.url(), "mauvais", root()).unwrap();
    assert!(matches!(bad.list().await, Err(ClientError::Auth)));
}

#[tokio::test]
async fn rutorrent_conformance_and_rtorrent_quirks() {
    let m = MockRutorrent::start().await;
    let c = RutorrentClient::new(&m.url(), "", "", root()).unwrap();
    conformance(&c, false).await;
    file_conformance(&c).await;

    c.add(req(false)).await.unwrap();
    let upper = HASH.to_ascii_uppercase();
    assert!(m.get(&upper).unwrap().active, "load.start démarre");
    // `d.directory` inclut le nom du torrent ; le driver rapporte le dossier demandé à l'ajout.
    assert_eq!(c.list().await.unwrap()[0].save_subdir, "conf");
    // Compteurs cumulés ; rTorrent n'expose pas de date d'ajout fiable (0 = inconnue).
    let l = c.list().await.unwrap().remove(0);
    assert_eq!((l.uploaded, l.downloaded, l.added_on), (9000, 4000, 0));
    // Progression issue de bytes_done / size.
    m.finish(&upper);
    let t = &c.list().await.unwrap()[0];
    assert_eq!(t.progress, 1.0);
    assert_eq!(t.status, proto::pb::TorrentStatus::Seeding as i32);
    c.set_limits(500_000, 0).await.unwrap();
    assert_eq!(m.limits(), (489, 0));
    // Chemin pouvant altérer l'analyse des commandes rTorrent : refusé.
    let mut bad = req(true);
    bad.save_path = PathBuf::from("/downloads/a,b");
    assert!(c.add(bad).await.is_err());
    // Suppression des fichiers : refus explicite (jamais de fichiers conservés en silence).
    assert!(c.remove(HASH, true).await.is_err());
    assert!(m.get(&upper).is_some());
}

/// L'exécuteur (validation des commandes) est indépendant du client : mêmes garanties avec chacun.
#[tokio::test]
async fn executor_guards_work_with_every_driver() {
    let t = MockTransmission::start().await;
    let d = MockDeluge::start().await;
    let r = MockRutorrent::start().await;
    let tmp = tempfile::tempdir().unwrap();
    #[allow(unused_mut)]
    let mut clients: Vec<Arc<dyn TorrentClient>> = vec![
        Arc::new(
            TransmissionClient::new(&t.url(), "admin", "adminadmin", tmp.path().to_owned())
                .unwrap(),
        ),
        Arc::new(DelugeClient::new(&d.url(), "deluge", tmp.path().to_owned()).unwrap()),
    ];
    // rTorrent n'existe que sous Unix : un chemin Windows (antislash) y est refusé par conception.
    #[cfg(unix)]
    clients.push(Arc::new(
        RutorrentClient::new(&r.url(), "", "", tmp.path().to_owned()).unwrap(),
    ));
    let _ = &r;
    for c in clients {
        let kind = c.kind();
        let e = Executor::new(c.clone(), tmp.path().to_owned());
        let add = |subdir: &str| Command {
            cmd_id: "c".into(),
            kind: Some(Kind::Add(AddCmd {
                source: Some(Source::Infohash(HASH.into())),
                save_subdir: subdir.into(),
                start_paused: false,
            })),
        };
        for bad in ["../x", "/etc", "a/../../b"] {
            assert!(e.execute(&add(bad)).await.is_err(), "{kind}: {bad}");
        }
        assert!(
            c.list().await.unwrap().is_empty(),
            "{kind}: rien ne doit atteindre le client"
        );
        e.execute(&add("tv")).await.unwrap();
        e.execute(&add("tv")).await.unwrap(); // rejeu idempotent
        let l = c.list().await.unwrap();
        assert_eq!(l.len(), 1, "{kind}");
        assert_eq!(
            l[0].save_subdir, "tv",
            "{kind}: sous-dossier relatif à la racine"
        );
    }
}
