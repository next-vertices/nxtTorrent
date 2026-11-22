use agent::pair::{pair, PairOptions};
use agent::tls::{connect_channel, pinned_client_config};
use agent::tunnel::{run, TunnelConfig, TunnelStatus};
use master::notify::{Notification, Notifier, PresenceKind};
use master::{ca::Ca, service, store::Store, Core, Settings, PAIRING_CODE_TTL};
use proto::pb::orchestrator_client::OrchestratorClient;
use proto::pb::AgentMsg;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(10);

struct TestMaster {
    core: Arc<Core>,
    addr: String,
    _dir: TempDir,
    _stop: oneshot::Sender<()>,
}

async fn start_master(settings: Settings, notifiers: Vec<Arc<dyn Notifier>>) -> TestMaster {
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
    TestMaster {
        core,
        addr,
        _dir: dir,
        _stop: tx,
    }
}

fn fast_settings() -> Settings {
    Settings {
        hb_timeout: Duration::from_secs(30),
        confirm_delay: Duration::from_secs(60),
        offline_grace: Duration::from_secs(900),
        cert_validity: Duration::from_secs(14 * 86400),
        disk_reserve: 0,
        sweep_interval: Duration::from_secs(30),
    }
}

struct Paired {
    dir: TempDir,
    node_id: String,
}

async fn pair_node(m: &TestMaster) -> Paired {
    let dir = tempfile::tempdir().unwrap();
    let code = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let p = pair(PairOptions {
        addr: m.addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "n".into(),
        force: false,
        labels: vec![],
    })
    .await
    .unwrap();
    Paired {
        dir,
        node_id: p.node_id,
    }
}

struct Running {
    status: watch::Receiver<TunnelStatus>,
    stop: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl Running {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let _ = self.handle.await;
    }
    async fn wait(&mut self, pred: impl Fn(&TunnelStatus) -> bool) {
        tokio::time::timeout(WAIT, self.status.wait_for(|s| pred(s)))
            .await
            .expect("statut attendu non atteint")
            .unwrap();
    }
}

fn spawn_agent(dir: &Path, tweak: impl FnOnce(&mut TunnelConfig)) -> Running {
    let mut cfg = TunnelConfig::new(dir.to_owned());
    cfg.backoff_base = Duration::from_millis(100);
    cfg.backoff_max = Duration::from_secs(60);
    tweak(&mut cfg);
    let (tx, rx) = watch::channel(TunnelStatus::Connecting);
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        run(cfg, tx, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });
    Running {
        status: rx,
        stop: Some(stop_tx),
        handle,
    }
}

async fn wait_state(core: &Arc<Core>, id: &str, want: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let st = core.store.node_state(id).unwrap();
        if st.as_deref() == Some(want) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "état {want} non atteint (actuel: {st:?})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn tunnel_online_drop_offline_then_resume() {
    let m = start_master(fast_settings(), vec![]).await;
    let n = pair_node(&m).await;
    let mut a = spawn_agent(n.dir.path(), |_| {});
    a.wait(|s| *s == TunnelStatus::Connected).await;
    wait_state(&m.core, &n.node_id, "Online").await;
    assert!(m.core.sessions.is_connected(&n.node_id));
    let info = m.core.store.node_info(&n.node_id).unwrap().unwrap();
    assert!(info.last_seen.is_some());

    // Coupure : le tunnel se ferme -> Offline, last_seen conservé.
    a.stop().await;
    wait_state(&m.core, &n.node_id, "Offline").await;
    let off = m.core.store.node_info(&n.node_id).unwrap().unwrap();
    assert!(off.last_seen.is_some() && off.offline_since.is_some());

    // Reprise : reconnexion + FullState -> Online.
    let mut b = spawn_agent(n.dir.path(), |_| {});
    b.wait(|s| *s == TunnelStatus::Connected).await;
    wait_state(&m.core, &n.node_id, "Online").await;
    b.stop().await;
}

