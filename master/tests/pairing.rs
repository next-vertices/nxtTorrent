use agent::pair::{pair, PairError, PairOptions};
use master::{ca::Ca, service, store::Store, Core, PAIRING_CODE_TTL};
use proto::pairing::PairingCode;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;

struct TestMaster {
    core: Arc<Core>,
    addr: String,
    _dir: TempDir,
    _stop: oneshot::Sender<()>,
}

async fn start_master() -> TestMaster {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let core = Core::new(ca, Store::open_in_memory().unwrap());
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

fn opts(addr: &str, code: &str, dir: &Path) -> PairOptions {
    PairOptions {
        addr: addr.to_owned(),
        code: code.to_owned(),
        data_dir: dir.to_owned(),
        hostname: "nas-1".to_owned(),
        force: false,
        labels: vec!["ssd".to_owned(), "vpn-ch".to_owned()],
    }
}

#[tokio::test]
async fn full_pairing_stores_identity() {
    let m = start_master().await;
    let code = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let paired = pair(opts(&m.addr, &code, dir.path())).await.unwrap();

    for f in ["agent.key", "agent.crt", "ca.crt", "node_id"] {
        assert!(dir.path().join(f).exists(), "{f} manquant");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ca.crt")).unwrap(),
        m.core.ca.cert_pem
    );
    assert_eq!(
        m.core.store.node_state(&paired.node_id).unwrap().as_deref(),
        Some("Pending")
    );
    assert!(m
        .core
        .store
        .audit_actions()
        .unwrap()
        .contains(&"node_paired".to_owned()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("agent.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Un nœud déjà appairé ne se ré-appaire pas.
    let again = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    assert!(matches!(
        pair(opts(&m.addr, &again, dir.path())).await,
        Err(PairError::AlreadyPaired(_))
    ));
}

#[tokio::test]
async fn code_is_single_use() {
    let m = start_master().await;
    let code = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    pair(opts(&m.addr, &code, d1.path())).await.unwrap();
    let err = pair(opts(&m.addr, &code, d2.path())).await.unwrap_err();
    assert!(matches!(err, PairError::Rejected(_)), "{err}");
    assert!(!d2.path().join("agent.key").exists());
}

#[tokio::test]
async fn expired_code_is_rejected() {
    let m = start_master().await;
    let code = m.core.create_pairing_code(Duration::ZERO).unwrap();
    let d = tempfile::tempdir().unwrap();
    let err = pair(opts(&m.addr, &code, d.path())).await.unwrap_err();
    assert!(matches!(err, PairError::Rejected(_)), "{err}");
}

#[tokio::test]
async fn wrong_ca_fingerprint_blocks_mitm() {
    let real = start_master().await;
    let code = real.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let mut parsed = PairingCode::parse(&code).unwrap();
    parsed.ca_fp[0] ^= 0xff; // le code « promet » une autre CA que celle du serveur
    let d = tempfile::tempdir().unwrap();
    let err = pair(opts(&real.addr, &parsed.encode(), d.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, PairError::Transport(_)), "{err}");
    assert!(!d.path().join("agent.key").exists());

    // Un autre master (autre CA) ne peut pas non plus se faire passer pour le vrai.
    let imposter = start_master().await;
    let err = pair(opts(&imposter.addr, &code, d.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, PairError::Transport(_)), "{err}");
    // Le code n'a jamais atteint l'imposteur ni été consommé chez le vrai master.
    let ok = pair(opts(&real.addr, &code, d.path())).await;
    assert!(ok.is_ok(), "{ok:?}", ok = ok.as_ref().err());
}

#[tokio::test]
async fn malformed_code_rejected_locally() {
    let d = tempfile::tempdir().unwrap();
    let err = pair(opts("127.0.0.1:1", "nimporte-quoi", d.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, PairError::BadCode));
}

#[tokio::test]
async fn brute_force_is_rate_limited() {
    let m = start_master().await;
    let good = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let mut guess = PairingCode::parse(&good).unwrap();
    guess.secret[0] ^= 0xff; // bon fingerprint, mauvais secret
    let bad = guess.encode();
    for _ in 0..5 {
        let d = tempfile::tempdir().unwrap();
        let e = pair(opts(&m.addr, &bad, d.path())).await.unwrap_err();
        assert!(matches!(e, PairError::Rejected(_)));
    }
    // Même le bon code est refusé depuis cette IP une fois le seuil atteint.
    let d = tempfile::tempdir().unwrap();
    let e = pair(opts(&m.addr, &good, d.path())).await.unwrap_err();
    assert!(
        matches!(&e, PairError::Rejected(msg) if msg.contains("trop")),
        "{e}"
    );
    let actions = m.core.store.audit_actions().unwrap();
    assert!(actions.contains(&"pair_rejected".to_owned()));
    assert!(actions.contains(&"pair_rate_limited".to_owned()));
}

#[tokio::test]
async fn labels_come_from_the_admin_code_not_from_the_agent() {
    let m = start_master().await;
    let code = m
        .core
        .create_pairing_code_with_labels(PAIRING_CODE_TTL, &["ssd".to_owned()])
        .unwrap();
    let d = tempfile::tempdir().unwrap();
    let mut o = opts(&m.addr, &code, d.path());
    o.labels = vec!["vpn-ch".to_owned(), "tor".to_owned()]; // l'agent tente de se promouvoir
    let paired = pair(o).await.unwrap();
    let node = m
        .core
        .store
        .list_nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.id == paired.node_id)
        .unwrap();
    assert_eq!(node.labels, vec!["ssd".to_owned()]);
    // Label invalide refusé dès la création du code.
    assert!(m
        .core
        .create_pairing_code_with_labels(PAIRING_CODE_TTL, &["a b".to_owned()])
        .is_err());
}

#[tokio::test]
async fn forced_repairing_replaces_identity_atomically() {
    let m = start_master().await;
    let d = tempfile::tempdir().unwrap();
    let c1 = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let first = pair(opts(&m.addr, &c1, d.path())).await.unwrap();
    std::fs::write(d.path().join("agent.crt"), "").unwrap(); // certificat corrompu
    std::fs::write(d.path().join("outbox.log"), "ancien").unwrap();

    let c2 = m.core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    assert!(matches!(
        pair(opts(&m.addr, &c2, d.path())).await,
        Err(PairError::AlreadyPaired(_))
    ));
    let mut o = opts(&m.addr, &c2, d.path());
    o.force = true;
    let second = pair(o).await.unwrap();
    assert_ne!(first.node_id, second.node_id, "nouvelle identité");
    assert!(!std::fs::read_to_string(d.path().join("agent.crt"))
        .unwrap()
        .is_empty());
    assert!(
        !d.path().join("outbox.log").exists(),
        "état de l'ancien nœud purgé"
    );
    for f in ["agent.key", "agent.crt", "ca.crt", "node_id", "master_addr"] {
        assert!(d.path().join(f).exists(), "{f}");
    }
    assert!(!d.path().join("agent.key.tmp").exists());
}
