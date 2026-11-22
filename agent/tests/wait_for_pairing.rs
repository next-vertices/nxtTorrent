//! Un agent lancé AVANT d'être appairé (cas Docker : `compose up` démarre tout d'un coup) ne doit pas
//! quitter : il attend que `agent pair` écrive l'identité dans son dossier de données, puis se connecte seul.
use agent::pair::{pair, PairOptions};
use master::{ca::Ca, service, store::Store, Core, PAIRING_CODE_TTL};
use std::process::{Command, Stdio};
use std::time::Duration;

const AGENT: &str = env!("CARGO_BIN_EXE_agent");

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpaired_agent_waits_then_connects_by_itself_once_paired() {
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
        service::serve(c, listener, &["127.0.0.1".to_owned()], async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });

    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(AGENT)
        .arg("--data-dir")
        .arg(data.path())
        .args(["run", "--root"])
        .arg(root.path())
        .args(["--client-url", "http://127.0.0.1:1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Non appairé : il reste en vie (il ne s'arrête pas en erreur) et ne se connecte pas.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "l'agent non appairé a quitté au lieu d'attendre"
    );
    assert!(core.store.list_nodes().unwrap().is_empty());

    // L'appairage (lancé « à côté », comme `docker compose run --rm agent pair …`) écrit l'identité.
    let code = core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let node_id = pair(PairOptions {
        addr,
        code,
        data_dir: data.path().to_owned(),
        hostname: "nas-test".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;

    // L'agent déjà lancé détecte l'identité et ouvre son tunnel, sans redémarrage.
    let mut connected = false;
    for _ in 0..60 {
        if core.sessions.is_connected(&node_id) {
            connected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = stop_tx.send(());
    assert!(
        connected,
        "l'agent n'a pas rejoint le master après l'appairage"
    );
}