#[tokio::test]
async fn missed_heartbeats_mark_offline_and_agent_reconnects() {
    let mut s = fast_settings();
    s.hb_timeout = Duration::from_secs(1);
    let m = start_master(s, vec![]).await;
    let n = pair_node(&m).await;
    // L'agent n'émet (quasi) pas de heartbeat : le master doit déclarer le nœud Offline.
    let mut a = spawn_agent(n.dir.path(), |c| c.hb_interval = Duration::from_secs(3600));
    a.wait(|s| *s == TunnelStatus::Connected).await;
    a.wait(|s| matches!(s, TunnelStatus::Disconnected(r) if r.contains("heartbeat_timeout")))
        .await;
    wait_state(&m.core, &n.node_id, "Offline").await;
    // Backoff puis reconnexion automatique.
    a.wait(|s| *s == TunnelStatus::Connected).await;
    a.stop().await;
}

#[tokio::test]
async fn duplicate_session_closes_old_tunnel_and_alerts() {
    let m = start_master(fast_settings(), vec![]).await;
    let n = pair_node(&m).await;
    let clone_dir = tempfile::tempdir().unwrap();
    for f in ["agent.key", "agent.crt", "ca.crt", "node_id", "master_addr"] {
        std::fs::copy(n.dir.path().join(f), clone_dir.path().join(f)).unwrap();
    }
    let mut a = spawn_agent(n.dir.path(), |_| {});
    a.wait(|s| *s == TunnelStatus::Connected).await;
    let mut b = spawn_agent(clone_dir.path(), |_| {});
    b.wait(|s| *s == TunnelStatus::Connected).await;
    a.wait(|s| matches!(s, TunnelStatus::Disconnected(r) if r == "duplicate_session"))
        .await;
    assert!(m
        .core
        .store
        .audit_actions()
        .unwrap()
        .contains(&"duplicate_session".to_owned()));
    // Le nouveau tunnel reste le tunnel actif et le nœud est Online.
    wait_state(&m.core, &n.node_id, "Online").await;
    assert!(m.core.sessions.is_connected(&n.node_id));
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn connect_without_client_cert_is_refused() {
    let m = start_master(fast_settings(), vec![]).await;
    let cfg = pinned_client_config(m.core.ca.fingerprint()).unwrap();
    let chan = connect_channel(&m.addr, cfg).await.unwrap();
    let res = OrchestratorClient::new(chan)
        .connect(tokio_stream::iter(Vec::<AgentMsg>::new()))
        .await;
    assert!(res.is_err(), "Connect sans certificat client doit échouer");
}

#[tokio::test]
async fn expired_certificate_is_refused() {
    let mut s = fast_settings();
    s.cert_validity = Duration::from_secs(2);
    let m = start_master(s, vec![]).await;
    let n = pair_node(&m).await;
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let a = spawn_agent(n.dir.path(), |_| {});
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!m.core.sessions.is_connected(&n.node_id));
    assert!(!matches!(*a.status.borrow(), TunnelStatus::Connected));
    a.stop().await;
    // L'échec mTLS (certificat expiré) est journalisé, avec l'IP et la raison.
    let rows = m
        .core
        .store
        .audit_list(50, None, Some("mtls_failed"))
        .unwrap();
    assert!(!rows.is_empty(), "échec mTLS non audité");
    assert!(
        rows[0].detail.starts_with("127.0.0.1:"),
        "{}",
        rows[0].detail
    );
    assert!(
        rows[0].detail.to_lowercase().contains("expired"),
        "raison attendue : {}",
        rows[0].detail
    );
}

