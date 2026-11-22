//! Arrêt propre sur SIGTERM (signal envoyé par systemd, Docker, `kill`) : le processus doit se
//! terminer avec le code 0, pas être tué. Sans cela `systemctl stop` laisserait la base SQLite et le
//! tunnel dans un état non fermé et marquerait le service « failed ».
#![cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn master_exits_cleanly_on_sigterm() {
    let data = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_master"))
        .arg("--data-dir")
        .arg(data.path())
        .args(["serve", "--listen", &listen, "--san", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Attend que le listener soit prêt : le signal doit arriver en régime établi.
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::net::TcpStream::connect_timeout(&listen.parse().unwrap(), Duration::from_millis(200))
        .is_err()
    {
        assert!(Instant::now() < deadline, "le master ne démarre pas");
        assert!(
            child.try_wait().unwrap().is_none(),
            "le master s'est arrêté prématurément"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let k = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(k.success());

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("le master n'a pas quitté dans les 10 s suivant SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "sortie propre attendue (code 0), obtenu : {status:?}"
    );
}
