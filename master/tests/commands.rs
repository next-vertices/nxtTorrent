use agent::pair::{pair, PairOptions};
use agent::qbit::QbitClient;
use agent::tunnel::{run, TunnelConfig, TunnelStatus};
use master::webhook::WebhookNotifier;
use master::{ca::Ca, service, store::Store, Core, Settings, PAIRING_CODE_TTL};
use proto::pb::command::Kind;
use proto::pb::event::Kind as EK;
use proto::pb::{add_cmd::Source, AddCmd, Command, RemoveCmd, TargetCmd, TorrentStatus};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use testkit::qbit::{MockQbit, MockTorrent};
use testkit::webhook::WebhookSink;
use tokio::sync::{oneshot, watch};

const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";

struct Env {
    core: Arc<Core>,
    qbit: MockQbit,
    node_id: String,
    agent_dir: TempDir,
    root: TempDir,
    addr: String,
    _master_dir: TempDir,
    _stop: oneshot::Sender<()>,
}

async fn setup(settings: Settings, notifiers: Vec<Arc<dyn master::notify::Notifier>>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let core = Core::with_settings(ca, Store::open_in_memory().unwrap(), settings, notifiers);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = oneshot::channel::<()>();
    let c = core.clone();
    tokio::spawn(async move {
        service::serve(c, listener, &["127.0.0.1".to_owned()], async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    let agent_dir = tempfile::tempdir().unwrap();
    let code = core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let p = pair(PairOptions {
        addr: addr.clone(),
        code,
        data_dir: agent_dir.path().to_owned(),
        hostname: "nas".into(),
        force: false,
        labels: vec![],
    })
    .await
    .unwrap();
    Env {
        core,
        qbit: MockQbit::start().await,
        node_id: p.node_id,
        agent_dir,
        root: tempfile::tempdir().unwrap(),
        addr,
        _master_dir: dir,
        _stop: tx,
    }
}

fn settings() -> Settings {
    Settings {
        hb_timeout: Duration::from_secs(30),
        confirm_delay: Duration::from_secs(60),
        offline_grace: Duration::from_secs(900),
        cert_validity: Duration::from_secs(14 * 86400),
        disk_reserve: 0,
        sweep_interval: Duration::from_secs(30),
    }
}

struct Agent {
    stop: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl Agent {
    async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.handle.await;
    }
}

async fn start_agent(e: &Env) -> Agent {
    let client = QbitClient::new(
        &e.qbit.url(),
        "admin",
        "adminadmin",
        e.root.path().to_owned(),
    )
    .unwrap();
    start_agent_with(e, Arc::new(client)).await
}

async fn start_agent_with(e: &Env, client: Arc<dyn agent::client::TorrentClient>) -> Agent {
    let mut cfg = TunnelConfig::new(PathBuf::from(e.agent_dir.path()));
    cfg.client = Some(client);
    cfg.root = e.root.path().to_owned();
    cfg.poll_interval = Duration::from_millis(150);
    cfg.hb_interval = Duration::from_millis(200);
    cfg.backoff_base = Duration::from_millis(100);
    let (tx, mut rx) = watch::channel(TunnelStatus::Connecting);
    let (stop, stop_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        run(cfg, tx, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::timeout(
        Duration::from_secs(10),
        rx.wait_for(|s| *s == TunnelStatus::Connected),
    )
    .await
    .expect("tunnel non établi")
    .unwrap();
    Agent { stop, handle }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "délai dépassé: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn cmd(id: &str, kind: Kind) -> Command {
    Command {
        cmd_id: id.into(),
        kind: Some(kind),
    }
}

fn add(id: &str, h: &str, subdir: &str) -> Command {
    cmd(
        id,
        Kind::Add(AddCmd {
            source: Some(Source::Magnet(format!("magnet:?xt=urn:btih:{h}"))),
            save_subdir: subdir.into(),
            start_paused: false,
        }),
    )
}

#[tokio::test]
async fn full_state_reflects_real_client_on_connect() {
    let e = setup(settings(), vec![]).await;
    let root = agent::paths::simplify(e.root.path().canonicalize().unwrap()).join("tv");
    e.qbit.insert(MockTorrent::new(
        H1,
        "stalledUP",
        1.0,
        &root.to_string_lossy(),
    ));
    let a = start_agent(&e).await;

    eventually("FullState reçu", || {
        e.core.telemetry(&e.node_id).full_states >= 1
    })
    .await;
    let t = e.core.telemetry(&e.node_id);
    assert_eq!(t.client_kind, "qbittorrent");
    assert_eq!(t.client_version, "v4.6.0");
    assert_eq!(t.disk_free, 123_456_789);
    assert_eq!(t.torrents.len(), 1);
    assert_eq!(t.torrents[0].infohash, H1);
    assert_eq!(t.torrents[0].status, TorrentStatus::Seeding as i32);
    assert_eq!(t.torrents[0].save_subdir, "tv");
    a.stop().await;
}

#[tokio::test]
async fn commands_events_and_completion() {
    let e = setup(settings(), vec![]).await;
    let a = start_agent(&e).await;
    let id = e.node_id.clone();

    assert!(e.core.send_command(&id, add("c-add", H1, "movies")).await);
    eventually("torrent ajouté", || e.qbit.get(H1).is_some()).await;
    let canon = agent::paths::simplify(e.root.path().canonicalize().unwrap());
    assert!(std::path::Path::new(&e.qbit.get(H1).unwrap().save_path).starts_with(&canon));
    eventually("ack ok", || {
        e.core
            .telemetry(&id)
            .acks
            .iter()
            .any(|x| x.cmd_id == "c-add" && x.ok)
    })
    .await;
    eventually("event Added", || {
        e.core
            .telemetry(&id)
            .events
            .iter()
            .any(|ev| ev.kind == Some(EK::Added(H1.into())))
    })
    .await;

    // Fin du téléchargement -> exactement un événement Completed.
    e.qbit.complete(H1);
    eventually("event Completed", || {
        e.core
            .telemetry(&id)
            .events
            .iter()
            .any(|ev| ev.kind == Some(EK::Completed(H1.into())))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let completed = e
        .core
        .telemetry(&id)
        .events
        .iter()
        .filter(|ev| ev.kind == Some(EK::Completed(H1.into())))
        .count();
    assert_eq!(completed, 1);

    // Pause / Resume / Remove.
    let target = TargetCmd {
        infohash: H1.into(),
    };
    assert!(
        e.core
            .send_command(&id, cmd("c-pause", Kind::Pause(target.clone())))
            .await
    );
    eventually("pausé", || {
        e.qbit.get(H1).is_some_and(|t| t.state == "pausedUP")
    })
    .await;
    assert!(
        e.core
            .send_command(&id, cmd("c-resume", Kind::Resume(target)))
            .await
    );
    eventually("repris", || {
        e.qbit.get(H1).is_some_and(|t| t.state == "stalledUP")
    })
    .await;
    let rm = RemoveCmd {
        infohash: H1.into(),
        delete_files: false,
    };
    assert!(
        e.core
            .send_command(&id, cmd("c-rm", Kind::Remove(rm)))
            .await
    );
    eventually("supprimé", || e.qbit.get(H1).is_none()).await;
    eventually("event Removed", || {
        e.core
            .telemetry(&id)
            .events
            .iter()
            .any(|ev| ev.kind == Some(EK::Removed(H1.into())))
    })
    .await;
    a.stop().await;
}

#[tokio::test]
async fn malicious_path_is_rejected_by_agent_and_audited() {
    let e = setup(settings(), vec![]).await;
    let a = start_agent(&e).await;
    let id = e.node_id.clone();

    for (i, p) in ["../../etc", "/etc/cron.d", "a/../../b"].iter().enumerate() {
        assert!(
            e.core
                .send_command(&id, add(&format!("bad{i}"), H2, p))
                .await
        );
    }
    eventually("3 refus", || {
        e.core.telemetry(&id).acks.iter().filter(|x| !x.ok).count() == 3
    })
    .await;
    assert!(
        e.qbit.torrents().is_empty(),
        "aucun torrent ne doit être créé"
    );
    assert!(e
        .core
        .store
        .audit_actions()
        .unwrap()
        .contains(&"command_refused".to_owned()));
    // Le tunnel reste sain après un refus.
    assert!(e.core.sessions.is_connected(&id));
    a.stop().await;
}

#[tokio::test]
async fn commands_to_offline_node_are_not_delivered() {
    let e = setup(settings(), vec![]).await;
    assert!(!e.core.send_command(&e.node_id, add("x", H1, "")).await);
}

#[tokio::test]
async fn webhook_receives_confirmed_offline() {
    let sink = WebhookSink::start().await;
    let hook = Arc::new(WebhookNotifier::new(&sink.url()).unwrap());
    let mut s = settings();
    s.confirm_delay = Duration::from_millis(800);
    let e = setup(s, vec![hook]).await;
    let a = start_agent(&e).await;
    tokio::time::sleep(Duration::from_millis(1500)).await; // Online confirmé (silencieux)
    a.stop().await;
    eventually("webhook reçu", || !sink.bodies().is_empty()).await;
    let body = sink.bodies().remove(0);
    assert!(
        body.contains("node_offline") && body.contains(&e.node_id),
        "{body}"
    );
    let _ = e.addr;
}

/// Test optionnel contre une vraie instance qBittorrent :
/// `QBIT_URL=http://127.0.0.1:8080 QBIT_USER=admin QBIT_PASS=... cargo test -p master --test commands -- --ignored`
#[tokio::test]
#[ignore = "nécessite un qBittorrent réel"]
async fn real_qbittorrent_smoke() {
    use agent::client::TorrentClient;
    let url = std::env::var("QBIT_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let user = std::env::var("QBIT_USER").unwrap_or_else(|_| "admin".into());
    let pass = std::env::var("QBIT_PASS").unwrap_or_default();
    let root = tempfile::tempdir().unwrap();
    let c = QbitClient::new(&url, &user, &pass, root.path().to_owned()).unwrap();
    assert!(!c.version().await.unwrap().is_empty());
    c.list().await.unwrap();
    c.disk_free().await.unwrap();
}

#[tokio::test]
async fn free_disk_space_is_refreshed_by_heartbeat() {
    let e = setup(settings(), vec![]).await;
    let a = start_agent(&e).await;
    eventually("disque initial", || {
        e.core.telemetry(&e.node_id).disk_free == 123_456_789
    })
    .await;
    // Le disque se remplit : le scheduler doit le savoir sans attendre un changement de torrent.
    e.qbit.set_free(4_242);
    eventually("disque rafraîchi", || {
        e.core.telemetry(&e.node_id).disk_free == 4_242
    })
    .await;
    a.stop().await;
}

/// Master + agent de bout en bout avec chacun des autres clients (Transmission, Deluge, ruTorrent).
#[tokio::test]
async fn end_to_end_with_transmission_deluge_and_rutorrent() {
    use agent::client::TorrentClient;
    use agent::{
        deluge::DelugeClient, rutorrent::RutorrentClient, transmission::TransmissionClient,
    };
    use testkit::{deluge::MockDeluge, rtorrent::MockRutorrent, transmission::MockTransmission};

    let tr = MockTransmission::start().await;
    let de = MockDeluge::start().await;
    let ru = MockRutorrent::start().await;
    for (kind, version) in [
        ("transmission", "4.1.4 (mock)"),
        ("deluge", "2.2.0-mock"),
        ("rutorrent", "rTorrent 0.16.25-mock"),
    ] {
        // rTorrent n'existe que sous Unix : un chemin Windows (antislash) y est refusé par conception.
        if cfg!(windows) && kind == "rutorrent" {
            continue;
        }
        let e = setup(settings(), vec![]).await;
        let root = e.root.path().to_owned();
        let client: Arc<dyn TorrentClient> = match kind {
            "transmission" => {
                Arc::new(TransmissionClient::new(&tr.url(), "admin", "adminadmin", root).unwrap())
            }
            "deluge" => Arc::new(DelugeClient::new(&de.url(), "deluge", root).unwrap()),
            _ => Arc::new(RutorrentClient::new(&ru.url(), "", "", root).unwrap()),
        };
        let a = start_agent_with(&e, client).await;
        let id = e.node_id.clone();
        eventually(&format!("FullState {kind}"), || {
            e.core.telemetry(&id).full_states >= 1
        })
        .await;
        let t = e.core.telemetry(&id);
        assert_eq!(t.client_kind, kind);
        assert_eq!(t.client_version, version);
        assert!(t.disk_free > 0, "{kind}: espace disque remonté");

        // Une commande valide traverse master -> agent -> client ; un chemin malveillant est refusé.
        let h = match kind {
            "transmission" => H1,
            "deluge" => H2,
            _ => "3333333333333333333333333333333333333333",
        };
        assert!(
            e.core
                .send_command(&id, add(&format!("ok-{kind}"), h, "movies"))
                .await
        );
        assert!(
            e.core
                .send_command(&id, add(&format!("bad-{kind}"), h, "../evil"))
                .await
        );
        eventually(&format!("ack ok {kind}"), || {
            e.core
                .telemetry(&id)
                .acks
                .iter()
                .any(|x| x.cmd_id == format!("ok-{kind}") && x.ok)
        })
        .await;
        eventually(&format!("ack refus {kind}"), || {
            e.core
                .telemetry(&id)
                .acks
                .iter()
                .any(|x| x.cmd_id == format!("bad-{kind}") && !x.ok)
        })
        .await;
        // Le torrent apparaît dans l'état observé, avec son sous-dossier relatif.
        eventually("observé par le master", || {
            e.core
                .telemetry(&id)
                .torrents
                .iter()
                .any(|x| x.infohash == h && x.save_subdir == "movies")
        })
        .await;
        a.stop().await;
    }
}