#[tokio::test]
async fn non_tls_scans_are_audited_once_per_ip_without_flooding_the_log() {
    use tokio::io::AsyncWriteExt;
    let m = start_master(fast_settings(), vec![]).await;
    // Un scanner envoie du HTTP en clair puis des octets quelconques, 30 fois.
    for i in 0..30 {
        let mut c = tokio::net::TcpStream::connect(&m.addr).await.unwrap();
        let _ = c
            .write_all(if i % 2 == 0 {
                b"GET / HTTP/1.1

"
            } else {
                b" garbage"
            })
            .await;
        drop(c);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rows = m
        .core
        .store
        .audit_list(100, None, Some("mtls_failed"))
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "une seule entrée par IP et par fenêtre : {rows:?}"
    );
    assert!(rows[0].detail.starts_with("127.0.0.1:"));
    // Le master reste pleinement fonctionnel après le scan.
    let n = pair_node(&m).await;
    let mut a = spawn_agent(n.dir.path(), |_| {});
    a.wait(|s| *s == TunnelStatus::Connected).await;
    a.stop().await;
}

#[tokio::test]
async fn silent_connections_do_not_hold_handshake_slots_forever() {
    // Des clients qui ouvrent le TCP et ne disent rien ne doivent pas empêcher les vrais agents de se connecter.
    let m = start_master(fast_settings(), vec![]).await;
    let mut idle = Vec::new();
    for _ in 0..50 {
        idle.push(tokio::net::TcpStream::connect(&m.addr).await.unwrap());
    }
    let n = pair_node(&m).await;
    let mut a = spawn_agent(n.dir.path(), |_| {});
    a.wait(|s| *s == TunnelStatus::Connected).await;
    a.stop().await;
    drop(idle);
}

#[tokio::test]
async fn revoked_node_is_disconnected_and_refused() {
    let m = start_master(fast_settings(), vec![]).await;
    let n = pair_node(&m).await;
    let mut a = spawn_agent(n.dir.path(), |_| {});
    a.wait(|s| *s == TunnelStatus::Connected).await;

    m.core.revoke_node(&n.node_id);
    a.wait(|s| *s == TunnelStatus::Revoked).await;
    a.handle.await.unwrap(); // l'agent s'arrête de lui-même
    wait_state(&m.core, &n.node_id, "Revoked").await;
    assert!(!m.core.sessions.is_connected(&n.node_id));

    // Le certificat est encore valide, mais le master refuse la session.
    let mut b = spawn_agent(n.dir.path(), |_| {});
    b.wait(|s| *s == TunnelStatus::Revoked).await;
    assert_eq!(
        m.core.store.node_state(&n.node_id).unwrap().as_deref(),
        Some("Revoked")
    );
    assert!(m
        .core
        .store
        .audit_actions()
        .unwrap()
        .contains(&"connect_refused".to_owned()));
}

#[tokio::test]
async fn certificate_is_renewed_over_existing_tunnel() {
    let mut s = fast_settings();
    s.cert_validity = Duration::from_secs(9);
    let m = start_master(s, vec![]).await;
    let n = pair_node(&m).await;
    let crt: PathBuf = n.dir.path().join("agent.crt");
    let before = std::fs::read_to_string(&crt).unwrap();
    let mut a = spawn_agent(n.dir.path(), |c| c.renew_check = Duration::from_millis(300));
    a.wait(|s| *s == TunnelStatus::Connected).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while std::fs::read_to_string(&crt).unwrap() == before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "certificat non renouvelé"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(m
        .core
        .store
        .audit_actions()
        .unwrap()
        .contains(&"cert_renewed".to_owned()));

    // Au-delà de l'échéance de l'ancien certificat, le tunnel doit fonctionner avec le nouveau.
    tokio::time::sleep(Duration::from_secs(6)).await;
    a.wait(|s| *s == TunnelStatus::Connected).await;
    assert!(m.core.sessions.is_connected(&n.node_id));
    a.stop().await;
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Notification>>);
impl Notifier for Recorder {
    fn notify(&self, n: &Notification) {
        self.0.lock().unwrap().push(n.clone());
    }
}

