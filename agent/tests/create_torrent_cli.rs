//! `agent create-torrent` : le VRAI binaire crée un .torrent lisible, refuse ce qui est dangereux, protège la sortie.
use std::process::Command;

fn agent() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agent"))
}

fn sample_dir() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("Film.2024");
    std::fs::create_dir_all(root.join("Subs")).unwrap();
    std::fs::write(root.join("video.mkv"), vec![7u8; 100_000]).unwrap();
    std::fs::write(
        root.join("Subs/fr.srt"),
        b"1\n00:00:01,000 --> 00:00:02,000\nSalut\n",
    )
    .unwrap();
    t
}

#[test]
fn creates_a_readable_private_torrent_with_trackers_and_a_restricted_file() {
    let t = sample_dir();
    let out = t.path().join("film.torrent");
    let r = agent()
        .args(["create-torrent"])
        .arg(t.path().join("Film.2024"))
        .arg("-o")
        .arg(&out)
        .args([
            "--tracker",
            "https://tracker.test/announce?passkey=SECRET",
            "--tracker",
            "udp://t2.test:6969/announce",
        ])
        .args(["--private", "--source", "MonTracker", "--comment", "essai"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let m = proto::metainfo::parse(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(
        (m.name.as_str(), m.file_count, m.size, m.private),
        ("Film.2024", 2, 100_000 + 38, true)
    );
    assert_eq!(
        m.trackers,
        vec![
            "https://tracker.test/announce?passkey=SECRET",
            "udp://t2.test:6969/announce"
        ]
    );
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(
        stdout.contains(&m.infohash) && stdout.contains("magnet:?xt=urn:btih:"),
        "{stdout}"
    );
    // L'adresse d'un tracker privé contient un passkey : le fichier n'est lisible que par son propriétaire.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Pas d'écrasement sans --force.
    let again = agent()
        .arg("create-torrent")
        .arg(t.path().join("Film.2024"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("existe déjà"));
    let forced = agent()
        .arg("create-torrent")
        .arg(t.path().join("Film.2024"))
        .arg("-o")
        .arg(&out)
        .arg("--force")
        .output()
        .unwrap();
    assert!(forced.status.success());
}

#[test]
fn refuses_unsafe_or_incoherent_requests_before_reading_anything() {
    let t = sample_dir();
    let src = t.path().join("Film.2024");
    let out = t.path().join("x.torrent");
    let run = |extra: &[&str]| {
        agent()
            .arg("create-torrent")
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .args(extra)
            .output()
            .unwrap()
    };
    for (extra, why) in [
        (vec!["--private"], "privé sans tracker"),
        (vec!["--tracker", "ftp://x.test/a"], "schéma non autorisé"),
        (vec!["--tracker", "http://a b"], "espace dans l'adresse"),
        (vec!["--piece-length", "1000"], "taille de pièce invalide"),
        (vec!["--name", "../x"], "nom invalide"),
    ] {
        let r = run(&extra);
        assert!(!r.status.success(), "{why} : devrait échouer");
        assert!(!out.exists(), "{why} : aucun fichier ne doit être écrit");
    }
    let missing = agent()
        .arg("create-torrent")
        .arg(t.path().join("absent"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!missing.status.success() && !out.exists());
}
