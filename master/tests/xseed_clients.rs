//! Manuel (`--ignored`) : l'hypothèse du cross-seed avec de VRAIS clients Docker, pilotés par les drivers de l'agent.
//! 1. Un torrent d'infohash DIFFÉRENT (autre tracker, drapeau privé) ajouté en pause sur des fichiers déjà présents est
//!    vérifié à 100 % sans rien télécharger, puis partage.
//! 2. Si le contenu diffère (un octet), la vérification reste < 100 % : c'est le cas que le moteur retire.
//! 3. Retirer un torrent sans les fichiers laisse les données (et l'autre torrent) intacts.
use agent::client::{AddRequest, AddSource, TorrentClient};
use agent::qbit::QbitClient;
use agent::transmission::TransmissionClient;
use proto::pb::TorrentStatus;
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

async fn until<C: TorrentClient>(
    c: &C,
    h: &str,
    what: &str,
    secs: u64,
    ok: impl Fn(&proto::pb::TorrentInfo) -> bool,
) -> proto::pb::TorrentInfo {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let list = c.list().await.unwrap_or_default();
        let t = list.iter().find(|t| t.infohash == h);
        if let Some(t) = t.filter(|t| ok(t)) {
            return t.clone();
        }
        assert!(
            Instant::now() < end,
            "délai dépassé : {what} ; état {:?}",
            t.map(|t| (t.status, t.progress))
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Scénario commun : `put` copie les données dans le conteneur ; `ls` liste le dossier.
async fn scenario<C: TorrentClient>(c: &C, container: &str) {
    let content = data(0xC0FFEE, 3 * 1024 * 1024);
    // Parent : tracker A, public. Candidat : tracker B, privé => `info` différent => infohash différent, même contenu.
    let parent = testkit::torrent::for_data(
        "film.bin",
        &content,
        65536,
        "http://tracker-a.invalid/ann",
        false,
    );
    let cand = testkit::torrent::for_data(
        "film.bin",
        &content,
        65536,
        "http://tracker-b.invalid/ann",
        true,
    );
    // Même nom et même taille, mais UN octet différent : autre infohash, contenu qui ne correspond plus.
    let mut bad = content.clone();
    bad[1_500_000] ^= 0xFF;
    let fake = testkit::torrent::for_data(
        "film.bin",
        &bad,
        65536,
        "http://tracker-c.invalid/ann",
        true,
    );
    let (mp, mc, mf) = (
        proto::metainfo::parse(&parent).unwrap(),
        proto::metainfo::parse(&cand).unwrap(),
        proto::metainfo::parse(&fake).unwrap(),
    );
    assert_ne!(
        mp.infohash, mc.infohash,
        "le candidat est bien un AUTRE torrent"
    );
    assert_eq!(mp.content_key(), mc.content_key(), "...du même contenu");
    assert_eq!(
        mp.content_key(),
        mf.content_key(),
        "(le faux a même nom et même taille)"
    );

    docker(&["exec", container, "mkdir", "-p", "/downloads/movies"]);
    let tmp = std::env::temp_dir().join("xseed-film.bin");
    std::fs::write(&tmp, &content).unwrap();
    docker(&[
        "cp",
        tmp.to_str().unwrap(),
        &format!("{container}:/downloads/movies/film.bin"),
    ]);
    docker(&["exec", container, "chown", "-R", "1000:1000", "/downloads"]);
    let req = |b: &[u8], paused: bool| AddRequest {
        source: AddSource::File(b.to_vec()),
        save_path: "/downloads/movies".into(),
        paused,
    };

    // Parent : ajouté, données trouvées, partage.
    c.add(req(&parent, false)).await.unwrap();
    until(c, &mp.infohash, "le parent est complet", 90, |t| {
        t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32
    })
    .await;

    // 1. Candidat identique : en pause, revérification -> 100 % sans télécharger, puis partage.
    c.add(req(&cand, true)).await.unwrap();
    c.recheck(&mc.infohash).await.unwrap();
    let t = until(
        c,
        &mc.infohash,
        "le candidat est vérifié à 100 %",
        90,
        |t| t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32,
    )
    .await;
    assert_eq!(t.dl_rate, 0, "aucun téléchargement");
    c.resume(&mc.infohash).await.unwrap();
    until(c, &mc.infohash, "le candidat partage", 30, |t| {
        t.status == TorrentStatus::Seeding as i32
    })
    .await;
    println!("candidat identique : {t:?}");

    // 2. Candidat au contenu différent, sur son PROPRE arbre de liens (ce que fait le cross-seed quand les données ne sont
    //    pas prouvées identiques) : la vérification reste < 100 %. Constaté en test réel : un client (Transmission)
    //    qui juge un torrent partiel RENOMME ses fichiers en « .part » ; posé directement sur les fichiers de l'original,
    //    il aurait cassé le torrent d'origine. Sur des liens, seul son propre nom bouge.
    docker(&[
        "exec", "-u", "1000", container, "sh", "-c",
        "mkdir -p /downloads/movies/.nxt-xseed/faux && ln /downloads/movies/film.bin /downloads/movies/.nxt-xseed/faux/film.bin",
    ]);
    let isolated = AddRequest {
        source: AddSource::File(fake.clone()),
        save_path: "/downloads/movies/.nxt-xseed/faux".into(),
        paused: true,
    };
    c.add(isolated).await.unwrap();
    c.recheck(&mf.infohash).await.unwrap();
    let f = until(
        c,
        &mf.infohash,
        "la vérification du faux se termine",
        90,
        |t| t.status != TorrentStatus::Checking as i32 && t.progress > 0.5,
    )
    .await;
    assert!(
        f.progress < 0.9999,
        "contenu différent : doit rester incomplet, vu {}",
        f.progress
    );
    println!("candidat au contenu différent : {f:?}");

    // 3. Retrait du faux AVEC ses fichiers (ses liens) : l'original, son torrent et le candidat identique restent intacts.
    c.remove(&mf.infohash, true).await.unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while c
        .list()
        .await
        .unwrap()
        .iter()
        .any(|t| t.infohash == mf.infohash)
    {
        assert!(Instant::now() < end, "retrait non effectué");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let sum = docker(&["exec", container, "sha256sum", "/downloads/movies/film.bin"]);
    use sha2::{Digest, Sha256};
    assert_eq!(
        sum.split(' ').next().unwrap(),
        data_encoding::HEXLOWER.encode(&Sha256::digest(&content)),
        "le fichier d'origine est intact (jamais renommé en .part)"
    );
    let list = c.list().await.unwrap();
    let parent_state = list
        .iter()
        .find(|t| t.infohash == mp.infohash)
        .expect("l'original est toujours là");
    assert!(
        parent_state.progress >= 0.9999,
        "le torrent d'origine reste complet : {parent_state:?}"
    );
    assert!(list.iter().any(|t| t.infohash == mc.infohash));
}

/// Disposition à LIENS PHYSIQUES : un autre dossier racine dont les fichiers sont des hardlinks de l'original (ce que
/// l'agent crée ; ici `ln` dans le conteneur en tient lieu, le code de l'agent étant testé à part). Le client doit
/// vérifier le candidat à 100 % sans rien télécharger, et supprimer le candidat AVEC ses fichiers ne doit pas toucher à
/// l'original (ce sont seulement des noms en moins).
async fn linked_scenario<C: TorrentClient>(c: &C, container: &str) {
    let video = data(0xBEEF, 2 * 1024 * 1024);
    let subs = data(0xCAFE, 4096);
    let files: [(&str, &[u8]); 2] = [("video.mkv", &video), ("Subs/fr.srt", &subs)];
    let parent = testkit::torrent::for_files(
        "Film.2024",
        &files,
        65536,
        "http://tracker-a.invalid/ann",
        false,
    );
    let cand = testkit::torrent::for_files(
        "Film.2024-GRP",
        &files,
        65536,
        "http://tracker-b.invalid/ann",
        true,
    );
    let (mp, mc) = (
        proto::metainfo::parse(&parent).unwrap(),
        proto::metainfo::parse(&cand).unwrap(),
    );
    assert_ne!(mp.infohash, mc.infohash);
    assert_ne!(mp.name, mc.name);
    assert!(
        proto::metainfo::plan_links(&mp, &mc).is_some() && proto::metainfo::needs_links(&mp, &mc)
    );

    docker(&[
        "exec",
        container,
        "mkdir",
        "-p",
        "/downloads/movies/Film.2024/Subs",
    ]);
    for (rel, d) in files {
        let tmp = std::env::temp_dir().join(format!("xseed-link-{}", rel.replace('/', "_")));
        std::fs::write(&tmp, d).unwrap();
        docker(&[
            "cp",
            tmp.to_str().unwrap(),
            &format!("{container}:/downloads/movies/Film.2024/{rel}"),
        ]);
    }
    docker(&[
        "exec",
        container,
        "chown",
        "-R",
        "1000:1000",
        "/downloads/movies",
    ]);
    let req = |b: &[u8], paused: bool| AddRequest {
        source: AddSource::File(b.to_vec()),
        save_path: "/downloads/movies".into(),
        paused,
    };
    c.add(req(&parent, false)).await.unwrap();
    until(c, &mp.infohash, "le parent est complet", 90, |t| {
        t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32
    })
    .await;

    // Les liens (rôle de l'agent), sous l'utilisateur du client.
    docker(&[
        "exec", "-u", "1000", container, "sh", "-c",
        "mkdir -p /downloads/movies/Film.2024-GRP/Subs && ln /downloads/movies/Film.2024/video.mkv /downloads/movies/Film.2024-GRP/video.mkv && ln /downloads/movies/Film.2024/Subs/fr.srt /downloads/movies/Film.2024-GRP/Subs/fr.srt",
    ]);
    c.add(req(&cand, true)).await.unwrap();
    c.recheck(&mc.infohash).await.unwrap();
    let t = until(
        c,
        &mc.infohash,
        "le candidat lié est vérifié à 100 %",
        90,
        |t| t.progress >= 0.9999 && t.status != TorrentStatus::Checking as i32,
    )
    .await;
    assert_eq!(t.dl_rate, 0, "aucun téléchargement");
    c.resume(&mc.infohash).await.unwrap();
    until(c, &mc.infohash, "le candidat lié partage", 30, |t| {
        // « En file d'attente » : le client limite le nombre d'envois actifs ; le torrent est complet et prêt.
        t.status == TorrentStatus::Seeding as i32 || t.status == TorrentStatus::Queued as i32
    })
    .await;

    // Suppression du candidat AVEC ses fichiers : l'original et son torrent restent intacts.
    c.remove(&mc.infohash, true).await.unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while c
        .list()
        .await
        .unwrap()
        .iter()
        .any(|t| t.infohash == mc.infohash)
    {
        assert!(Instant::now() < end, "retrait non effectué");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    use sha2::{Digest, Sha256};
    let sum = docker(&[
        "exec",
        container,
        "sha256sum",
        "/downloads/movies/Film.2024/video.mkv",
    ]);
    assert_eq!(
        sum.split(' ').next().unwrap(),
        data_encoding::HEXLOWER.encode(&Sha256::digest(&video)),
        "original intact"
    );
    let parent_state = c
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.infohash == mp.infohash)
        .unwrap();
    assert!(
        parent_state.progress >= 0.9999,
        "le torrent d'origine reste complet : {parent_state:?}"
    );
}

#[tokio::test]
#[ignore = "manuel : exige Docker et l'image linuxserver/qbittorrent"]
async fn qbittorrent_verifies_a_different_infohash_on_existing_files_without_downloading() {
    let _ = Command::new("docker")
        .args(["network", "create", "nxt-xs-net"])
        .output();
    rm("nxt-xs-q");
    docker(&[
        "run",
        "-d",
        "--name",
        "nxt-xs-q",
        "--network",
        "nxt-xs-net",
        "-e",
        "PUID=1000",
        "-e",
        "PGID=1000",
        "-e",
        "WEBUI_PORT=19401",
        "-p",
        "19401:19401",
        "lscr.io/linuxserver/qbittorrent:latest",
    ]);
    let end = Instant::now() + Duration::from_secs(90);
    let pw = loop {
        let l = Command::new("docker")
            .args(["logs", "nxt-xs-q"])
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
    let c = QbitClient::new("http://127.0.0.1:19401", "admin", &pw, "/downloads".into()).unwrap();
    scenario(&c, "nxt-xs-q").await;
    linked_scenario(&c, "nxt-xs-q").await;
    rm("nxt-xs-q");
    let _ = Command::new("docker")
        .args(["network", "rm", "nxt-xs-net"])
        .output();
}

#[tokio::test]
#[ignore = "manuel : exige Docker et l'image linuxserver/transmission"]
async fn transmission_verifies_a_different_infohash_on_existing_files_without_downloading() {
    let _ = Command::new("docker")
        .args(["network", "create", "nxt-xs-net"])
        .output();
    rm("nxt-xs-t");
    docker(&[
        "run",
        "-d",
        "--name",
        "nxt-xs-t",
        "--network",
        "nxt-xs-net",
        "-e",
        "PUID=1000",
        "-e",
        "PGID=1000",
        "-p",
        "19402:9091",
        "lscr.io/linuxserver/transmission:latest",
    ]);
    std::thread::sleep(Duration::from_secs(10));
    let c = TransmissionClient::new("http://127.0.0.1:19402", "", "", "/downloads".into()).unwrap();
    scenario(&c, "nxt-xs-t").await;
    linked_scenario(&c, "nxt-xs-t").await;
    rm("nxt-xs-t");
    let _ = Command::new("docker")
        .args(["network", "rm", "nxt-xs-net"])
        .output();
}
