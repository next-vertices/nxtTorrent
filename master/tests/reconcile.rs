//! Phase 4 : réconciliation, scheduler/labels, réplication, outbox — master et agents réels.
use agent::outbox::Outbox;
use agent::pair::{pair, PairOptions};
use agent::qbit::QbitClient;
use agent::tunnel::{run, TunnelConfig, TunnelStatus};
use master::manage::{NewTorrent, Source};
use master::{ca::Ca, service, store::Store, Core, Settings, PAIRING_CODE_TTL};
use proto::pb::event::Kind as EK;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use testkit::qbit::{MockQbit, MockTorrent};
use tokio::sync::{oneshot, watch};

const T1: &str = "1111111111111111111111111111111111111111";
const T2: &str = "2222222222222222222222222222222222222222";
const T3: &str = "3333333333333333333333333333333333333333";

struct Master {
    core: Arc<Core>,
    addr: String,
    _dir: TempDir,
    _stop: oneshot::Sender<()>,
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

async fn start_master(settings: Settings) -> Master {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let core = Core::with_settings(ca, Store::open_in_memory().unwrap(), settings, vec![]);
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
    Master {
        core,
        addr,
        _dir: dir,
        _stop: tx,
    }
}

struct Node {
    id: String,
    qbit: MockQbit,
    root: TempDir,
    dir: TempDir,
    running: Option<(oneshot::Sender<()>, tokio::task::JoinHandle<()>)>,
    backoff: Duration,
}

async fn add_node(m: &Master, labels: &[&str]) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let labels: Vec<String> = labels.iter().map(|s| (*s).to_owned()).collect();
    let code = m
        .core
        .create_pairing_code_with_labels(PAIRING_CODE_TTL, &labels)
        .unwrap();
    let p = pair(PairOptions {
        addr: m.addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "n".into(),
        force: false,
        labels: labels.iter().map(|s| (*s).to_owned()).collect(),
    })
    .await
    .unwrap();
    Node {
        id: p.node_id,
        qbit: MockQbit::start().await,
        root: tempfile::tempdir().unwrap(),
        dir,
        running: None,
        backoff: Duration::from_millis(100),
    }
}

