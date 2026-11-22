//! Garde-fou : un agent qui ne parvient plus à joindre le master (ex. pile réseau périmée dans Docker)
//! quitte avec une erreur pour que son superviseur le relance ; un agent qui le joint reste en vie.
use agent::pair::{pair, PairOptions};
use agent::tunnel::{run, TunnelConfig, TunnelError, TunnelStatus};
use master::{ca::Ca, service, store::Store, Core, PAIRING_CODE_TTL};
use std::time::Duration;
use tokio::sync::watch;

async fn paired_agent_dir() -> (
    tempfile::TempDir,
    std::sync::Arc<Core>,
    tokio::sync::oneshot::Sender<()>,
) {
    let mdir = tempfile::tempdir().unwrap();
    let core = Core::new(
        Ca::load_or_create(mdir.path()).unwrap(),
        Store::open_in_memory().unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let c = core.clone();
    tokio::spawn(async move {
        let _keep = mdir; // le dossier de la CA vit autant que le serveur
        service::serve(c, listener, &["127.0.0.1".to_owned()], async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });
    let data = tempfile::tempdir().unwrap();
    let code = core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    pair(PairOptions {
        addr,
        code,
        data_dir: data.path().to_owned(),
        hostname: "wd".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap();
    (data, core, stop_tx)
}

fn cfg(data: &std::path::Path, limit: Duration) -> TunnelConfig {
    let mut c = TunnelConfig::new(data.to_owned());
    c.backoff_base = Duration::from_millis(100);
    c.exit_after_disconnected = Some(limit);
    c
}

#[tokio::test]
async fn agent_exits_when_the_master_stays_unreachable() {
    let (data, _core, _stop) = paired_agent_dir().await;
    // Le master « disparaît » : l'adresse mémorisée pointe maintenant vers un port fermé.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    std::fs::write(data.path().join("master_addr"), closed.to_string()).unwrap();

    let (tx, _rx) = watch::channel(TunnelStatus::Connecting);
    let started = std::time::Instant::now();
    let res = tokio::time::timeout(
        Duration::from_secs(40),
        run(
            cfg(data.path(), Duration::from_secs(3)),
            tx,
            std::future::pending::<()>(),
        ),
    )
    .await
    .expect("l'agent aurait dû quitter");
    assert!(matches!(res, Err(TunnelError::Unreachable(3))), "{res:?}");
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[tokio::test]
async fn agent_stays_up_while_it_can_reach_the_master() {
    let (data, core, _stop) = paired_agent_dir().await;
    let (tx, mut rx) = watch::channel(TunnelStatus::Connecting);
    let (stop_agent, stopped) = tokio::sync::oneshot::channel::<()>();
    let dir = data.path().to_owned();
    let handle = tokio::spawn(async move {
        run(cfg(&dir, Duration::from_secs(2)), tx, async {
            let _ = stopped.await;
        })
        .await
    });
    tokio::time::timeout(
        Duration::from_secs(10),
        rx.wait_for(|s| *s == TunnelStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    // Bien plus longtemps que la limite : connecté, il ne doit pas partir.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!handle.is_finished(), "l'agent connecté a quitté à tort");
    assert!(core
        .store
        .list_nodes()
        .unwrap()
        .iter()
        .any(|n| n.state == "Online"));
    let _ = stop_agent.send(());
    assert!(handle.await.unwrap().is_ok());
}
