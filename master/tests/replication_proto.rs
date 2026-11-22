//! Prototype (manuel, `--ignored`) : la réplication par le protocole BitTorrent lui-même, avec deux VRAIS clients
//! Transmission sur un réseau Docker isolé. Vérifie les trois inconnues de la conception :
//! 1. un client accepte un .torrent réécrit pour n'annoncer qu'au mini-tracker du master (même infohash) ;
//! 2. la copie se télécharge depuis la source, via les pairs fournis par le tracker, contenu vérifié ;
//! 3. on peut ensuite « rebrancher » la copie sur le .torrent d'origine (retrait sans les fichiers + ajout + vérif).
use base64_free::b64;
use std::process::Command;
use std::time::{Duration, Instant};

mod base64_free {
    pub fn b64(b: &[u8]) -> String {
        data_encoding::BASE64.encode(b)
    }
}

fn docker(args: &[&str]) -> String {
    let o = Command::new("docker").args(args).output().expect("docker");
    assert!(
        o.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

struct Rpc {
    url: String,
    http: reqwest::Client,
    sid: tokio::sync::Mutex<String>,
}
impl Rpc {
    async fn call(&self, method: &str, args: serde_json::Value) -> serde_json::Value {
        for _ in 0..3 {
            let sid = self.sid.lock().await.clone();
            let r = self
                .http
                .post(&self.url)
                .header("X-Transmission-Session-Id", sid)
                .json(&serde_json::json!({"method": method, "arguments": args}))
                .send()
                .await
                .expect("rpc");
            if r.status() == 409 {
                *self.sid.lock().await = r.headers()["x-transmission-session-id"]
                    .to_str()
                    .unwrap()
                    .to_owned();
                continue;
            }
            let v: serde_json::Value = r.json().await.expect("json");
            assert_eq!(v["result"], "success", "{method}: {v}");
            return v["arguments"].clone();
        }
        panic!("session id");
    }
    async fn torrent(&self, hash: &str) -> serde_json::Value {
        let a = self
            .call("torrent-get", serde_json::json!({"ids":[hash],"fields":["hashString","percentDone","status","error","errorString","peersConnected","downloadDir"]}))
            .await;
        a["torrents"][0].clone()
    }
    async fn wait(
        &self,
        hash: &str,
        what: &str,
        secs: u64,
        ok: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let t = self.torrent(hash).await;
            if ok(&t) {
                return t;
            }
            assert!(
                Instant::now() < end,
                "délai dépassé : {what} ; dernier état {t}"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

fn start(name: &str, port: u16) -> Rpc {
    let _ = Command::new("docker").args(["rm", "-f", name]).output();
    docker(&[
        "run",
        "-d",
        "--name",
        name,
        "--network",
        "nxt-proto",
        "--add-host",
        "host.docker.internal:host-gateway",
        "-e",
        "PUID=1000",
        "-e",
        "PGID=1000",
        "-p",
        &format!("{port}:9091"),
        "lscr.io/linuxserver/transmission:latest",
    ]);
    Rpc {
        url: format!("http://127.0.0.1:{port}/transmission/rpc"),
        http: reqwest::Client::new(),
        sid: tokio::sync::Mutex::new(String::new()),
    }
}

#[tokio::test]
#[ignore = "manuel : exige Docker et l'image linuxserver/transmission"]
async fn replica_downloads_from_source_via_private_tracker_then_retargets_to_original() {
    let _ = Command::new("docker")
        .args(["network", "create", "nxt-proto"])
        .output();
    // Contenu aléatoire (~3 Mio) et torrent d'ORIGINE (tracker public fictif).
    let mut x = 0x2545F4914F6CDD1Du64;
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
        true,
    );
    let meta = proto::metainfo::parse(&original).unwrap();
    let hash = meta.infohash.clone();
    let ih: [u8; 20] = data_encoding::HEXLOWER
        .decode(hash.as_bytes())
        .unwrap()
        .try_into()
        .unwrap();

    // Mini-tracker du master, joignable depuis les conteneurs.
    let tracker = master::tracker::Tracker::new(&[9u8; 32]);
    let l = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let tport = l.local_addr().unwrap().port();
    tokio::spawn(master::tracker::serve(l, tracker.clone()));

    let (src, rep) = (start("nxt-proto-src", 19201), start("nxt-proto-rep", 19202));
    tokio::time::sleep(Duration::from_secs(8)).await;
    for (n, c) in [("src", "nxt-proto-src"), ("rep", "nxt-proto-rep")] {
        let ip = docker(&[
            "inspect",
            "-f",
            "{{(index .NetworkSettings.Networks \"nxt-proto\").IPAddress}}",
            c,
        ]);
        tracker.set_node_ip(n, ip.parse().unwrap());
    }
    docker(&["exec", "nxt-proto-rep", "mkdir", "-p", "/downloads/rep"]);
    docker(&[
        "exec",
        "nxt-proto-rep",
        "chown",
        "-R",
        "1000:1000",
        "/downloads",
    ]);
    // La source possède déjà les données.
    docker(&["exec", "nxt-proto-src", "mkdir", "-p", "/downloads/rep"]);
    let tmp = std::env::temp_dir().join("proto.bin");
    std::fs::write(&tmp, &data).unwrap();
    docker(&[
        "cp",
        tmp.to_str().unwrap(),
        "nxt-proto-src:/downloads/rep/proto.bin",
    ]);
    docker(&[
        "exec",
        "nxt-proto-src",
        "chown",
        "-R",
        "1000:1000",
        "/downloads/rep",
    ]);

    // 1. .torrent réécrit : n'annonce QU'AU tracker du master, un jeton par nœud.
    let rewritten = |node: &str| {
        let url = format!(
            "http://host.docker.internal:{tport}{}",
            tracker.announce_path(&ih, node)
        );
        proto::metainfo::rewrite_announce(&original, &url).unwrap()
    };
    let add = |bytes: Vec<u8>, dir: &str, paused: bool| serde_json::json!({"metainfo": b64(&bytes), "download-dir": dir, "paused": paused});
    // La source garde son torrent D'ORIGINE (trackers publics) : le master lui ajoute seulement l'annonce privée,
    // par le VRAI driver de l'agent (idempotent : deux appels, une seule annonce).
    src.call(
        "torrent-add",
        add(original.clone(), "/downloads/rep", false),
    )
    .await;
    src.wait(&hash, "la source vérifie et seede", 60, |t| {
        t["percentDone"] == 1.0
    })
    .await;
    let driver = agent::transmission::TransmissionClient::new(
        "http://127.0.0.1:19201",
        "",
        "",
        "/downloads".into(),
    )
    .unwrap();
    let src_url = format!(
        "http://host.docker.internal:{tport}{}",
        tracker.announce_path(&ih, "src")
    );
    use agent::client::TorrentClient;
    // Transmission 4.1.4 n'exporte pas le .torrent : l'agent doit le dire clairement (repli sur l'ancien cycle).
    assert!(driver.export_torrent(&hash).await.is_err());
    driver.add_tracker(&hash, &src_url).await.unwrap();
    driver.add_tracker(&hash, &src_url).await.unwrap();
    let trackers = src
        .call(
            "torrent-get",
            serde_json::json!({"ids":[hash],"fields":["trackers"]}),
        )
        .await;
    let n = trackers["torrents"][0]["trackers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["announce"] == src_url.as_str())
        .count();
    assert_eq!(n, 1, "l'annonce privée est présente une seule fois");
    rep.call(
        "torrent-add",
        add(rewritten("rep"), "/downloads/rep", false),
    )
    .await;
    // Plafond de débit de la copie (par torrent), posé par le vrai driver et relu chez Transmission.
    let rep_driver = agent::transmission::TransmissionClient::new(
        "http://127.0.0.1:19202",
        "",
        "",
        "/downloads".into(),
    )
    .unwrap();
    rep_driver
        .set_torrent_limits(&hash, 50_000_000, 0)
        .await
        .unwrap();
    let t = rep
        .call(
            "torrent-get",
            serde_json::json!({"ids":[hash],"fields":["downloadLimit","downloadLimited","uploadLimited"]}),
        )
        .await;
    assert_eq!(t["torrents"][0]["downloadLimit"], 50_000);
    assert_eq!(t["torrents"][0]["downloadLimited"], true);
    assert_eq!(t["torrents"][0]["uploadLimited"], false);

    // 2. La copie se remplit depuis la source (pairs fournis par le tracker uniquement).
    let t = rep
        .wait(&hash, "la copie se télécharge", 180, |t| {
            t["percentDone"] == 1.0
        })
        .await;
    println!("copie terminée : {t}");
    let sum = |c: &str| docker(&["exec", c, "sha256sum", "/downloads/rep/proto.bin"]);
    assert_eq!(
        sum("nxt-proto-src").split(' ').next(),
        sum("nxt-proto-rep").split(' ').next(),
        "contenu identique"
    );
    assert_eq!(tracker.swarm_size(&ih), 2);

    // 3. Rebrancher la copie sur le torrent D'ORIGINE : retrait SANS les fichiers, ajout de l'original, revérification.
    rep.call(
        "torrent-remove",
        serde_json::json!({"ids":[hash], "delete-local-data": false}),
    )
    .await;
    rep.call("torrent-add", add(original.clone(), "/downloads/rep", true))
        .await;
    rep.call("torrent-verify", serde_json::json!({"ids":[hash]}))
        .await;
    let t = rep
        .wait(&hash, "revérification", 60, |t| {
            t["percentDone"] == 1.0 && t["status"] != 2
        })
        .await;
    rep.call("torrent-start", serde_json::json!({"ids":[hash]}))
        .await;
    let t2 = rep.torrent(&hash).await;
    println!("après rebranchement : {t} puis {t2}");
    assert_eq!(
        t2["hashString"],
        hash.as_str(),
        "même infohash après rebranchement"
    );

    for c in ["nxt-proto-src", "nxt-proto-rep"] {
        let _ = Command::new("docker").args(["rm", "-f", c]).output();
    }
    let _ = Command::new("docker")
        .args(["network", "rm", "nxt-proto"])
        .output();
}
