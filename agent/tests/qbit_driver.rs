use agent::client::{AddRequest, AddSource, ClientError, TorrentClient};
use agent::exec::Executor;
use agent::qbit::QbitClient;
use proto::pb::command::Kind;
use proto::pb::{
    add_cmd::Source, AddCmd, Command, RemoveCmd, SetLimitsCmd, TargetCmd, TorrentStatus,
};
use std::path::Path;
use std::sync::Arc;
use testkit::qbit::{MockQbit, MockTorrent};

const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";

fn client(m: &MockQbit, root: &Path) -> QbitClient {
    QbitClient::new(&m.url(), "admin", "adminadmin", root.to_owned()).unwrap()
}

fn magnet(h: &str) -> String {
    format!("magnet:?xt=urn:btih:{h}&dn=x")
}

fn cmd(kind: Kind) -> Command {
    Command {
        cmd_id: "c1".into(),
        kind: Some(kind),
    }
}

fn add_cmd(m: Source, subdir: &str, paused: bool) -> Command {
    cmd(Kind::Add(AddCmd {
        source: Some(m),
        save_subdir: subdir.into(),
        start_paused: paused,
    }))
}

fn target(h: &str) -> TargetCmd {
    TargetCmd { infohash: h.into() }
}

#[tokio::test]
async fn driver_full_cycle_with_lazy_login() {
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());

    assert_eq!(c.version().await.unwrap(), "v4.6.0");
    assert_eq!(c.disk_free().await.unwrap(), 123_456_789);
    assert_eq!(m.logins(), 1, "un seul login pour plusieurs requêtes");

    let save = agent::paths::simplify(root.path().canonicalize().unwrap()).join("tv");
    c.add(AddRequest {
        source: AddSource::Magnet(magnet(H1)),
        save_path: save,
        paused: false,
    })
    .await
    .unwrap();
    let l = c.list().await.unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].infohash, H1);
    assert_eq!(l[0].status, TorrentStatus::Downloading as i32);
    assert_eq!(l[0].save_subdir, "tv", "chemin exposé relatif à la racine");

    c.pause(H1).await.unwrap();
    assert_eq!(
        c.list().await.unwrap()[0].status,
        TorrentStatus::Paused as i32
    );
    c.resume(H1).await.unwrap();
    assert_eq!(
        c.list().await.unwrap()[0].status,
        TorrentStatus::Downloading as i32
    );
    c.recheck(H1).await.unwrap();
    assert_eq!(
        c.list().await.unwrap()[0].status,
        TorrentStatus::Checking as i32
    );
    c.set_limits(1000, 0).await.unwrap();
    assert_eq!(m.limits(), (1000, 0));
    c.remove(H1, true).await.unwrap();
    assert!(c.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn driver_reauthenticates_when_session_expires() {
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());
    c.list().await.unwrap();
    assert_eq!(m.logins(), 1);
    m.expire_session();
    c.list().await.unwrap();
    assert_eq!(m.logins(), 2, "reconnexion automatique");
}

#[tokio::test]
async fn driver_reauthenticates_on_401_like_recent_qbittorrent() {
    let m = MockQbit::start().await;
    m.set_unauth_401(true);
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());
    c.list().await.unwrap();
    m.expire_session();
    c.list().await.unwrap();
    assert_eq!(m.logins(), 2);
}

#[tokio::test]
async fn driver_falls_back_to_v5_stop_start() {
    let m = MockQbit::start().await;
    m.set_v5(true);
    m.insert(MockTorrent::new(H1, "downloading", 0.2, "/x"));
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());
    c.pause(H1).await.unwrap();
    assert_eq!(m.get(H1).unwrap().state, "pausedDL");
    c.resume(H1).await.unwrap();
    assert_eq!(m.get(H1).unwrap().state, "downloading");
}

#[tokio::test]
async fn driver_wrong_password_is_auth_error() {
    let m = MockQbit::start().await;
    let c = QbitClient::new(&m.url(), "admin", "mauvais", ".".into()).unwrap();
    assert!(matches!(c.list().await, Err(ClientError::Auth)));
    // Le mot de passe ne fuit pas dans le Debug.
    assert!(!format!("{c:?}").contains("mauvais"));
}

#[tokio::test]
async fn driver_unreachable_is_http_error() {
    let c = QbitClient::new("http://127.0.0.1:1", "a", "b", ".".into()).unwrap();
    assert!(matches!(c.list().await, Err(ClientError::Http(_))));
}

fn executor(m: &MockQbit, root: &Path) -> Executor {
    Executor::new(Arc::new(client(m, root)), root.to_owned())
}

