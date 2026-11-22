//! Manuel (`--ignored`) : la réplication par swarm privé avec deux VRAIS qBittorrent (réseau Docker isolé), pilotés
//! uniquement par le driver de l'agent (ajout par fichier, `add_tracker`, suppression sans fichiers, revérification).
use agent::client::{AddRequest, AddSource, TorrentClient};
use agent::qbit::QbitClient;
use std::process::Command;
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

fn start(name: &str, port: u16) -> QbitClient {
    let _ = Command::new("docker").args(["rm", "-f", name]).output();
    docker(&[
        "run",
        "-d",
        "--name",
        name,
        "--network",
        "nxt-proto-q",
        "--add-host",
        "host.docker.internal:host-gateway",
        "-e",
        "PUID=1000",
        "-e",
        "PGID=1000",
        // qBittorrent refuse un en-tête Host dont le port diffère de celui de son WebUI : même port des deux côtés.
        "-e",
        &format!("WEBUI_PORT={port}"),
        "-p",
        &format!("{port}:{port}"),
        "lscr.io/linuxserver/qbittorrent:latest",
    ]);
    // Mot de passe temporaire imprimé par qBittorrent au démarrage.
    let end = Instant::now() + Duration::from_secs(90);
    loop {
        let logs = Command::new("docker")
            .args(["logs", name])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        );
        if let Some(pw) = text
            .lines()
            .find_map(|l| l.split("provided for this session: ").nth(1))
        {
            return QbitClient::new(
                &format!("http://127.0.0.1:{port}"),
                "admin",
                pw.trim(),
                "/downloads".into(),
            )
            .unwrap();
        }
        assert!(Instant::now() < end, "qBittorrent n'a pas démarré : {text}");
        std::thread::sleep(Duration::from_secs(2));
    }
}

async fn wait<F: Fn(&proto::pb::TorrentInfo) -> bool>(
    c: &QbitClient,
    hash: &str,
    what: &str,
    secs: u64,
    ok: F,
) -> proto::pb::TorrentInfo {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let list = c.list().await.unwrap_or_default();
        let t = list.iter().find(|t| t.infohash == hash);
        if let Some(t) = t.filter(|t| ok(t)) {
            return t.clone();
        }
        assert!(
            Instant::now() < end,
            "délai dépassé : {what} ; état {:?}",
            t.map(|t| (t.status, t.progress, t.message.clone()))
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test]
#[ignore = "manuel : exige Docker et l'image linuxserver/qbittorrent"]
async fn two_real_qbittorrent_replicate_via_private_tracker_then_retarget() {
    use proto::pb::TorrentStatus;
    let _ = Command::new("docker")
        .args(["network", "create", "nxt-proto-q"])
        .output();
    let mut x = 0x9E3779B97F4A7C15u64;
    let data: Vec<u8> = (0..3 * 1024 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let original = testkit::torrent::for_data(
        "proto.bin",
        &data,
        65536,
        "http://tracker.invalid/announce",
        false,
    );
    let hash = proto::metainfo::parse(&original).unwrap().infohash;
    let ih: [u8; 20] = data_encoding::HEXLOWER
        .decode(hash.as_bytes())
        .unwrap()
        .try_into()
        .unwrap();

    let tracker = master::tracker::Tracker::new(&[5u8; 32]);
    let l = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let tport = l.local_addr().unwrap().port();
    tokio::spawn(master::tracker::serve(l, tracker.clone()));

    let (src, rep) = (start("nxt-q-src", 19301), start("nxt-q-rep", 19302));
    for (n, c) in [("src", "nxt-q-src"), ("rep", "nxt-q-rep")] {
        let ip = docker(&[
            "inspect",
            "-f",
            "{{(index .NetworkSettings.Networks \"nxt-proto-q\").IPAddress}}",
            c,
        ]);
        tracker.set_node_ip(n, ip.parse().unwrap());
        docker(&["exec", c, "mkdir", "-p", "/downloads/rep"]);
        docker(&["exec", c, "chown", "-R", "1000:1000", "/downloads"]);
    }
    let tmp = std::env::temp_dir().join("proto-q.bin");
    std::fs::write(&tmp, &data).unwrap();
    docker(&[
        "cp",
        tmp.to_str().unwrap(),
        "nxt-q-src:/downloads/rep/proto.bin",
    ]);
    docker(&[
        "exec",
        "nxt-q-src",
        "chown",
        "-R",
        "1000:1000",
        "/downloads/rep",
    ]);

    let url = |node: &str| {
        format!(
            "http://host.docker.internal:{tport}{}",
            tracker.announce_path(&ih, node)
        )
    };
    let req = |bytes: Vec<u8>, paused: bool| AddRequest {
        source: AddSource::File(bytes),
        save_path: "/downloads/rep".into(),
        paused,
    };

    // Source : torrent d'ORIGINE, puis annonce privée ajoutée (driver réel, deux fois : idempotent).
    src.add(req(original.clone(), false)).await.unwrap();
    wait(&src, &hash, "la source vérifie et partage", 90, |t| {
        t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32
    })
    .await;
    // Export du .torrent par le vrai client (cas d'un torrent ajouté par magnet) : même infohash.
    let exported = src.export_torrent(&hash).await.unwrap();
    assert_eq!(proto::metainfo::parse(&exported).unwrap().infohash, hash);
    src.add_tracker(&hash, &url("src")).await.unwrap();
    src.add_tracker(&hash, &url("src")).await.unwrap();

    // Copie : torrent RÉÉCRIT (tracker privé uniquement).
    let rewritten = proto::metainfo::rewrite_announce(&original, &url("rep")).unwrap();
    rep.add(req(rewritten, false)).await.unwrap();
    // Plafond de débit de la copie (par torrent) : accepté par le vrai client.
    rep.set_torrent_limits(&hash, 50_000_000, 0).await.unwrap();
    let t = wait(&rep, &hash, "la copie se télécharge", 240, |t| {
        t.progress >= 0.9999
    })
    .await;
    println!("copie terminée : {t:?}");
    let sum = |c: &str| docker(&["exec", c, "sha256sum", "/downloads/rep/proto.bin"]);
    assert_eq!(
        sum("nxt-q-src").split(' ').next(),
        sum("nxt-q-rep").split(' ').next(),
        "contenu identique"
    );
    assert!(tracker.swarm_size(&ih) >= 1);

    // Rebranchement : retrait SANS fichiers, original ré-ajouté en pause, revérification, partage.
    rep.remove(&hash, false).await.unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while rep.list().await.unwrap().iter().any(|t| t.infohash == hash) {
        assert!(Instant::now() < end, "le client n'a pas retiré le torrent");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(
        docker(&["exec", "nxt-q-rep", "ls", "/downloads/rep"]).contains("proto.bin"),
        "les fichiers sont conservés"
    );
    rep.add(req(original.clone(), true)).await.unwrap();
    rep.recheck(&hash).await.unwrap();
    wait(&rep, &hash, "revérification", 60, |t| {
        t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32
    })
    .await;
    rep.resume(&hash).await.unwrap();
    let t = wait(&rep, &hash, "partage sur l'original", 30, |t| {
        t.status == TorrentStatus::Seeding as i32
    })
    .await;
    println!("après rebranchement : {t:?}");

    for c in ["nxt-q-src", "nxt-q-rep"] {
        let _ = Command::new("docker").args(["rm", "-f", c]).output();
    }
    let _ = Command::new("docker")
        .args(["network", "rm", "nxt-proto-q"])
        .output();
}