impl Node {
    async fn start(&mut self) {
        let client = QbitClient::new(
            &self.qbit.url(),
            "admin",
            "adminadmin",
            self.root.path().to_owned(),
        )
        .unwrap();
        let mut cfg = TunnelConfig::new(PathBuf::from(self.dir.path()));
        cfg.client = Some(Arc::new(client));
        cfg.root = self.root.path().to_owned();
        cfg.poll_interval = Duration::from_millis(100);
        cfg.backoff_base = self.backoff;
        let (tx, mut rx) = watch::channel(TunnelStatus::Connecting);
        let (stop, stop_rx) = oneshot::channel::<()>();
        let h = tokio::spawn(async move {
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
        self.running = Some((stop, h));
    }

    async fn stop(&mut self) {
        if let Some((stop, h)) = self.running.take() {
            let _ = stop.send(());
            let _ = h.await;
        }
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "délai dépassé: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn new_t(ih: &str, requires: &[&str], group: Option<&str>) -> NewTorrent {
    NewTorrent {
        source: Source::Infohash(ih.into()),
        save_subdir: "movies".into(),
        requires: requires.iter().map(|s| (*s).to_owned()).collect(),
        replicate_group: group.map(str::to_owned),
        size_hint: 0,
        name: String::new(),
    }
}

#[tokio::test]
async fn scheduler_respects_labels_load_and_keeps_pending() {
    let m = start_master(settings()).await;
    let mut a = add_node(&m, &["ssd"]).await;
    let mut b = add_node(&m, &["ssd", "vpn-ch"]).await;
    a.start().await;
    b.start().await;

    // Seul B est conforme à `vpn-ch`.
    m.core
        .add_torrent(new_t(T1, &["vpn-ch"], None))
        .await
        .unwrap();
    eventually("T1 sur B", || b.qbit.get(T1).is_some()).await;
    assert!(a.qbit.get(T1).is_none());
    assert_eq!(m.core.torrent_primary(T1).as_deref(), Some(b.id.as_str()));

    // Aucun nœud `tor` : reste Pending, jamais assigné à un nœud non conforme.
    m.core.add_torrent(new_t(T2, &["tor"], None)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(m.core.torrent_primary(T2), None);
    assert!(a.qbit.get(T2).is_none() && b.qbit.get(T2).is_none());

    // Un nœud conforme arrive : le torrent en attente lui est assigné.
    let mut c = add_node(&m, &["tor"]).await;
    c.start().await;
    eventually("T2 sur C", || c.qbit.get(T2).is_some()).await;
    assert_eq!(m.core.torrent_primary(T2).as_deref(), Some(c.id.as_str()));

    // Le moins chargé : A (0 torrent) plutôt que B (1).
    m.core.add_torrent(new_t(T3, &["ssd"], None)).await.unwrap();
    eventually("T3 sur A", || a.qbit.get(T3).is_some()).await;
    assert!(b.qbit.get(T3).is_none());

    // Entrées invalides refusées côté master.
    let mut bad = new_t(T3, &[], None);
    bad.save_subdir = "../x".into();
    assert!(m.core.add_torrent(bad).await.is_err());
    assert!(m
        .core
        .add_torrent(new_t("pas-un-hash", &[], None))
        .await
        .is_err());
    for n in [&mut a, &mut b, &mut c] {
        n.stop().await;
    }
}

#[tokio::test]
async fn reconnect_repairs_drift_and_master_stays_source_of_truth() {
    let m = start_master(settings()).await;
    let mut a = add_node(&m, &[]).await;
    a.start().await;
    // Un torrent que le master ne gère pas : ne doit jamais être touché.
    a.qbit.insert(MockTorrent::new(
        &"f".repeat(40),
        "downloading",
        0.5,
        "/ailleurs",
    ));

    m.core.add_torrent(new_t(T1, &[], None)).await.unwrap();
    eventually("ajouté", || a.qbit.get(T1).is_some()).await;
    let rev = || m.core.store.node_rev(&a.id).unwrap();
    eventually("applied_rev == desired_rev", || {
        let t = m.core.telemetry(&a.id);
        rev() > 0 && t.applied_rev == rev()
    })
    .await;

    // Dérive pendant une coupure : le torrent disparaît localement.
    a.stop().await;
    a.qbit.remove(T1);
    a.start().await;
    eventually("ré-ajouté par le diff", || a.qbit.get(T1).is_some()).await;
    assert!(
        a.qbit.get(&"f".repeat(40)).is_some(),
        "torrent non géré intact"
    );

    // La pause décidée par le master s'impose au nœud, puis la reprise.
    m.core.set_torrent_paused(T1, true).await.unwrap();
    eventually("pausé", || {
        a.qbit.get(T1).is_some_and(|t| t.state == "pausedDL")
    })
    .await;
    m.core.set_torrent_paused(T1, false).await.unwrap();
    eventually("repris", || {
        a.qbit.get(T1).is_some_and(|t| t.state == "downloading")
    })
    .await;
    // Dérive manuelle (reprise/pause locale) corrigée aussi.
    m.core.set_torrent_paused(T1, true).await.unwrap();
    eventually("repausé", || {
        a.qbit.get(T1).is_some_and(|t| t.state == "pausedDL")
    })
    .await;

    // Suppression côté master -> tombstone appliqué puis assignation purgée.
    m.core.remove_torrent(T1, false).await.unwrap();
    eventually("retiré", || a.qbit.get(T1).is_none()).await;
    eventually("assignation purgée", || {
        m.core.store.assignments_for_torrent(T1).unwrap().is_empty()
    })
    .await;
    assert!(a.qbit.get(&"f".repeat(40)).is_some());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        a.qbit.get(T1).is_none(),
        "pas de ré-ajout après suppression"
    );
    a.stop().await;
}

#[tokio::test]
async fn completion_replicates_to_target_group_with_recheck_then_seed() {
    let m = start_master(settings()).await;
    let mut src = add_node(&m, &["src"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    let mut c = add_node(&m, &["mirror"]).await;
    let mut d = add_node(&m, &["autre"]).await;
    for n in [&mut src, &mut b, &mut c, &mut d] {
        n.start().await;
    }
    m.core
        .add_torrent(new_t(T1, &["src"], Some("mirror")))
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(T1).is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        b.qbit.get(T1).is_none() && c.qbit.get(T1).is_none(),
        "pas de réplica avant la complétion"
    );

    // Fin du téléchargement sur la source -> réplicas en pause, Recheck, puis seed.
    src.qbit.complete(T1);
    for n in [&b, &c] {
        eventually("réplica ajouté en pause puis recheck", || {
            n.qbit.get(T1).is_some_and(|t| t.state == "checkingDL")
        })
        .await;
        let t = n.qbit.get(T1).unwrap();
        assert!(
            t.save_path.ends_with("movies"),
            "même save_subdir: {}",
            t.save_path
        );
        n.qbit.finish_check(T1); // la vérification trouve les données complètes
    }
    for n in [&b, &c] {
        eventually("seed", || {
            n.qbit.get(T1).is_some_and(|t| t.state == "stalledUP")
        })
        .await;
    }
    assert!(d.qbit.get(T1).is_none(), "hors groupe cible");
    let rows = m.core.store.assignments_for_torrent(T1).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r.role == "replica" && r.phase == "Active")
            .count(),
        2
    );
    assert!(m.core.store.get_torrent(T1).unwrap().unwrap().completed);
    for n in [&mut src, &mut b, &mut c, &mut d] {
        n.stop().await;
    }
}

#[tokio::test]
async fn completion_during_tunnel_outage_is_replayed_and_deduplicated() {
    let m = start_master(settings()).await;
    let mut a = add_node(&m, &[]).await;
    a.backoff = Duration::from_millis(1500); // coupure suffisamment longue
    a.start().await;
    m.core.add_torrent(new_t(T1, &[], None)).await.unwrap();
    eventually("ajouté", || a.qbit.get(T1).is_some()).await;
    eventually("Added reçu", || {
        m.core
            .telemetry(&a.id)
            .events
            .iter()
            .any(|e| e.kind == Some(EK::Added(T1.into())))
    })
    .await;

    // Coupure du tunnel ; le client continue de travailler et termine le torrent.
    assert!(m.core.sessions.kill(&a.id, "test"));
    eventually("tunnel coupé", || !m.core.sessions.is_connected(&a.id)).await;
    a.qbit.complete(T1);

    // Après reconnexion : l'événement mis en outbox pendant la coupure est livré.
    eventually("Completed rejoué", || {
        m.core
            .telemetry(&a.id)
            .events
            .iter()
            .any(|e| e.kind == Some(EK::Completed(T1.into())))
    })
    .await;
    assert!(m.core.store.get_torrent(T1).unwrap().unwrap().completed);
    // Acquitté -> purgé de l'outbox persistante.
    let outbox_path = a.dir.path().join("outbox.log");
    eventually("outbox purgée", || {
        Outbox::open(outbox_path.clone())
            .unwrap()
            .pending()
            .is_empty()
    })
    .await;
    // Un rejeu ne crée pas de doublon.
    let n = m
        .core
        .telemetry(&a.id)
        .events
        .iter()
        .filter(|e| e.kind == Some(EK::Completed(T1.into())))
        .count();
    assert_eq!(n, 1);
    a.stop().await;
}

#[tokio::test]
async fn offline_primary_is_reassigned_after_grace_and_old_copy_removed_on_return() {
    let mut s = settings();
    s.offline_grace = Duration::from_secs(1);
    s.sweep_interval = Duration::from_millis(200);
    let m = start_master(s).await;
    let _sweeper = m.core.start_background();
    let mut a = add_node(&m, &[]).await;
    let mut b = add_node(&m, &[]).await;
    a.start().await;
    b.start().await;

    m.core.add_torrent(new_t(T1, &[], None)).await.unwrap();
    eventually("assigné", || m.core.torrent_primary(T1).is_some()).await;
    let first = m.core.torrent_primary(T1).unwrap();
    let (old, other) = if first == a.id {
        (&mut a, &mut b)
    } else {
        (&mut b, &mut a)
    };
    eventually("présent sur le primaire", || old.qbit.get(T1).is_some()).await;

    // Le primaire tombe : après le délai de grâce, un nœud conforme reprend le torrent.
    old.stop().await;
    eventually("réassigné", || {
        m.core.torrent_primary(T1).as_deref() == Some(other.id.as_str())
    })
    .await;
    eventually("présent sur le remplaçant", || {
        other.qbit.get(T1).is_some()
    })
    .await;

    // Au retour de l'ancien nœud, le master (source de vérité) lui fait retirer sa copie.
    old.start().await;
    eventually("copie obsolète retirée", || old.qbit.get(T1).is_none()).await;
    assert!(other.qbit.get(T1).is_some());
    other.stop().await;
    old.stop().await;
}

#[tokio::test]
async fn completion_while_agent_process_was_down_is_derived_from_full_state() {
    let m = start_master(settings()).await;
    let mut src = add_node(&m, &["src"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    src.start().await;
    b.start().await;
    m.core
        .add_torrent(new_t(T1, &["src"], Some("mirror")))
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(T1).is_some()).await;

    // Le processus agent s'arrête ; le client torrent, lui, termine le téléchargement.
    src.stop().await;
    src.qbit.complete(T1);
    // Au redémarrage, la ligne de base locale est déjà « complet » : aucun événement Completed.
    src.start().await;
    // Le master le déduit du FullState et lance la réplication.
    eventually("marqué complet", || {
        m.core.store.get_torrent(T1).unwrap().unwrap().completed
    })
    .await;
    eventually("réplica créé", || b.qbit.get(T1).is_some()).await;
    src.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn torrent_file_is_delivered_to_the_node_and_replayed_after_reconnect() {
    let m = start_master(settings()).await;
    let mut a = add_node(&m, &[]).await;
    a.start().await;

    // Fichier .torrent (tracker privé : le magnet seul ne suffirait pas).
    let file = testkit::torrent::sample("private-seed", 8192, 0x77);
    let ih = testkit::torrent::hash_from_file(&file).unwrap();
    let got = m
        .core
        .add_torrent(NewTorrent {
            source: Source::File(file),
            save_subdir: "priv".into(),
            requires: vec![],
            replicate_group: None,
            size_hint: 0,
            name: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(got, ih);
    eventually("torrent sur le nœud", || a.qbit.get(&ih).is_some()).await;
    assert!(a.qbit.get(&ih).unwrap().save_path.ends_with("priv"));

    // Le client perd le torrent pendant une coupure : la reconnexion le réinjecte AVEC son fichier.
    a.stop().await;
    a.qbit.remove(&ih);
    a.start().await;
    eventually("torrent réinjecté", || a.qbit.get(&ih).is_some()).await;
    a.stop().await;
}

#[tokio::test]
async fn placement_can_be_changed_after_the_fact_to_unblock_and_replicate() {
    let m = start_master(settings()).await;
    let mut a = add_node(&m, &["ssd"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    a.start().await;
    b.start().await;

    // Exige un label que personne n'a : reste en attente.
    m.core.add_torrent(new_t(T1, &["tor"], None)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(m.core.torrent_primary(T1), None);

    // Retirer l'exigence débloque le torrent (nœud le moins chargé conforme).
    m.core
        .set_torrent_placement_as("admin:test", T1, vec![], None)
        .await
        .unwrap();
    eventually("assigné", || m.core.torrent_primary(T1).is_some()).await;
    let primary_is_a = m.core.torrent_primary(T1).as_deref() == Some(a.id.as_str());
    let (src, other) = if primary_is_a { (&a, &b) } else { (&b, &a) };
    eventually("sur le primaire", || src.qbit.get(T1).is_some()).await;
    src.qbit.complete(T1);
    eventually("complété", || {
        m.core.store.get_torrent(T1).unwrap().unwrap().completed
    })
    .await;
    assert!(other.qbit.get(T1).is_none(), "pas de groupe : pas de copie");

    // Groupe de réplication ajouté APRÈS la fin du téléchargement : la copie part sur le nœud « mirror ».
    m.core
        .set_torrent_placement_as("admin:test", T1, vec![], Some("mirror".into()))
        .await
        .unwrap();
    if primary_is_a {
        eventually("copie sur b", || b.qbit.get(T1).is_some()).await;
    } else {
        // Le primaire est déjà le nœud du groupe : rien à copier.
        assert!(a.qbit.get(T1).is_none());
    }

    // Entrées invalides refusées.
    assert!(m
        .core
        .set_torrent_placement_as("admin:test", T1, vec!["a b".into()], None)
        .await
        .is_err());
    assert!(m
        .core
        .set_torrent_placement_as("admin:test", T2, vec![], None)
        .await
        .is_err());
    for n in [&mut a, &mut b] {
        n.stop().await;
    }
}

#[tokio::test]
async fn admin_speed_limits_are_applied_on_connect_and_recheck_reaches_the_client() {
    let m = start_master(settings()).await;
    let mut a = add_node(&m, &[]).await;
    // Sans réglage de l'admin, le master ne touche pas aux limites locales du client.
    a.start().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(a.qbit.limits(), (0, 0), "limites locales préservées");
    a.stop().await;

    // L'admin fixe des limites : elles sont (ré)appliquées à la connexion suivante.
    assert!(m
        .core
        .store
        .set_node_limits(&a.id, 5_000_000, 2_000_000)
        .unwrap());
    a.start().await;
    eventually("limites appliquées", || {
        a.qbit.limits() == (5_000_000, 2_000_000)
    })
    .await;
    // L'agent se reconnecte (nouvelle session) : les limites sont réenvoyées.
    a.stop().await;
    a.qbit.insert(MockTorrent::new(T3, "stalledUP", 1.0, "/x")); // activité quelconque
    a.start().await;
    eventually("limites réappliquées", || {
        a.qbit.limits() == (5_000_000, 2_000_000)
    })
    .await;
    // Retour à « illimité » = 0/0 explicite (et non plus « géré par le client »).
    assert!(m.core.store.set_node_limits(&a.id, 0, 0).unwrap());
    assert!(m.core.apply_limits(&a.id).await);
    eventually("illimité", || a.qbit.limits() == (0, 0)).await;

    // Revérification demandée par l'admin : atteint le client du nœud qui porte le torrent.
    m.core.add_torrent(new_t(T1, &[], None)).await.unwrap();
    eventually("T1 sur le nœud", || a.qbit.get(T1).is_some()).await;
    a.qbit.complete(T1);
    eventually("terminé", || {
        a.qbit.get(T1).is_some_and(|t| t.state == "stalledUP")
    })
    .await;
    assert_eq!(
        m.core.recheck_torrent_as("admin:test", T1).await.unwrap(),
        1
    );
    eventually("revérification lancée", || {
        a.qbit
            .get(T1)
            .is_some_and(|t| t.state.starts_with("checking"))
    })
    .await;
    // Torrent inconnu / nœud hors ligne : erreurs claires.
    assert!(m.core.recheck_torrent_as("admin:test", T2).await.is_err());
    a.stop().await;
    eventually("nœud déconnecté", || {
        !m.core.sessions.is_connected(&a.id)
    })
    .await;
    assert!(
        m.core.recheck_torrent_as("admin:test", T1).await.is_err(),
        "aucun nœud connecté"
    );
}

/// Réplication SANS stockage partagé : la copie se télécharge par BitTorrent via le tracker privé du master, puis est
/// rebranchée sur le torrent d'origine (cross-seed).
#[tokio::test]
async fn swarm_replication_transfers_via_private_tracker_then_retargets_to_the_original() {
    let m = start_master(settings()).await;
    let tracker = m.core.enable_swarm("http://127.0.0.1:7444").unwrap();
    // Plafond de débit des copies : appliqué à la copie pendant son transfert.
    m.core
        .store
        .setting_set("copy_dl_limit", "5000000")
        .unwrap();
    let mut src = add_node(&m, &["src"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    for n in [&mut src, &mut b] {
        n.start().await;
    }
    let original = testkit::torrent::sample("film.mkv", 32768, 7);
    let h = testkit::torrent::hash_from_file(&original).unwrap();
    let ih: [u8; 20] = data_encoding::HEXLOWER
        .decode(h.as_bytes())
        .unwrap()
        .try_into()
        .unwrap();
    m.core
        .add_torrent(NewTorrent {
            source: Source::File(original.clone()),
            save_subdir: "movies".into(),
            requires: vec!["src".into()],
            replicate_group: Some("mirror".into()),
            size_hint: 0,
            name: String::new(),
        })
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    assert_eq!(
        src.qbit.added_files()[0].1,
        original,
        "la source reçoit l'ORIGINAL"
    );

    // La source finit : une copie est placée en mode Transfer, avec le torrent RÉÉCRIT.
    src.qbit.complete(&h);
    eventually("copie en transfert", || b.qbit.get(&h).is_some()).await;
    let (fh, bytes) = b.qbit.added_files()[0].clone();
    assert_eq!(fh, h, "même infohash");
    let meta = proto::metainfo::parse(&bytes).unwrap();
    let expect = format!("http://127.0.0.1:7444{}", tracker.announce_path(&ih, &b.id));
    assert_eq!(
        meta.trackers,
        vec![expect],
        "n'annonce QU'AU tracker privé, jeton propre au nœud"
    );
    assert_ne!(bytes, original);
    assert_eq!(
        b.qbit.get(&h).unwrap().state,
        "downloading",
        "le transfert démarre tout de suite"
    );
    // La source annonce aussi au tracker privé (pour que la copie la trouve), avec SON jeton.
    eventually("la source annonce au tracker", || {
        !src.qbit.added_trackers().is_empty()
    })
    .await;
    assert_eq!(
        src.qbit.added_trackers(),
        vec![(
            h.clone(),
            format!(
                "http://127.0.0.1:7444{}",
                tracker.announce_path(&ih, &src.id)
            )
        )]
    );
    eventually("plafond de débit appliqué à la copie", || {
        b.qbit.torrent_limits(&h) == Some((5_000_000, 0))
    })
    .await;
    assert_eq!(
        src.qbit.torrent_limits(&h),
        None,
        "la source n'est pas bridée"
    );
    let phases = |m: &Master| {
        m.core
            .store
            .assignments_for_torrent(&h)
            .unwrap()
            .into_iter()
            .find(|r| r.role == "replica")
            .map(|r| r.phase)
    };
    assert_eq!(phases(&m).as_deref(), Some("Transfer"));

    // La copie se termine : retrait SANS les fichiers, puis l'ORIGINAL ré-ajouté en pause et revérifié.
    b.qbit.complete(&h);
    eventually("original ré-ajouté", || b.qbit.added_files().len() == 2).await;
    assert_eq!(
        b.qbit.added_files()[1].1,
        original,
        "retour au torrent d'origine"
    );
    eventually("revérification", || {
        b.qbit.get(&h).is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    b.qbit.finish_check(&h);
    eventually("partage sur l'original", || {
        b.qbit.get(&h).is_some_and(|t| t.state == "stalledUP")
    })
    .await;
    eventually("phase Active", || phases(&m).as_deref() == Some("Active")).await;
    assert_eq!(
        src.qbit.added_trackers().len(),
        1,
        "l'annonce n'est pas renvoyée en boucle"
    );
    for n in [&mut src, &mut b] {
        n.stop().await;
    }
}

/// Sans tracker activé, le comportement historique est inchangé (stockage partagé : ajout en pause + recheck).
#[tokio::test]
async fn without_a_tracker_replicas_keep_the_shared_storage_cycle() {
    let m = start_master(settings()).await;
    assert!(m.core.swarm().is_none());
    let mut src = add_node(&m, &["src"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    for n in [&mut src, &mut b] {
        n.start().await;
    }
    let original = testkit::torrent::sample("a.mkv", 32768, 9);
    let h = testkit::torrent::hash_from_file(&original).unwrap();
    m.core
        .add_torrent(NewTorrent {
            source: Source::File(original.clone()),
            save_subdir: "movies".into(),
            requires: vec!["src".into()],
            replicate_group: Some("mirror".into()),
            size_hint: 0,
            name: String::new(),
        })
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    src.qbit.complete(&h);
    eventually("copie en pause puis recheck", || {
        b.qbit.get(&h).is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    assert_eq!(
        b.qbit.added_files()[0].1,
        original,
        "torrent d'origine, pas réécrit"
    );
    assert!(src.qbit.added_trackers().is_empty());
    for n in [&mut src, &mut b] {
        n.stop().await;
    }
}

/// Torrent ajouté par MAGNET : le master n'a pas le .torrent ; il le demande au nœud qui a fini, puis la copie
/// démarre en mode swarm avec le torrent réécrit.
#[tokio::test]
async fn magnet_torrent_gets_its_file_from_the_node_then_replicates_via_swarm() {
    let m = start_master(settings()).await;
    m.core.enable_swarm("http://127.0.0.1:7444").unwrap();
    let mut src = add_node(&m, &["src"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    for n in [&mut src, &mut b] {
        n.start().await;
    }
    let original = testkit::torrent::sample("serie.mkv", 32768, 11);
    let h = testkit::torrent::hash_from_file(&original).unwrap();
    m.core
        .add_torrent(new_t(&h, &["src"], Some("mirror")))
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    assert!(!m.core.store.get_torrent(&h).unwrap().unwrap().has_file);
    src.qbit.set_export(&h, original.clone());
    src.qbit.complete(&h);
    eventually("le master récupère le .torrent", || {
        m.core.store.torrent_file(&h).unwrap().as_deref() == Some(original.as_slice())
    })
    .await;
    // La copie démarre en mode swarm : torrent RÉÉCRIT (autre contenu, même infohash).
    eventually("copie en transfert", || b.qbit.get(&h).is_some()).await;
    let (fh, bytes) = b.qbit.added_files()[0].clone();
    assert_eq!(fh, h);
    assert_ne!(bytes, original);
    assert_eq!(src.qbit.export_calls(), 1, "demandé une seule fois");
    for n in [&mut src, &mut b] {
        n.stop().await;
    }
}

/// Un nœud ne peut pas imposer un .torrent : réponse non sollicitée, mauvais infohash ou fichier invalide = refus.
#[tokio::test]
async fn unsolicited_or_mismatching_metainfo_is_refused() {
    let m = start_master(settings()).await;
    m.core.enable_swarm("http://127.0.0.1:7444").unwrap();
    let a = testkit::torrent::sample("a.mkv", 32768, 21);
    let ha = testkit::torrent::hash_from_file(&a).unwrap();
    let other = testkit::torrent::sample("b.mkv", 32768, 22);
    m.core
        .add_torrent(new_t(&ha, &[], Some("g")))
        .await
        .unwrap();
    // Non sollicité (le master n'a rien demandé à ce nœud).
    m.core.on_metainfo("node-x", &ha, &a).await;
    assert!(m.core.store.torrent_file(&ha).unwrap().is_none());
    // Sollicité mais avec le .torrent d'un AUTRE torrent, puis avec du bruit.
    m.core.note_metainfo_asked(&ha, "node-x");
    m.core.on_metainfo("node-x", &ha, &other).await;
    m.core.on_metainfo("node-x", &ha, b"pas un torrent").await;
    assert!(m.core.store.torrent_file(&ha).unwrap().is_none());
    // Le bon fichier est accepté, une seule fois (le premier gagne).
    m.core.on_metainfo("node-x", &ha, &a).await;
    assert_eq!(
        m.core.store.torrent_file(&ha).unwrap().as_deref(),
        Some(a.as_slice())
    );
}

/// Le client du nœud ne sait pas exporter le .torrent : on n'attend pas indéfiniment, repli sur l'ancien cycle.
#[tokio::test]
async fn magnet_torrent_falls_back_to_shared_storage_when_the_client_cannot_export() {
    let m = start_master(settings()).await;
    m.core.enable_swarm("http://127.0.0.1:7444").unwrap();
    let mut src = add_node(&m, &["src"]).await;
    let mut b = add_node(&m, &["mirror"]).await;
    for n in [&mut src, &mut b] {
        n.start().await;
    }
    let h =
        testkit::torrent::hash_from_file(&testkit::torrent::sample("x.mkv", 32768, 31)).unwrap();
    m.core
        .add_torrent(new_t(&h, &["src"], Some("mirror")))
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    // Pas de set_export : le faux client répond « conflit », l'agent acquitte en échec.
    src.qbit.complete(&h);
    eventually("repli : copie en pause puis recheck", || {
        b.qbit.get(&h).is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    assert!(
        b.qbit.added_files().is_empty(),
        "ajout par magnet, comme avant"
    );
    for n in [&mut src, &mut b] {
        n.stop().await;
    }
}

// ---------- Cross-seed ----------

use testkit::torznab::MockTorznab;

fn xs_data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Torrent d'un contenu donné, annoncé à `tracker` : mêmes fichiers => même clé de contenu, infohash différent.
fn xs_torrent(name: &str, data: &[u8], tracker: &str) -> Vec<u8> {
    // Torrent « privé » d'un autre tracker : le drapeau est DANS `info`, donc l'infohash diffère de l'original
    // (qui, lui, n'est pas privé) pour un contenu identique.
    testkit::torrent::for_data(
        name,
        data,
        16384,
        tracker,
        tracker.contains("tracker-b") || tracker.contains("tracker-c") || tracker.contains("other"),
    )
}

struct Xs {
    m: Master,
    src: Node,
    idx: MockTorznab,
    h: String,
}

async fn xs_setup(mode: &str) -> Xs {
    let m = start_master(settings()).await;
    let mut src = add_node(&m, &["src"]).await;
    src.start().await;
    let parent = xs_torrent("film.mkv", &xs_data(40_000), "http://tracker-a.test/ann");
    let h = testkit::torrent::hash_from_file(&parent).unwrap();
    m.core
        .add_torrent(NewTorrent {
            source: Source::File(parent),
            save_subdir: "movies".into(),
            requires: vec!["src".into()],
            replicate_group: None,
            size_hint: 0,
            name: String::new(),
        })
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    src.qbit.complete(&h);
    let (id, core, hh) = (src.id.clone(), m.core.clone(), h.clone());
    eventually("la complétion remonte au master", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == hh && t.progress >= 0.9999)
    })
    .await;
    let idx = MockTorznab::start("k").await;
    m.core
        .store
        .indexer_add("ix1", "Mock", &idx.url(), "k")
        .unwrap();
    m.core.store.setting_set("xseed_mode", mode).unwrap();
    m.core.set_xseed_tuning(0, 0, 0);
    Xs { m, src, idx, h }
}

/// Exécute le suivi du cross-seed puis dit si une ligne du journal du parent est dans l'état voulu.
async fn xs_follow_until(core: &Arc<Core>, parent: &str, state: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        core.xseed_follow_up().await;
        if core
            .store
            .xseed_log(50)
            .unwrap()
            .iter()
            .any(|r| r.parent == parent && r.state == state)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "délai dépassé : état « {state} » attendu ; journal {:?} ; assignations {:?}",
            core.store.xseed_log(50).unwrap(),
            core.store
                .all_assignments()
                .unwrap()
                .iter()
                .map(|a| (
                    a.infohash[..8].to_owned(),
                    a.role.clone(),
                    a.phase.clone(),
                    a.removed
                ))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn cross_seed_finds_the_same_content_on_other_trackers_and_seeds_it_without_downloading() {
    let mut x = xs_setup("active").await;
    let data = xs_data(40_000);
    let good = xs_torrent("film.mkv", &data, "http://tracker-b.test/ann"); // même contenu, autre tracker
    let other_content = xs_torrent("film.mkv", &xs_data(39_000), "http://tracker-c.test/ann"); // autre taille
                                                                                               // Même hôte de tracker que l'original ; pièces de 32 Kio pour un infohash distinct de `good`.
    let same_tracker = testkit::torrent::for_data(
        "film.mkv",
        &data,
        32768,
        "http://tracker-a.test/other",
        true,
    );
    let good_h = testkit::torrent::hash_from_file(&good).unwrap();
    x.idx.set_results(vec![
        ("Film 2024 [B]", good.clone()),
        ("Film 2024 autre", other_content),
        ("Film 2024 [A2]", same_tracker),
    ]);

    let sum = x.m.core.xseed_sweep().await;
    assert_eq!((sum.searched, sum.matched), (1, 1), "{sum:?}");
    assert_eq!(
        x.idx.searches(),
        vec!["film mkv".to_owned()],
        "seul le nom est envoyé à l'indexeur"
    );

    // Le candidat est ajouté sur le MÊME nœud, même dossier, en pause puis revérifié : aucun téléchargement.
    eventually("candidat ajouté puis revérifié", || {
        x.src
            .qbit
            .get(&good_h)
            .is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    let t = x.src.qbit.get(&good_h).unwrap();
    assert!(t.save_path.ends_with("movies"), "{}", t.save_path);
    assert!(x
        .src
        .qbit
        .added_files()
        .iter()
        .any(|(h, b)| *h == good_h && *b == good));
    // Contenu complet : la vérification le confirme, le candidat partage.
    x.src.qbit.finish_check(&good_h);
    let (id, core, gh) = (x.src.id.clone(), x.m.core.clone(), good_h.clone());
    eventually("la fin de vérification remonte au master", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == gh && t.progress >= 0.9999)
    })
    .await;
    xs_follow_until(&x.m.core, &x.h, "seeding").await;

    let log = x.m.core.store.xseed_log(50).unwrap();
    assert!(log
        .iter()
        .any(|r| r.candidate == good_h && r.state == "seeding"));
    let refused: Vec<_> = log.iter().filter(|r| r.state == "refused").collect();
    assert_eq!(refused.len(), 1, "{log:?}");
    assert!(refused[0].detail.contains("même tracker"));
    assert!(
        log.iter().all(|r| !r.tracker.contains("://")),
        "jamais l'URL du tracker"
    );
    // Rattaché à son parent (regroupement), jamais replacé sur un autre nœud.
    let kids = x.m.core.store.xseed_children().unwrap();
    assert_eq!(kids.get(&good_h), Some(&x.h));
    assert_eq!(
        x.m.core
            .store
            .assignments_for_torrent(&good_h)
            .unwrap()
            .len(),
        1
    );
    // Une seconde passe ne refait rien : candidats déjà connus.
    let again = x.m.core.xseed_sweep().await;
    assert_eq!(again.matched, 0);
    x.src.stop().await;
}

#[tokio::test]
async fn simulation_mode_only_reports_and_adds_nothing() {
    let mut x = xs_setup("dry").await;
    let good = xs_torrent("film.mkv", &xs_data(40_000), "http://tracker-b.test/ann");
    let good_h = testkit::torrent::hash_from_file(&good).unwrap();
    x.idx.set_results(vec![("Film [B]", good)]);
    let sum = x.m.core.xseed_sweep().await;
    assert_eq!(sum.matched, 1);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        x.src.qbit.get(&good_h).is_none(),
        "rien n'est ajouté en simulation"
    );
    assert!(x.m.core.store.get_torrent(&good_h).unwrap().is_none());
    let log = x.m.core.store.xseed_log(10).unwrap();
    assert_eq!((log.len(), log[0].state.as_str()), (1, "found"));
    // Par défaut (aucun réglage) : la simulation.
    let fresh = start_master(settings()).await;
    assert_eq!(fresh.core.xseed_mode(), master::crossseed::XseedMode::Dry);
    x.src.stop().await;
}

#[tokio::test]
async fn an_incomplete_candidate_is_removed_and_never_downloaded() {
    let mut x = xs_setup("active").await;
    // Le fichier d'origine existe réellement sur le disque du nœud (le candidat vivra sur des liens isolés).
    std::fs::create_dir_all(x.src.root.path().join("movies")).unwrap();
    std::fs::write(x.src.root.path().join("movies/film.mkv"), xs_data(40_000)).unwrap();
    // Même nom et même taille, mais le contenu DIFFÈRE : le client le constatera à la vérification.
    let mut data = xs_data(40_000);
    data[100] ^= 0xFF;
    let fake = xs_torrent("film.mkv", &data, "http://tracker-b.test/ann");
    let fake_h = testkit::torrent::hash_from_file(&fake).unwrap();
    x.idx.set_results(vec![("Film [B]", fake)]);
    assert_eq!(x.m.core.xseed_sweep().await.matched, 1);
    eventually("revérification demandée", || {
        x.src
            .qbit
            .get(&fake_h)
            .is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    // La vérification ne trouve que 40 % : le client reste en pause.
    let path = x.src.qbit.get(&fake_h).unwrap().save_path;
    x.src
        .qbit
        .insert(MockTorrent::new(&fake_h, "pausedDL", 0.4, &path));
    // Le FullState suivant remonte l'avancement au master.
    let (id, core, fh) = (x.src.id.clone(), x.m.core.clone(), fake_h.clone());
    eventually("avancement remonté", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == fh && (t.progress - 0.4).abs() < 0.01)
    })
    .await;
    xs_follow_until(&x.m.core, &x.h, "removed").await;
    let row = &x.m.core.store.xseed_log(10).unwrap()[0];
    assert!(
        row.detail.contains("incomplet") && row.detail.contains("40 %"),
        "{}",
        row.detail
    );
    eventually("retiré du client", || x.src.qbit.get(&fake_h).is_none()).await;
    assert!(
        x.m.core
            .store
            .get_torrent(&fake_h)
            .unwrap()
            .unwrap()
            .deleted
    );
    // Le candidat (contenu différent, donc jugé « partiel » par un client qui renomme les fichiers incomplets) vivait
    // sur ses PROPRES liens : il est retiré avec eux, et l'original n'est jamais touché.
    assert!(
        x.src.qbit.deleted().contains(&(fake_h.clone(), true)),
        "{:?}",
        x.src.qbit.deleted()
    );
    assert_eq!(
        std::fs::read(x.src.root.path().join("movies/film.mkv")).unwrap(),
        xs_data(40_000),
        "le fichier d'origine est intact"
    );
    // L'original n'est pas touché.
    assert!(x.src.qbit.get(&x.h).is_some());
    x.src.stop().await;
}

// ---------- Cross-seed : protection des comptes (rate-limiting, erreurs des indexeurs) ----------

use testkit::torznab::Behavior;

fn first_indexer(core: &Arc<Core>) -> master::store::IndexerRow {
    core.store.indexers().unwrap().remove(0)
}

/// Trois candidats de même contenu (infohash distincts), pour consommer des requêtes de téléchargement.
fn xs_three_candidates(x: &Xs) {
    let data = xs_data(40_000);
    let mk = |piece: usize, tracker: &str| {
        testkit::torrent::for_data("film.mkv", &data, piece, tracker, true)
    };
    x.idx.set_results(vec![
        ("Film [B]", mk(16384, "http://tracker-b.test/ann")),
        ("Film [C]", mk(32768, "http://tracker-c.test/ann")),
        ("Film [D]", mk(65536, "http://tracker-d.test/ann")),
    ]);
}

#[tokio::test]
async fn a_429_with_retry_after_pauses_the_indexer_and_nothing_more_is_sent() {
    let mut x = xs_setup("dry").await;
    x.idx.set_behavior(Behavior::Status(429, Some(900)));
    let sum = x.m.core.xseed_sweep().await;
    assert_eq!(sum.searched, 0, "aucune recherche n'a abouti");
    assert_eq!(
        x.idx.requests(),
        1,
        "une seule requête : on s'arrête dès le 429"
    );
    let ix = first_indexer(&x.m.core);
    let now = master::store::unix_now();
    assert!(
        ix.enabled && ix.disabled_reason.is_empty(),
        "un 429 met en pause, il ne désactive pas"
    );
    assert!(
        ix.paused_until >= now + 890 && ix.paused_until <= now + 910,
        "pause de ~900 s (Retry-After) : {}",
        ix.paused_until - now
    );
    assert_eq!(ix.fail_count, 1);
    // Tant que la pause dure, AUCUNE requête, même en lançant à la main (période de re-recherche ignorée).
    for _ in 0..3 {
        x.m.core.xseed_sweep_with(true).await;
    }
    assert_eq!(
        x.idx.requests(),
        1,
        "indexeur en pause : plus rien n'est envoyé"
    );
    // La pause est visible dans le journal, avec sa durée.
    let log =
        x.m.core
            .store
            .audit_list(10, None, Some("xseed_indexer_paused"))
            .unwrap();
    assert!(log.iter().any(|r| r.detail.starts_with("Mock|")), "{log:?}");
    // La pause finie et l'indexeur rétabli : tout reprend et l'échec est oublié.
    x.idx.set_behavior(Behavior::Normal);
    xs_three_candidates(&x);
    x.m.core.store.indexer_success(&ix.id).unwrap();
    assert_eq!(x.m.core.xseed_sweep_with(true).await.searched, 1);
    assert_eq!(first_indexer(&x.m.core).fail_count, 0);
    x.src.stop().await;
}

#[tokio::test]
async fn a_429_without_retry_after_uses_a_safe_default_and_absurd_values_are_clamped() {
    for (ra, lo, hi) in [
        (None, 290, 3700),
        (Some(1), 55, 3700),
        (Some(10_000_000), 86_000, 86_500),
    ] {
        let mut x = xs_setup("dry").await;
        x.idx.set_behavior(Behavior::Status(429, ra));
        x.m.core.xseed_sweep().await;
        let wait = first_indexer(&x.m.core).paused_until - master::store::unix_now();
        assert!(
            (lo..=hi).contains(&wait),
            "Retry-After {ra:?} : pause {wait} s hors de [{lo}, {hi}]"
        );
        x.src.stop().await;
    }
}

#[tokio::test]
async fn access_refused_disables_the_indexer_automatically_and_stops_all_requests() {
    for b in [
        Behavior::Status(403, None),
        Behavior::Status(401, None),
        Behavior::Error(100, "Incorrect user credentials"),
        Behavior::Error(101, "Account suspended"),
    ] {
        let mut x = xs_setup("active").await;
        x.idx.set_behavior(b.clone());
        x.m.core.xseed_sweep().await;
        let ix = first_indexer(&x.m.core);
        assert!(!ix.enabled, "{b:?} : désactivé automatiquement");
        assert!(
            ix.disabled_reason.contains("désactivé automatiquement"),
            "{}",
            ix.disabled_reason
        );
        assert_eq!(
            x.idx.requests(),
            1,
            "{b:?} : on n'insiste pas avec une clé refusée"
        );
        for _ in 0..3 {
            x.m.core.xseed_sweep_with(true).await;
        }
        assert_eq!(x.idx.requests(), 1, "{b:?} : plus aucune requête");
        // Seule une décision de l'admin (réactiver) rétablit l'indexeur et efface la raison.
        x.m.core.store.indexer_enable(&ix.id, true).unwrap();
        let again = first_indexer(&x.m.core);
        assert!(again.enabled && again.disabled_reason.is_empty() && again.fail_count == 0);
        x.src.stop().await;
    }
}

#[tokio::test]
async fn server_errors_back_off_and_torznab_quotas_pause_for_hours() {
    // 503 sans Retry-After : attente de base, puis plus aucune requête pendant cette attente.
    let mut x = xs_setup("dry").await;
    x.idx.set_behavior(Behavior::Status(503, None));
    x.m.core.xseed_sweep().await;
    x.m.core.xseed_sweep_with(true).await;
    assert_eq!(x.idx.requests(), 1);
    let ix = first_indexer(&x.m.core);
    let wait = ix.paused_until - master::store::unix_now();
    assert!(
        (55..=65).contains(&wait),
        "première attente ~1 min : {wait}"
    );
    assert!(ix.enabled && ix.fail_count == 1);
    x.src.stop().await;

    // Quota annoncé par l'indexeur (code Torznab 500) : pause de 12 h.
    let mut y = xs_setup("dry").await;
    y.idx
        .set_behavior(Behavior::Error(500, "Request limit reached"));
    y.m.core.xseed_sweep().await;
    let wait = first_indexer(&y.m.core).paused_until - master::store::unix_now();
    assert!(
        (12 * 3600 - 10..=12 * 3600 + 10).contains(&wait),
        "quota : 12 h, vu {wait}"
    );
    assert_eq!(y.idx.requests(), 1);
    y.src.stop().await;
}

#[tokio::test]
async fn the_hourly_budget_counts_searches_and_downloads_and_is_never_exceeded() {
    let mut x = xs_setup("dry").await;
    xs_three_candidates(&x);
    let ix = first_indexer(&x.m.core);
    // 3 requêtes par heure : 1 recherche + 2 téléchargements, le troisième résultat n'est PAS téléchargé.
    x.m.core
        .store
        .indexer_set_limits(&ix.id, 500, 3, 500, 10)
        .unwrap();
    let sum = x.m.core.xseed_sweep().await;
    assert_eq!(
        x.idx.requests(),
        3,
        "budget horaire respecté à la requête près"
    );
    assert_eq!(x.idx.searches().len(), 1);
    assert_eq!(x.idx.downloads().len(), 2);
    assert_eq!(sum.matched, 2);
    // Budget épuisé : les passages suivants n'envoient plus rien.
    x.m.core.set_xseed_tuning(0, 0, 0);
    for _ in 0..3 {
        x.m.core.xseed_sweep_with(true).await;
    }
    assert_eq!(x.idx.requests(), 3);
    // Le torrent n'est pas marqué « cherché » faute d'avoir pu aller au bout : il sera retenté quand le budget revient.
    x.src.stop().await;
}

#[tokio::test]
async fn at_most_max_candidates_results_are_downloaded_per_search() {
    let mut x = xs_setup("dry").await;
    xs_three_candidates(&x);
    let ix = first_indexer(&x.m.core);
    x.m.core
        .store
        .indexer_set_limits(&ix.id, 500, 100, 500, 1)
        .unwrap();
    x.m.core.xseed_sweep().await;
    assert_eq!(
        x.idx.downloads().len(),
        1,
        "un seul résultat examiné par recherche"
    );
    x.src.stop().await;
}

#[tokio::test]
async fn requests_are_spaced_by_the_indexers_own_minimum_interval() {
    let mut x = xs_setup("dry").await;
    xs_three_candidates(&x);
    let ix = first_indexer(&x.m.core);
    x.m.core
        .store
        .indexer_set_limits(&ix.id, 500, 100, 500, 10)
        .unwrap();
    // Production : le délai vient des réglages de l'indexeur (pas de forçage).
    x.m.core.set_xseed_tuning(u64::MAX, 0, 0);
    x.m.core.xseed_sweep().await;
    let t = x.idx.request_times();
    assert_eq!(t.len(), 4, "1 recherche + 3 téléchargements");
    for w in t.windows(2) {
        let gap = w[1].duration_since(w[0]).as_millis();
        assert!(
            gap >= 450,
            "deux requêtes à {gap} ms d'intervalle, 500 ms exigées"
        );
    }
    x.src.stop().await;
}

#[tokio::test]
async fn a_result_already_examined_is_never_downloaded_again() {
    let mut x = xs_setup("dry").await;
    // Contenu différent (taille annoncée identique) : téléchargé UNE fois, puis mémorisé comme déjà examiné.
    let other = testkit::torrent::for_data(
        "film.mkv",
        &xs_data(39_000),
        16384,
        "http://tracker-b.test/ann",
        true,
    );
    x.idx.set_results(vec![("Film [B]", other)]);
    x.idx.set_announced_size(Some(40_000));
    x.m.core.xseed_sweep().await;
    assert_eq!(x.idx.downloads().len(), 1);
    for _ in 0..3 {
        x.m.core.xseed_sweep_with(true).await;
    }
    assert_eq!(
        x.idx.downloads().len(),
        1,
        "pas de nouveau téléchargement du même résultat"
    );
    assert_eq!(
        x.idx.searches().len(),
        4,
        "(les recherches, elles, sont refaites à la demande)"
    );
    x.src.stop().await;
}

// ---------- Cross-seed avec LIENS PHYSIQUES (autre dossier racine, autres noms) ----------

/// Comme `xs_setup`, mais avec un torrent d'origine donné ET ses fichiers réellement présents sur le disque du nœud
/// (l'agent crée de VRAIS liens physiques sous sa racine).
async fn xs_setup_files(mode: &str, parent: Vec<u8>, disk: &[(&str, &[u8])]) -> Xs {
    let m = start_master(settings()).await;
    let mut src = add_node(&m, &["src"]).await;
    src.start().await;
    let meta = proto::metainfo::parse(&parent).unwrap();
    for (rel, data) in disk {
        let p = src.root.path().join("movies").join(&meta.name).join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    let h = meta.infohash.clone();
    m.core
        .add_torrent(NewTorrent {
            source: Source::File(parent),
            save_subdir: "movies".into(),
            requires: vec!["src".into()],
            replicate_group: None,
            size_hint: 0,
            name: String::new(),
        })
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    src.qbit.complete(&h);
    let (id, core, hh) = (src.id.clone(), m.core.clone(), h.clone());
    eventually("la complétion remonte au master", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == hh && t.progress >= 0.9999)
    })
    .await;
    let idx = MockTorznab::start("k").await;
    m.core
        .store
        .indexer_add("ix1", "Mock", &idx.url(), "k")
        .unwrap();
    m.core.store.setting_set("xseed_mode", mode).unwrap();
    m.core.set_xseed_tuning(0, 0, 0);
    Xs { m, src, idx, h }
}

fn film_files() -> (Vec<u8>, Vec<u8>) {
    (xs_data(40_000), (0..1_000).map(|i| (i % 7) as u8).collect())
}

#[tokio::test]
async fn a_different_root_folder_is_hard_linked_by_the_agent_then_verified_and_seeded() {
    let (video, subs) = film_files();
    let files: [(&str, &[u8]); 2] = [("video.mkv", &video), ("Subs/fr.srt", &subs)];
    let parent = testkit::torrent::for_files(
        "Film.2024",
        &files,
        16384,
        "http://tracker-a.test/ann",
        false,
    );
    let mut x = xs_setup_files("active", parent, &files).await;
    // Même contenu, AUTRE dossier racine, autre tracker privé.
    let cand = testkit::torrent::for_files(
        "Film.2024-GRP",
        &files,
        16384,
        "http://tracker-b.test/ann",
        true,
    );
    let ch = testkit::torrent::hash_from_file(&cand).unwrap();
    x.idx.set_results(vec![("Film.2024-GRP", cand.clone())]);

    assert_eq!(x.m.core.xseed_sweep().await.matched, 1);
    // Les liens PHYSIQUES existent sous la disposition du candidat, créés par l'agent, avant l'ajout au client.
    let link_dir = x
        .src
        .root
        .path()
        .join("movies/.nxt-xseed")
        .join(&ch[..16])
        .join("Film.2024-GRP");
    eventually("le candidat est ajouté au client", || {
        x.src.qbit.get(&ch).is_some()
    })
    .await;
    assert_eq!(std::fs::read(link_dir.join("video.mkv")).unwrap(), video);
    assert_eq!(std::fs::read(link_dir.join("Subs/fr.srt")).unwrap(), subs);
    // Un VRAI lien physique (pas une copie) : une écriture dans l'original se voit dans le lien.
    {
        use std::io::Write;
        let orig = x.src.root.path().join("movies/Film.2024/video.mkv");
        std::fs::OpenOptions::new()
            .append(true)
            .open(orig)
            .unwrap()
            .write_all(b"+")
            .unwrap();
        assert_eq!(
            std::fs::read(link_dir.join("video.mkv")).unwrap().last(),
            Some(&b'+')
        );
    }
    // Le client a reçu le torrent du candidat, dans le MÊME dossier d'enregistrement.
    assert!(x
        .src
        .qbit
        .added_files()
        .iter()
        .any(|(h, b)| *h == ch && *b == cand));
    let sp = x.src.qbit.get(&ch).unwrap().save_path;
    assert!(
        sp.contains(".nxt-xseed") && sp.contains(&ch[..16]),
        "dossier isolé : {sp}"
    );
    // L'original, lui, reste à sa place d'origine.
    assert!(x
        .src
        .root
        .path()
        .join("movies/Film.2024/video.mkv")
        .exists());
    // Vérification puis partage, comme un cross-seed sans liens.
    eventually("revérification", || {
        x.src.qbit.get(&ch).is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    x.src.qbit.finish_check(&ch);
    let (id, core, c2) = (x.src.id.clone(), x.m.core.clone(), ch.clone());
    eventually("la fin de vérification remonte", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == c2 && t.progress >= 0.9999)
    })
    .await;
    xs_follow_until(&x.m.core, &x.h, "seeding").await;
    assert!(x.m.core.store.xseed_linked_set().unwrap().contains(&ch));
    let log = x.m.core.store.xseed_log(10).unwrap();
    assert!(log
        .iter()
        .any(|r| r.candidate == ch && r.state == "seeding"));

    // La liste regroupe le torrent lié sous son original, et le dit.
    let rows = x.m.core.torrent_rows().unwrap();
    let parent_row = rows.iter().find(|r| r.infohash == x.h).unwrap();
    assert_eq!(
        rows.iter().filter(|r| r.infohash == ch).count(),
        0,
        "pas de ligne à part"
    );
    assert!(parent_row
        .members
        .iter()
        .any(|m| m.infohash == ch && m.linked));

    // Supprimer la ligne SANS cocher « supprimer les fichiers » : le torrent lié part avec ses liens (les fichiers
    // d'origine ne sont pas concernés : l'original, lui, est retiré sans ses fichiers).
    x.m.core
        .group_remove_as("admin", &x.h, false)
        .await
        .unwrap();
    eventually("retiré du client", || x.src.qbit.get(&ch).is_none()).await;
    let del = x.src.qbit.deleted();
    assert!(
        del.contains(&(ch.clone(), true)),
        "les liens du membre sont supprimés avec lui : {del:?}"
    );
    assert!(
        del.contains(&(x.h.clone(), false)),
        "l'original garde ses fichiers : {del:?}"
    );
    x.src.stop().await;
}

#[tokio::test]
async fn renamed_single_files_are_linked_under_the_candidates_name() {
    let data = xs_data(30_000);
    let parent =
        testkit::torrent::for_data("film.mkv", &data, 16384, "http://tracker-a.test/ann", false);
    let m = proto::metainfo::parse(&parent).unwrap();
    assert!(m.single_file);
    // Le fichier réel d'un mono-fichier est directement dans le dossier d'enregistrement.
    let h = m.infohash.clone();
    let mx = start_master(settings()).await;
    let mut src = add_node(&mx, &["src"]).await;
    src.start().await;
    std::fs::create_dir_all(src.root.path().join("movies")).unwrap();
    std::fs::write(src.root.path().join("movies/film.mkv"), &data).unwrap();
    mx.core
        .add_torrent(NewTorrent {
            source: Source::File(parent),
            save_subdir: "movies".into(),
            requires: vec!["src".into()],
            replicate_group: None,
            size_hint: 0,
            name: String::new(),
        })
        .await
        .unwrap();
    eventually("primaire", || src.qbit.get(&h).is_some()).await;
    src.qbit.complete(&h);
    let (id, core, hh) = (src.id.clone(), mx.core.clone(), h.clone());
    eventually("complet", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == hh && t.progress >= 0.9999)
    })
    .await;
    let idx = MockTorznab::start("k").await;
    mx.core
        .store
        .indexer_add("ix1", "Mock", &idx.url(), "k")
        .unwrap();
    mx.core.store.setting_set("xseed_mode", "active").unwrap();
    mx.core.set_xseed_tuning(0, 0, 0);
    // Même taille, NOM DE FICHIER différent, autre tracker privé.
    let cand = testkit::torrent::for_data(
        "Film.2024.1080p.mkv",
        &data,
        16384,
        "http://tracker-b.test/ann",
        true,
    );
    let ch = testkit::torrent::hash_from_file(&cand).unwrap();
    idx.set_results(vec![("Film.2024.1080p", cand)]);
    assert_eq!(mx.core.xseed_sweep().await.matched, 1);
    eventually("candidat ajouté", || src.qbit.get(&ch).is_some()).await;
    assert_eq!(
        std::fs::read(
            src.root
                .path()
                .join("movies/.nxt-xseed")
                .join(&ch[..16])
                .join("Film.2024.1080p.mkv")
        )
        .unwrap(),
        data
    );
    assert_eq!(
        std::fs::read(src.root.path().join("movies/film.mkv")).unwrap(),
        data,
        "l'original est intact"
    );
    src.stop().await;
}

#[tokio::test]
async fn when_the_agent_cannot_create_the_links_the_candidate_is_dropped_and_never_added() {
    let (video, subs) = film_files();
    let files: [(&str, &[u8]); 2] = [("video.mkv", &video), ("Subs/fr.srt", &subs)];
    let parent = testkit::torrent::for_files(
        "Film.2024",
        &files,
        16384,
        "http://tracker-a.test/ann",
        false,
    );
    // Les fichiers de l'original sont ABSENTS du disque du nœud : la création des liens échoue.
    let mut x = xs_setup_files("active", parent, &[]).await;
    let cand = testkit::torrent::for_files(
        "Film.2024-GRP",
        &files,
        16384,
        "http://tracker-b.test/ann",
        true,
    );
    let ch = testkit::torrent::hash_from_file(&cand).unwrap();
    x.idx.set_results(vec![("Film.2024-GRP", cand)]);
    x.m.core.xseed_sweep().await;
    let core = x.m.core.clone();
    eventually("le candidat est abandonné", || {
        core.store
            .xseed_log(10)
            .unwrap()
            .iter()
            .any(|r| r.candidate == ch && r.state == "refused")
    })
    .await;
    let row = x.m.core.store.xseed_log(10).unwrap().remove(0);
    assert!(row.detail.contains("liens impossibles"), "{}", row.detail);
    assert!(x.src.qbit.get(&ch).is_none(), "jamais ajouté au client");
    assert!(
        !x.src.root.path().join("movies/.nxt-xseed").exists(),
        "aucun dossier laissé"
    );
    assert!(x.m.core.store.get_torrent(&ch).unwrap().unwrap().deleted);
    assert!(
        x.src.qbit.get(&x.h).is_some(),
        "l'original n'est pas touché"
    );
    x.src.stop().await;
}

#[tokio::test]
async fn simulation_announces_that_links_would_be_needed_and_creates_none() {
    let (video, subs) = film_files();
    let files: [(&str, &[u8]); 2] = [("video.mkv", &video), ("Subs/fr.srt", &subs)];
    let parent = testkit::torrent::for_files(
        "Film.2024",
        &files,
        16384,
        "http://tracker-a.test/ann",
        false,
    );
    let mut x = xs_setup_files("dry", parent, &files).await;
    let cand = testkit::torrent::for_files(
        "Film.2024-GRP",
        &files,
        16384,
        "http://tracker-b.test/ann",
        true,
    );
    x.idx.set_results(vec![("Film.2024-GRP", cand)]);
    assert_eq!(x.m.core.xseed_sweep().await.matched, 1);
    let row = x.m.core.store.xseed_log(10).unwrap().remove(0);
    assert_eq!(row.state, "found");
    assert!(
        row.detail.contains("liens physiques") && row.detail.contains("2 fichiers"),
        "{}",
        row.detail
    );
    assert!(
        !x.src.root.path().join("movies/.nxt-xseed").exists(),
        "rien n'est créé en simulation"
    );
    x.src.stop().await;
}

#[tokio::test]
async fn an_incomplete_linked_candidate_is_removed_together_with_its_links() {
    let (video, subs) = film_files();
    let files: [(&str, &[u8]); 2] = [("video.mkv", &video), ("Subs/fr.srt", &subs)];
    let parent = testkit::torrent::for_files(
        "Film.2024",
        &files,
        16384,
        "http://tracker-a.test/ann",
        false,
    );
    let mut x = xs_setup_files("active", parent, &files).await;
    // Mêmes noms et tailles mais UN octet différent : autre infohash ; la vérification restera incomplète.
    let mut bad = video.clone();
    bad[100] ^= 0xFF;
    let bad_files: [(&str, &[u8]); 2] = [("video.mkv", &bad), ("Subs/fr.srt", &subs)];
    let cand = testkit::torrent::for_files(
        "Film.2024-GRP",
        &bad_files,
        16384,
        "http://tracker-b.test/ann",
        true,
    );
    let ch = testkit::torrent::hash_from_file(&cand).unwrap();
    x.idx.set_results(vec![("Film.2024-GRP", cand)]);
    assert_eq!(x.m.core.xseed_sweep().await.matched, 1);
    eventually("revérification demandée", || {
        x.src.qbit.get(&ch).is_some_and(|t| t.state == "checkingDL")
    })
    .await;
    let path = x.src.qbit.get(&ch).unwrap().save_path;
    x.src
        .qbit
        .insert(MockTorrent::new(&ch, "pausedDL", 0.6, &path));
    let (id, core, c2) = (x.src.id.clone(), x.m.core.clone(), ch.clone());
    eventually("avancement remonté", || {
        core.telemetry(&id)
            .torrents
            .iter()
            .any(|t| t.infohash == c2 && (t.progress - 0.6).abs() < 0.01)
    })
    .await;
    xs_follow_until(&x.m.core, &x.h, "removed").await;
    eventually("retiré du client", || x.src.qbit.get(&ch).is_none()).await;
    assert!(
        x.src.qbit.deleted().contains(&(ch.clone(), true)),
        "un candidat lié est retiré AVEC ses liens : {:?}",
        x.src.qbit.deleted()
    );
    x.src.stop().await;
}

// ---------- Création de .torrent depuis des fichiers d'un nœud ----------

use master::torrentjobs::{CreateRequest, JobError};

async fn job_done(core: &Arc<Core>, id: &str) -> master::torrentjobs::JobView {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let v = core.torrent_job(id).expect("tâche connue");
        if v.state != "running" {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tâche toujours en cours : {v:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn write_node_file(n: &Node, rel: &str, data: &[u8]) {
    let p = n.root.path().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

#[tokio::test]
async fn a_torrent_is_created_from_node_files_downloaded_and_added_to_the_node() {
    let m = start_master(settings()).await;
    let mut n = add_node(&m, &["seed"]).await;
    n.start().await;
    let video = xs_data(300_000);
    write_node_file(&n, "movies/Film/video.mkv", &video);
    write_node_file(&n, "movies/Film/Subs/fr.srt", b"sous-titres");

    // Exploration : dossiers d'abord, sous la racine du nœud.
    let root = m.core.browse_node(&n.id, "").await.unwrap();
    assert!(root.entries.iter().any(|e| e.name == "movies" && e.dir));
    let film = m.core.browse_node(&n.id, "movies/Film").await.unwrap();
    let names: Vec<(&str, bool)> = film
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.dir))
        .collect();
    assert_eq!(names, [("Subs", true), ("video.mkv", false)]);
    assert_eq!(film.entries[1].size, 300_000);

    // Création : trackers par paliers dont un avec un passkey, torrent privé, ajout au nœud.
    let secret = "https://tracker.test/announce?passkey=SECRET-PASSKEY";
    let id = m
        .core
        .start_torrent_job(
            "admin:alice",
            CreateRequest {
                node_id: n.id.clone(),
                path: "movies/Film".into(),
                tiers: vec![
                    vec![secret.into(), "udp://t2.test:6969/announce".into()],
                    vec!["http://secours.test/a".into()],
                ],
                comment: "mon commentaire".into(),
                private: true,
                source: "MonTracker".into(),
                piece_length: 65536,
                add_to_node: true,
            },
        )
        .await
        .unwrap();
    let v = job_done(&m.core, &id).await;
    assert_eq!(
        (v.state, v.name.as_str(), v.size),
        ("done", "Film", 300_011),
        "{v:?}"
    );
    assert!(
        v.private && v.magnet.starts_with("magnet:?xt=urn:btih:") && v.added == Some(true),
        "{v:?}"
    );

    // Le fichier : trackers et métadonnées assemblés par le master, infohash identique à celui d'une création locale.
    let (fname, bytes) = m.core.torrent_job_file(&id).unwrap();
    assert_eq!(fname, "Film.torrent");
    let meta = proto::metainfo::parse(&bytes).unwrap();
    assert_eq!(
        meta.trackers,
        vec![
            secret,
            "udp://t2.test:6969/announce",
            "http://secours.test/a"
        ]
    );
    assert!(meta.private && meta.file_count == 2);
    let local = proto::create::create(
        &n.root.path().join("movies/Film"),
        &proto::create::CreateOptions {
            piece_length: Some(65536),
            private: true,
            source: Some("MonTracker".into()),
            ..Default::default()
        },
        &std::sync::atomic::AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(
        meta.infohash,
        proto::metainfo::parse(&local).unwrap().infohash,
        "même contenu, mêmes options : même infohash"
    );
    assert_eq!(v.infohash, meta.infohash);

    // Ajouté au nœud (il a les fichiers : il vérifiera puis partagera), dans le dossier PARENT du contenu.
    eventually("le client reçoit le torrent", || {
        n.qbit.get(&meta.infohash).is_some()
    })
    .await;
    assert!(
        n.qbit
            .get(&meta.infohash)
            .unwrap()
            .save_path
            .ends_with("movies"),
        "{}",
        n.qbit.get(&meta.infohash).unwrap().save_path
    );
    assert!(n
        .qbit
        .added_files()
        .iter()
        .any(|(h, b)| *h == meta.infohash && *b == bytes));
    let def = m.core.store.get_torrent(&meta.infohash).unwrap().unwrap();
    assert!(def.has_file && def.save_subdir == "movies");

    // Journal : l'action est attribuée à l'admin ; AUCUNE adresse de tracker (passkey) ni chemin sensible.
    let audit = m.core.store.audit_list(100, None, None).unwrap();
    assert!(audit
        .iter()
        .any(|r| r.action == "torrent_create_started" && r.actor == "admin:alice"));
    assert!(audit.iter().any(|r| r.action == "torrent_created"));
    let all: String = audit
        .iter()
        .map(|r| format!("{} {}", r.actor, r.detail))
        .collect();
    assert!(
        !all.contains("SECRET-PASSKEY") && !all.contains("tracker.test"),
        "pas de passkey dans le journal"
    );
    n.stop().await;
}

#[tokio::test]
async fn bad_requests_offline_nodes_and_unknown_jobs_are_refused_cleanly() {
    let m = start_master(settings()).await;
    let mut n = add_node(&m, &["seed"]).await;
    n.start().await;
    write_node_file(&n, "a/f.bin", b"contenu");
    let nid = n.id.clone();
    let req = |path: &str| CreateRequest {
        node_id: nid.clone(),
        path: path.into(),
        ..CreateRequest::default()
    };
    // Validation AVANT de déranger le nœud.
    for bad in ["", ".", "..", "../x", "/etc/passwd", "C:\\x"] {
        assert!(
            matches!(
                m.core.start_torrent_job("a", req(bad)).await,
                Err(JobError::Invalid(_))
            ),
            "{bad:?}"
        );
        assert!(
            m.core.browse_node(&n.id, bad).await.is_err() || bad.is_empty() || bad == ".",
            "{bad:?}"
        );
    }
    // Chemin valide mais inexistant : le nœud répond par une erreur lisible, la tâche échoue proprement.
    let id = m
        .core
        .start_torrent_job("a", req("a/absent"))
        .await
        .unwrap();
    let v = job_done(&m.core, &id).await;
    assert_eq!(v.state, "failed");
    assert!(
        !v.error.is_empty() && !v.error.contains(n.root.path().to_str().unwrap()),
        "pas de chemin local : {}",
        v.error
    );
    assert!(m.core.torrent_job_file(&id).is_none());
    // Racine entière refusée par le nœud lui-même (ceinture et bretelles).
    assert!(
        m.core.browse_node(&n.id, "a/f.bin").await.is_err(),
        "ce n'est pas un dossier"
    );
    // Un nœud hors ligne : refus net.
    n.stop().await;
    eventually("tunnel fermé", || !m.core.sessions.is_connected(&n.id)).await;
    assert!(matches!(
        m.core.start_torrent_job("a", req("a/f.bin")).await,
        Err(JobError::Offline)
    ));
    assert!(matches!(
        m.core.browse_node(&n.id, "a").await,
        Err(JobError::Offline)
    ));
    // Tâches inconnues.
    assert!(m.core.torrent_job("nope").is_none());
    assert!(matches!(
        m.core.cancel_torrent_job("nope").await,
        Err(JobError::Unknown)
    ));
    assert!(matches!(
        m.core.forget_torrent_job("nope"),
        Err(JobError::Unknown)
    ));
    // Oublier une tâche terminée libère la mémoire.
    m.core.forget_torrent_job(&id).unwrap();
    assert!(m.core.torrent_job(&id).is_none());
}

#[tokio::test]
async fn a_cancelled_job_never_adds_anything_and_a_third_concurrent_job_is_refused() {
    let m = start_master(settings()).await;
    let mut n = add_node(&m, &["seed"]).await;
    n.start().await;
    // Assez gros pour que la création dure un instant (hachage de 64 Mio).
    for d in ["x", "y", "z"] {
        write_node_file(&n, &format!("big/{d}.bin"), &vec![5u8; 64 * 1024 * 1024]);
    }
    let req = |p: &str| CreateRequest {
        node_id: n.id.clone(),
        path: p.into(),
        add_to_node: true,
        ..CreateRequest::default()
    };
    let a = m
        .core
        .start_torrent_job("a", req("big/x.bin"))
        .await
        .unwrap();
    let b = m
        .core
        .start_torrent_job("a", req("big/y.bin"))
        .await
        .unwrap();
    // Deux créations en cours sur ce nœud : la troisième attend.
    assert!(matches!(
        m.core.start_torrent_job("a", req("big/z.bin")).await,
        Err(JobError::Busy(_))
    ));
    // Annulation immédiate : l'état est « annulé » et le résultat tardif de l'agent est ignoré.
    m.core.cancel_torrent_job(&a).await.unwrap();
    assert_eq!(m.core.torrent_job(&a).unwrap().state, "cancelled");
    assert!(
        matches!(m.core.forget_torrent_job(&b), Err(JobError::Busy(_))),
        "tâche en cours : à annuler d'abord"
    );
    let vb = job_done(&m.core, &b).await;
    assert_eq!(vb.state, "done");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let va = m.core.torrent_job(&a).unwrap();
    assert_eq!(va.state, "cancelled");
    assert!(
        va.infohash.is_empty() && m.core.torrent_job_file(&a).is_none(),
        "rien n'est conservé d'une tâche annulée"
    );
    assert!(
        m.core.store.get_torrent(&vb.infohash).unwrap().is_some(),
        "la tâche non annulée a bien ajouté son torrent"
    );
    n.stop().await;
}