#[tokio::test]
async fn executor_runs_closed_command_set_idempotently() {
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let e = executor(&m, root.path());
    let canon = agent::paths::simplify(root.path().canonicalize().unwrap());

    e.execute(&add_cmd(Source::Magnet(magnet(H1)), "movies/hd", false))
        .await
        .unwrap();
    let t = m.get(H1).unwrap();
    assert!(
        Path::new(&t.save_path).starts_with(&canon),
        "{}",
        t.save_path
    );
    // Rejeu : sans effet, sans erreur.
    e.execute(&add_cmd(Source::Magnet(magnet(H1)), "movies/hd", false))
        .await
        .unwrap();
    assert_eq!(m.torrents().len(), 1);

    // Add par infohash seul, en pause.
    e.execute(&add_cmd(Source::Infohash(H2.to_uppercase()), "", true))
        .await
        .unwrap();
    assert_eq!(m.get(H2).unwrap().state, "pausedDL");

    e.execute(&cmd(Kind::Pause(target(H1)))).await.unwrap();
    e.execute(&cmd(Kind::Pause(target(H1)))).await.unwrap();
    assert_eq!(m.get(H1).unwrap().state, "pausedDL");
    e.execute(&cmd(Kind::Resume(target(H1)))).await.unwrap();
    e.execute(&cmd(Kind::Recheck(target(H1)))).await.unwrap();
    e.execute(&cmd(Kind::SetLimits(SetLimitsCmd {
        dl_limit: 5,
        ul_limit: 6,
    })))
    .await
    .unwrap();
    assert_eq!(m.limits(), (5, 6));
    e.execute(&cmd(Kind::Remove(RemoveCmd {
        infohash: H1.into(),
        delete_files: false,
    })))
    .await
    .unwrap();
    e.execute(&cmd(Kind::Remove(RemoveCmd {
        infohash: H1.into(),
        delete_files: false,
    })))
    .await
    .unwrap();
    assert!(m.get(H1).is_none());
}

#[tokio::test]
async fn executor_rejects_malicious_commands_without_touching_client() {
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let e = executor(&m, root.path());

    for subdir in ["../escape", "/etc", "a/../../b", "C:\\Windows", "..\\x"] {
        let r = e
            .execute(&add_cmd(Source::Magnet(magnet(H1)), subdir, false))
            .await;
        assert!(r.is_err(), "{subdir:?} aurait dû être refusé");
    }
    for bad in [
        "http://evil.example/x.torrent",
        "file:///etc/passwd",
        "magnet:?dn=x",
        "magnet:?xt=urn:btih:zz",
    ] {
        assert!(
            e.execute(&add_cmd(Source::Magnet(bad.into()), "", false))
                .await
                .is_err(),
            "{bad}"
        );
    }
    assert!(e
        .execute(&add_cmd(Source::Infohash("../../x".into()), "", false))
        .await
        .is_err());
    assert!(e
        .execute(&cmd(Kind::Pause(target("pas-un-hash"))))
        .await
        .is_err());
    assert!(e
        .execute(&Command {
            cmd_id: "x".into(),
            kind: None
        })
        .await
        .is_err());
    assert!(e
        .execute(&add_cmd(Source::Magnet(magnet(H1)), "", false))
        .await
        .is_ok());
    assert_eq!(
        m.torrents().len(),
        1,
        "seule la commande légitime a atteint le client"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn executor_rejects_symlink_escape() {
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("evil")).unwrap();
    let e = executor(&m, root.path());
    assert!(e
        .execute(&add_cmd(Source::Magnet(magnet(H1)), "evil/sub", false))
        .await
        .is_err());
    assert!(m.torrents().is_empty());
}

#[tokio::test]
async fn driver_uploads_a_torrent_file_to_qbittorrent() {
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());
    let bytes = testkit::torrent::sample("prive", 1000, 9);
    let ih = proto::metainfo::parse(&bytes).unwrap().infohash;
    c.add(AddRequest {
        source: AddSource::File(bytes),
        save_path: std::path::PathBuf::from("/downloads/films"),
        paused: true,
    })
    .await
    .unwrap();
    let t = m
        .get(&ih)
        .expect("le fichier a été reçu et son infohash recalculé");
    assert_eq!(
        (t.state.as_str(), t.save_path.as_str()),
        ("pausedDL", "/downloads/films")
    );
}

#[tokio::test]
async fn driver_reports_peers_swarm_tracker_without_the_passkey_and_network_health() {
    use proto::pb::Reachability;
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());
    let mut t = MockTorrent::new(H1, "uploading", 1.0, &root.path().to_string_lossy());
    t.num_seeds = 3;
    t.num_leechs = 9;
    t.num_complete = 120;
    t.num_incomplete = 45;
    t.upspeed = 1000;
    t.tracker = "https://tracker.example.org:443/SECRETPASSKEY/announce?k=SECRET".into();
    m.insert(t);

    let l = c.list().await.unwrap().remove(0);
    assert_eq!(l.peers_connected, 12);
    assert_eq!(l.peers_active, 12, "débit non nul : activité déduite");
    assert_eq!((l.swarm_seeds, l.swarm_leechers), (120, 45));
    assert_eq!(l.tracker, "tracker.example.org");
    assert!(
        !format!("{l:?}").contains("SECRET"),
        "le passkey ne doit jamais sortir du nœud"
    );

    let h = c.health().await.unwrap();
    assert!(h.known);
    assert_eq!(h.incoming, Reachability::Open as i32);
    assert_eq!(h.listen_port, 6881);

    m.set_connection("firewalled", 250);
    let h = c.health().await.unwrap();
    assert_eq!(h.incoming, Reachability::Closed as i32, "pare-feu détecté");
    assert_eq!(h.dht_nodes, 250);
}

