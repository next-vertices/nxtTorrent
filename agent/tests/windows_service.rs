//! Cycle de vie RÉEL du service Windows de l'agent, de bout en bout : appairage, installation, tunnel
//! tenu par le service (mot de passe du client lu dans le fichier protégé), commande jusqu'au client,
//! plantage puis redémarrage automatique, arrêt propre, suppression.
//! Nécessite un terminal Administrateur (la CI Windows en est un) :
//!   cargo test -p agent --test windows_service -- --ignored --nocapture
#![cfg(windows)]
use agent::pair::{pair, PairOptions};
use master::{ca::Ca, service, store::Store, Core, PAIRING_CODE_TTL};
use proto::pb::command::Kind;
use proto::pb::{add_cmd::Source, AddCmd, Command};
use std::path::Path;
use std::process::{Command as Proc, Output};
use std::sync::Arc;
use std::time::Duration;
use testkit::qbit::MockQbit;

const AGENT: &str = env!("CARGO_BIN_EXE_agent");
const HASH: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";

fn svc(data: &Path, name: &str, extra_env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Proc::new(AGENT);
    c.arg("--data-dir")
        .arg(data)
        .args(["service", "--service-name", name])
        .args(args);
    for (k, v) in extra_env {
        c.env(k, v);
    }
    c.output().expect("lancement de agent.exe")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn pid_of(status: &str) -> Option<u32> {
    status
        .split("(pid ")
        .nth(1)?
        .split(')')
        .next()?
        .trim()
        .parse()
        .ok()
}

async fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(tokio::time::Instant::now() < end, "délai dépassé : {what}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Supprime le service même si le test échoue en cours de route.
struct Cleanup<'a>(&'a Path, &'a str);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = svc(self.0, self.1, &[], &["uninstall"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nécessite un terminal Administrateur"]
async fn agent_service_lifecycle_and_crash_recovery() {
    // --- master en processus, avec un faux qBittorrent (admin / adminadmin) ---
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
    let qbit = MockQbit::start().await;

    // --- appairage de l'agent (identité dans un dossier lisible par le compte SYSTEM) ---
    let name = format!("nxt-agent-test-{}", std::process::id());
    let data = std::env::temp_dir().join(format!("nxt-agent-svc-{}", std::process::id()));
    let root = std::env::temp_dir().join(format!("nxt-agent-root-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let code = core.create_pairing_code(PAIRING_CODE_TTL).unwrap();
    let node_id = pair(PairOptions {
        addr: addr.clone(),
        code,
        data_dir: data.clone(),
        hostname: "svc-test".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;
    let _cleanup = Cleanup(&data, &name);
    let connected = |core: &Arc<Core>| core.sessions.is_connected(&node_id);

    // --- installation : le mot de passe du client passe par l'environnement de l'installateur ---
    let url = qbit.url();
    let o = svc(
        &data,
        &name,
        &[("AGENT_CLIENT_PASSWORD", "adminadmin")],
        &[
            "install",
            "--root",
            root.to_str().unwrap(),
            "--client",
            "qbittorrent",
            "--client-url",
            &url,
        ],
    );
    assert!(o.status.success(), "install: {}", text(&o));
    // Le mot de passe est dans un fichier protégé, jamais dans la configuration du service.
    let acl = Proc::new("icacls")
        .arg(data.join("client_password"))
        .output()
        .unwrap();
    let acl = String::from_utf8_lossy(&acl.stdout).to_lowercase();
    assert!(
        !acl.contains("everyone") && !acl.contains("builtin\\users"),
        "ACL trop large : {acl}"
    );
    // Un agent non appairé est refusé à l'installation.
    let empty = std::env::temp_dir().join(format!("nxt-agent-unpaired-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let o = svc(
        &empty,
        "nxt-agent-unpaired",
        &[],
        &["install", "--root", root.to_str().unwrap()],
    );
    assert!(
        !o.status.success() && text(&o).contains("appair"),
        "{}",
        text(&o)
    );
    let _ = std::fs::remove_dir_all(&empty);

    // --- démarrage : le SERVICE tient le tunnel et pilote le client avec le mot de passe du fichier ---
    assert!(svc(&data, &name, &[], &["start"]).status.success());
    wait("service Running", 30, || {
        text(&svc(&data, &name, &[], &["status"])).contains("Running")
    })
    .await;
    wait("tunnel établi par le service", 30, || connected(&core)).await;
    wait("auth du client via le fichier de mot de passe", 15, || {
        core.telemetry(&node_id).client_version == "v4.6.0"
    })
    .await;
    let add = Command {
        cmd_id: "svc-add".into(),
        kind: Some(Kind::Add(AddCmd {
            source: Some(Source::Infohash(HASH.into())),
            save_subdir: "films".into(),
            start_paused: false,
        })),
    };
    assert!(core.send_command(&node_id, add).await);
    wait("commande exécutée jusqu'au client", 15, || {
        qbit.get(HASH).is_some()
    })
    .await;
    let log = std::fs::read_to_string(data.join("agent.log")).unwrap_or_default();
    assert!(log.contains("service démarré"), "journal : {log}");

    // --- plantage brutal : le SCM relance le service, le tunnel se rétablit seul ---
    let first = pid_of(&text(&svc(&data, &name, &[], &["status"]))).expect("PID");
    let before = core.telemetry(&node_id).full_states;
    let k = Proc::new("taskkill")
        .args(["/F", "/PID", &first.to_string()])
        .output()
        .unwrap();
    assert!(
        k.status.success(),
        "taskkill: {}",
        String::from_utf8_lossy(&k.stdout)
    );
    wait("tunnel coupé par le plantage", 20, || !connected(&core)).await;
    wait("redémarrage automatique du service", 60, || {
        let s = text(&svc(&data, &name, &[], &["status"]));
        s.contains("Running") && pid_of(&s).is_some_and(|p| p != first)
    })
    .await;
    wait("tunnel rétabli après redémarrage", 30, || {
        connected(&core)
    })
    .await;
    wait("nouvel état complet reçu", 15, || {
        core.telemetry(&node_id).full_states > before
    })
    .await;
    assert!(
        qbit.get(HASH).is_some(),
        "le client torrent n'est pas affecté par le plantage de l'agent"
    );

    // --- arrêt volontaire : propre, et PAS relancé ---
    assert!(svc(&data, &name, &[], &["stop"]).status.success());
    wait("tunnel fermé", 15, || !connected(&core)).await;
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(
        text(&svc(&data, &name, &[], &["status"])).contains("Stopped"),
        "un arrêt volontaire ne doit pas être relancé"
    );
    assert!(!connected(&core));
    let log = std::fs::read_to_string(data.join("agent.log")).unwrap_or_default();
    assert!(
        log.contains("service arrêté"),
        "arrêt propre attendu : {log}"
    );

    // --- suppression ---
    assert!(svc(&data, &name, &[], &["uninstall"]).status.success());
    assert!(
        !svc(&data, &name, &[], &["status"]).status.success(),
        "le service ne doit plus exister"
    );
    let _ = stop_tx.send(());
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&root);
}