#[tokio::test]
async fn presence_notifications_are_debounced() {
    let rec = Arc::new(Recorder::default());
    let mut s = fast_settings();
    s.confirm_delay = Duration::from_millis(1500);
    let m = start_master(s, vec![rec.clone()]).await;
    let n = pair_node(&m).await;

    let a = spawn_agent(n.dir.path(), |_| {});
    tokio::time::sleep(Duration::from_millis(2500)).await; // Online confirmé (silencieux)
                                                           // Flapping : coupure brève, reconnexion avant le délai de confirmation.
    a.stop().await;
    wait_state(&m.core, &n.node_id, "Offline").await;
    let mut b = spawn_agent(n.dir.path(), |_| {});
    b.wait(|s| *s == TunnelStatus::Connected).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        rec.0.lock().unwrap().is_empty(),
        "le flapping ne doit rien notifier"
    );

    // Coupure durable : notification Offline, puis Online au retour.
    b.stop().await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        rec.0.lock().unwrap().last().map(|n| n.kind),
        Some(PresenceKind::WentOffline)
    );
    let mut c = spawn_agent(n.dir.path(), |_| {});
    c.wait(|s| *s == TunnelStatus::Connected).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let got: Vec<_> = rec.0.lock().unwrap().iter().map(|n| n.kind).collect();
    assert_eq!(
        got,
        vec![PresenceKind::WentOffline, PresenceKind::CameOnline]
    );
    c.stop().await;
}

#[tokio::test]
async fn presence_is_reset_when_master_starts() {
    use master::store::NodeRecord;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    for (id, st) in [
        ("online", "Online"),
        ("pending", "Pending"),
        ("revoked", "Revoked"),
    ] {
        store
            .insert_node(&NodeRecord {
                id: id.into(),
                hostname: "h".into(),
                agent_version: "v".into(),
                labels: vec![],
                name: None,
            })
            .unwrap();
        store.set_state(id, st, 0).unwrap();
    }
    let core = Core::with_settings(
        Ca::load_or_create(dir.path()).unwrap(),
        store,
        fast_settings(),
        vec![],
    );
    // Aucun tunnel n'existe au démarrage : « Online » en base était périmé.
    let online = core.store.node_info("online").unwrap().unwrap();
    assert_eq!(online.state, "Offline");
    assert!(
        online.offline_since.is_some(),
        "le délai de grâce peut courir"
    );
    assert_eq!(
        core.store.node_state("pending").unwrap().as_deref(),
        Some("Pending")
    );
    assert_eq!(
        core.store.node_state("revoked").unwrap().as_deref(),
        Some("Revoked")
    );
}

#[tokio::test]
async fn database_error_is_retryable_not_a_revocation() {
    let m = start_master(fast_settings(), vec![]).await;
    let n = pair_node(&m).await;
    m.core.store.break_nodes_table_for_tests();
    let mut a = spawn_agent(n.dir.path(), |_| {});
    a.wait(|s| matches!(s, TunnelStatus::Disconnected(r) if r.contains("internal_error")))
        .await;
    assert!(
        !matches!(*a.status.borrow(), TunnelStatus::Revoked),
        "une panne de base ne doit pas révoquer le nœud côté agent"
    );
    // La base revient : l'agent, toujours en vie, se reconnecte seul.
    m.core.store.restore_nodes_table_for_tests();
    a.wait(|s| *s == TunnelStatus::Connected).await;
    a.stop().await;
}

#[tokio::test]
async fn silent_tcp_probes_are_not_audited_as_security_failures() {
    // Une sonde de supervision / un test de port ouvre le TCP puis ferme sans envoyer un octet : ni attaque ni erreur.
    let m = start_master(fast_settings(), vec![]).await;
    for _ in 0..10 {
        drop(tokio::net::TcpStream::connect(&m.addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        m.core
            .store
            .audit_list(50, None, Some("mtls_failed"))
            .unwrap()
            .is_empty(),
        "une connexion muette ne doit pas être auditée"
    );
}