#[tokio::test]
async fn driver_explains_errors_from_state_and_tracker_without_leaking_urls() {
    use serde_json::json;
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let c = client(&m, root.path());
    let p = root.path().to_string_lossy().into_owned();
    // 1. États d'erreur : un texte explicite (qBittorrent n'en fournit pas lui-même).
    m.insert(MockTorrent::new(H1, "missingFiles", 0.5, &p));
    // 2. Actif mais sans tracker qui fonctionne : on lit le message du tracker (une seule fois, puis cache).
    m.insert(MockTorrent::new(H2, "stalledDL", 0.2, &p));
    m.set_trackers(
        H2,
        json!([
            { "url": "** [DHT] **", "status": 0, "msg": "" },
            { "url": "https://t.example.org/SECRETPASS/announce", "status": 4,
              "msg": "Unregistered torrent https://t.example.org/SECRETPASS/announce?k=SECRET" }
        ]),
    );
    // 3. Tracker qui fonctionne : rien à signaler.
    let h3 = "3333333333333333333333333333333333333333";
    let mut t3 = MockTorrent::new(h3, "stalledDL", 0.2, &p);
    t3.tracker = "https://ok.example.org/ann".into();
    m.insert(t3);

    let by =
        |l: &[proto::pb::TorrentInfo], h: &str| l.iter().find(|t| t.infohash == h).unwrap().clone();
    let l = c.list().await.unwrap();
    assert!(
        by(&l, H1).message.contains("missingFiles"),
        "{}",
        by(&l, H1).message
    );
    let msg = by(&l, H2).message;
    assert_eq!(msg, "Unregistered torrent [t.example.org]");
    assert!(!msg.contains("SECRET"), "aucun passkey dans un message");
    assert_eq!(by(&l, h3).message, "");
    // Le cache évite de redemander le tracker à chaque interrogation (toutes les secondes).
    c.list().await.unwrap();
    c.list().await.unwrap();
    assert_eq!(m.tracker_calls(), 1);
}

#[tokio::test]
async fn add_tracker_only_accepts_the_masters_private_announce_url() {
    use proto::pb::AddTrackerCmd;
    let m = MockQbit::start().await;
    let root = tempfile::tempdir().unwrap();
    let e = Executor::new(Arc::new(client(&m, root.path())), root.path().to_owned())
        .with_tracker_host("master.example.org");
    e.execute(&add_cmd(Source::Magnet(magnet(H1)), "", false))
        .await
        .unwrap();
    let add = |url: &str| {
        cmd(Kind::AddTracker(AddTrackerCmd {
            infohash: H1.into(),
            announce_url: url.into(),
        }))
    };
    let ok = "http://master.example.org:7444/announce/node-1/0123456789abcdef";
    e.execute(&add(ok)).await.unwrap();
    assert_eq!(m.added_trackers(), vec![(H1.to_owned(), ok.to_owned())]);

    // Tout le reste est refusé SANS toucher au client : autre hôte, autre schéma, identifiants, requête, chemin
    // hors /announce/, traversée, caractères de contrôle, démesure.
    let long = format!("http://master.example.org/announce/{}", "a".repeat(400));
    for bad in [
        "http://evil.example.net/announce/n/t",
        "http://master.example.org.evil.net/announce/n/t",
        "http://user@master.example.org/announce/n/t",
        "http://master.example.org@evil.net/announce/n/t",
        "https://master.example.org/announce/n/t",
        "ftp://master.example.org/announce/n/t",
        "http://master.example.org/other/n/t",
        "http://master.example.org/announce/../x",
        "http://master.example.org/announce/n/t?x=1",
        "http://master.example.org/announce/n/t#f",
        "http://master.example.org/announce/n/\u{7f}t",
        "http://master.example.org/announce/n/ t",
        r"http://master.example.org\@evil.net/announce/n/t",
        "",
        "magnet:?xt=urn:btih:abc",
        long.as_str(),
    ] {
        assert!(
            e.execute(&add(bad)).await.is_err(),
            "devrait être refusée : {bad:?}"
        );
    }
    assert_eq!(
        m.added_trackers().len(),
        1,
        "rien d'autre n'a atteint le client"
    );

    // Sans hôte configuré (agent sans master connu) : refus systématique.
    let bare = executor(&m, root.path());
    assert!(bare.execute(&add(ok)).await.is_err());
    // Infohash invalide refusé aussi.
    let bad_hash = cmd(Kind::AddTracker(AddTrackerCmd {
        infohash: "xyz".into(),
        announce_url: ok.into(),
    }));
    assert!(e.execute(&bad_hash).await.is_err());
}
