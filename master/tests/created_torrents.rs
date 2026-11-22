//! Manuel (`--ignored`) : les .torrent CRÉÉS par nxtTorrent (`proto::create` + `metainfo::compose`) sont acceptés par de VRAIS
//! clients (qBittorrent, Transmission) : ils retrouvent les fichiers, les vérifient à 100 % et partagent.
use agent::client::{AddRequest, AddSource, TorrentClient};
use agent::qbit::QbitClient;
use agent::transmission::TransmissionClient;
use proto::pb::TorrentStatus;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn docker(args: &[&str]) -> String {
    let o = Command::new("docker").args(args).output().expect("docker");
    assert!(
        o.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn rm(name: &str) {
    let _ = Command::new("docker").args(["rm", "-f", name]).output();
}

fn data(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn make(path: &Path, opts: &proto::create::CreateOptions, tracker: &str) -> Vec<u8> {
    let base = proto::create::create(path, opts, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    proto::metainfo::compose(
        &base,
        &[vec![tracker.to_owned()]],
        Some("test"),
        "nxtTorrent test",
        Some(1_700_000_000),
    )
    .unwrap()
}

async fn until<C: TorrentClient>(
    c: &C,
    h: &str,
    what: &str,
    ok: impl Fn(&proto::pb::TorrentInfo) -> bool,
) -> proto::pb::TorrentInfo {
    let end = Instant::now() + Duration::from_secs(90);
    loop {
        let list = c.list().await.unwrap_or_default();
        let t = list.iter().find(|t| t.infohash == h);
        if let Some(t) = t.filter(|t| ok(t)) {
            return t.clone();
        }
        assert!(
            Instant::now() < end,
            "délai dépassé : {what} ; état {:?}",
            t.map(|t| (t.status, t.progress, t.message.clone()))
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn scenario<C: TorrentClient>(c: &C, container: &str) {
    let host = tempfile::tempdir().unwrap();
    // Dossier multi-fichiers (sous-dossier, fichier vide, plusieurs pièces) et fichier seul.
    let film = host.path().join("Film.2024");
    std::fs::create_dir_all(film.join("Subs")).unwrap();
    std::fs::write(film.join("video.mkv"), data(11, 2 * 1024 * 1024 + 123)).unwrap();
    std::fs::write(film.join("Subs/fr.srt"), data(12, 4096)).unwrap();
    std::fs::write(film.join("vide.txt"), b"").unwrap();
    let seul = host.path().join("album.flac");
    std::fs::write(&seul, data(13, 700_000)).unwrap();

    let opts = proto::create::CreateOptions {
        piece_length: Some(65536),
        private: true,
        source: Some("MonTracker".into()),
        ..Default::default()
    };
    let t_film = make(
        &film,
        &opts,
        "http://tracker.invalid/announce?passkey=secret",
    );
    let t_seul = make(
        &seul,
        &proto::create::CreateOptions::default(),
        "udp://tracker.invalid:6969/announce",
    );
    let (m_film, m_seul) = (
        proto::metainfo::parse(&t_film).unwrap(),
        proto::metainfo::parse(&t_seul).unwrap(),
    );

    docker(&["exec", container, "mkdir", "-p", "/downloads/movies"]);
    for p in [&film, &seul] {
        let dest = format!(
            "{container}:/downloads/movies/{}",
            p.file_name().unwrap().to_str().unwrap()
        );
        docker(&["cp", p.to_str().unwrap(), &dest]);
    }
    docker(&[
        "exec",
        container,
        "chown",
        "-R",
        "1000:1000",
        "/downloads/movies",
    ]);

    for (bytes, hash, what) in [
        (&t_film, &m_film.infohash, "dossier"),
        (&t_seul, &m_seul.infohash, "fichier seul"),
    ] {
        c.add(AddRequest {
            source: AddSource::File(bytes.clone()),
            save_path: "/downloads/movies".into(),
            paused: false,
        })
        .await
        .unwrap_or_else(|e| panic!("{what} : le client refuse le torrent créé : {e}"));
        let t = until(c, hash, &format!("{what} : vérifié à 100 %"), |t| {
            t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32
        })
        .await;
        assert_eq!(t.dl_rate, 0, "{what} : aucun téléchargement");
        println!("{what} : {t:?}");
    }
    // Le client voit le bon nom et la bonne taille (ce que nous avons écrit dans le torrent).
    let list = c.list().await.unwrap();
    let f = list.iter().find(|t| t.infohash == m_film.infohash).unwrap();
    assert_eq!((f.name.as_str(), f.size_bytes), ("Film.2024", m_film.size));
}

#[tokio::test]
#[ignore = "manuel : exige Docker et l'image linuxserver/qbittorrent"]
async fn qbittorrent_accepts_and_verifies_torrents_created_by_nxttorrent() {
    let _ = Command::new("docker")
        .args(["network", "create", "nxt-ct-net"])
        .output();
    rm("nxt-ct-q");
    docker(&[
        "run",
        "-d",
        "--name",
        "nxt-ct-q",
        "--network",
        "nxt-ct-net",
        "-e",
        "PUID=1000",
        "-e",
        "PGID=1000",
        "-e",
        "WEBUI_PORT=19501",
        "-p",
        "19501:19501",
        "lscr.io/linuxserver/qbittorrent:latest",
    ]);
    let end = Instant::now() + Duration::from_secs(90);
    let pw = loop {
        let l = Command::new("docker")
            .args(["logs", "nxt-ct-q"])
            .output()
            .unwrap();
        let t = format!(
            "{}{}",
            String::from_utf8_lossy(&l.stdout),
            String::from_utf8_lossy(&l.stderr)
        );
        if let Some(p) = t
            .lines()
            .find_map(|l| l.split("provided for this session: ").nth(1))
        {
            break p.trim().to_owned();
        }
        assert!(Instant::now() < end, "qBittorrent n'a pas démarré");
        std::thread::sleep(Duration::from_secs(2));
    };
    let c = QbitClient::new("http://127.0.0.1:19501", "admin", &pw, "/downloads".into()).unwrap();
    scenario(&c, "nxt-ct-q").await;
    rm("nxt-ct-q");
    let _ = Command::new("docker")
        .args(["network", "rm", "nxt-ct-net"])
        .output();
}

#[tokio::test]
#[ignore = "manuel : exige Docker et l'image linuxserver/transmission"]
async fn transmission_accepts_and_verifies_torrents_created_by_nxttorrent() {
    let _ = Command::new("docker")
        .args(["network", "create", "nxt-ct-net"])
        .output();
    rm("nxt-ct-t");
    docker(&[
        "run",
        "-d",
        "--name",
        "nxt-ct-t",
        "--network",
        "nxt-ct-net",
        "-e",
        "PUID=1000",
        "-e",
        "PGID=1000",
        "-p",
        "19502:9091",
        "lscr.io/linuxserver/transmission:latest",
    ]);
    std::thread::sleep(Duration::from_secs(10));
    let c = TransmissionClient::new("http://127.0.0.1:19502", "", "", "/downloads".into()).unwrap();
    scenario(&c, "nxt-ct-t").await;
    rm("nxt-ct-t");
    let _ = Command::new("docker")
        .args(["network", "rm", "nxt-ct-net"])
        .output();
}
