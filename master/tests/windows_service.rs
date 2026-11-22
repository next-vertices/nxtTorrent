//! Cycle de vie RÉEL du service Windows : installation, démarrage, service effectif, arrêt, suppression.
//! Nécessite un terminal Administrateur (la CI Windows en est un) :
//!   cargo test -p master --test windows_service -- --ignored --nocapture
#![cfg(windows)]
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const MASTER: &str = env!("CARGO_BIN_EXE_master");

fn svc(data: &Path, name: &str, args: &[&str]) -> Output {
    Command::new(MASTER)
        .arg("--data-dir")
        .arg(data)
        .args(["service", "--service-name", name])
        .args(args)
        .output()
        .expect("lancement de master.exe")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < end, "délai dépassé : {what}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Supprime le service même si le test échoue en cours de route.
struct Cleanup<'a>(&'a Path, &'a str);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = svc(self.0, self.1, &["uninstall"]);
    }
}

#[test]
#[ignore = "nécessite un terminal Administrateur"]
fn service_install_start_serve_stop_uninstall() {
    let name = format!("nxt-test-{}", std::process::id());
    let data = std::env::temp_dir().join(format!("nxt-svc-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // Port libre pour le listener du service.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    let _cleanup = Cleanup(&data, &name);

    let o = svc(
        &data,
        &name,
        &["install", "--san", "127.0.0.1", "--listen", &listen],
    );
    assert!(o.status.success(), "install: {}", text(&o));
    assert!(text(&svc(&data, &name, &["status"])).contains("Stopped"));

    let o = svc(&data, &name, &["start"]);
    assert!(o.status.success(), "start: {}", text(&o));
    wait("service Running", 30, || {
        text(&svc(&data, &name, &["status"])).contains("Running")
    });

    // Le service fait réellement son travail : le listener mTLS répond.
    wait("listener joignable", 15, || {
        std::net::TcpStream::connect_timeout(&listen.parse().unwrap(), Duration::from_millis(500))
            .is_ok()
    });
    let log = std::fs::read_to_string(data.join("master.log")).unwrap_or_default();
    assert!(
        log.contains("service démarré"),
        "journal du service : {log}"
    );

    // Arrêt propre demandé par le SCM, puis le port est libéré.
    let o = svc(&data, &name, &["stop"]);
    assert!(o.status.success(), "stop: {}", text(&o));
    assert!(text(&svc(&data, &name, &["status"])).contains("Stopped"));
    wait("port libéré", 10, || {
        std::net::TcpStream::connect_timeout(&listen.parse().unwrap(), Duration::from_millis(300))
            .is_err()
    });
    let log = std::fs::read_to_string(data.join("master.log")).unwrap_or_default();
    assert!(
        log.contains("service arrêté"),
        "arrêt propre attendu dans le journal : {log}"
    );

    let o = svc(&data, &name, &["uninstall"]);
    assert!(o.status.success(), "uninstall: {}", text(&o));
    assert!(
        !svc(&data, &name, &["status"]).status.success(),
        "le service ne doit plus exister"
    );
    let _ = std::fs::remove_dir_all(&data);
}

/// Extrait le PID d'un état `Running (pid 1234)`.
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

/// Plantage : le processus est tué brutalement (sans passer par l'arrêt du SCM). Le gestionnaire doit
/// le relancer seul (action de reprise « Restart » après 5 s) : nouveau PID, listener de nouveau joignable.
#[test]
#[ignore = "nécessite un terminal Administrateur"]
fn service_restarts_automatically_after_a_crash() {
    let name = format!("nxt-crash-{}", std::process::id());
    let data = std::env::temp_dir().join(format!("nxt-crash-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    let _cleanup = Cleanup(&data, &name);
    let reachable = |ms| {
        std::net::TcpStream::connect_timeout(&listen.parse().unwrap(), Duration::from_millis(ms))
            .is_ok()
    };

    let o = svc(
        &data,
        &name,
        &["install", "--san", "127.0.0.1", "--listen", &listen],
    );
    assert!(o.status.success(), "install: {}", text(&o));
    assert!(svc(&data, &name, &["start"]).status.success());
    wait("service Running", 30, || {
        text(&svc(&data, &name, &["status"])).contains("Running")
    });
    wait("listener joignable", 15, || reachable(500));
    let first = pid_of(&text(&svc(&data, &name, &["status"]))).expect("PID du service");

    // Plantage simulé.
    let k = Command::new("taskkill")
        .args(["/F", "/PID", &first.to_string()])
        .output()
        .unwrap();
    assert!(
        k.status.success(),
        "taskkill: {}",
        String::from_utf8_lossy(&k.stdout)
    );
    wait("processus disparu", 10, || !reachable(300));

    // Reprise automatique par le SCM : nouveau processus, même service, listener de retour.
    wait("redémarrage automatique", 60, || {
        let s = text(&svc(&data, &name, &["status"]));
        s.contains("Running") && pid_of(&s).is_some_and(|p| p != first)
    });
    wait("listener de retour", 15, || reachable(500));

    // Un arrêt demandé, lui, ne déclenche PAS de redémarrage.
    assert!(svc(&data, &name, &["stop"]).status.success());
    std::thread::sleep(Duration::from_secs(8));
    assert!(
        text(&svc(&data, &name, &["status"])).contains("Stopped"),
        "un arrêt volontaire ne doit pas être relancé"
    );
    assert!(svc(&data, &name, &["uninstall"]).status.success());
    let _ = std::fs::remove_dir_all(&data);
}
