//! API d'administration : serveur HTTP réel, clients réels. Un test par propriété de sécurité annoncée.
use agent::pair::{pair, PairOptions};
use master::admin::{self, AdminSettings};
use master::{ca::Ca, service, store::Store, Core};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;

const PW: &str = "Correct-Horse-9";

struct Srv {
    core: Arc<Core>,
    base: String,
    agent_addr: String,
    _dir: TempDir,
    _stop: Vec<oneshot::Sender<()>>,
}

async fn start(mut settings: AdminSettings) -> Srv {
    let dir = tempfile::tempdir().unwrap();
    settings
        .data_dir
        .get_or_insert_with(|| dir.path().to_path_buf());
    let core = Core::new(
        Ca::load_or_create(dir.path()).unwrap(),
        Store::open_in_memory().unwrap(),
    );
    // Listener des agents (pour appairer un vrai agent) + listener d'administration.
    let al = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_addr = al.local_addr().unwrap().to_string();
    let (s1, r1) = oneshot::channel::<()>();
    let c = core.clone();
    tokio::spawn(async move {
        service::serve(c, al, &["127.0.0.1".to_owned()], async {
            let _ = r1.await;
        })
        .await
        .unwrap();
    });
    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", ml.local_addr().unwrap());
    let (s2, r2) = oneshot::channel::<()>();
    let c = core.clone();
    tokio::spawn(async move {
        admin::serve(c, ml, settings, async {
            let _ = r2.await;
        })
        .await
        .unwrap();
    });
    Srv {
        core,
        base,
        agent_addr,
        _dir: dir,
        _stop: vec![s1, s2],
    }
}

struct Client {
    http: reqwest::Client,
    base: String,
    cookie: Option<String>,
}

struct Resp {
    status: StatusCode,
    body: Value,
    headers: reqwest::header::HeaderMap,
}

impl Client {
    fn new(base: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.to_owned(),
            cookie: None,
        }
    }

    /// Client dont l'adresse source est `ip` (127.0.0.2, … : « autre machine » pour les limiteurs).
    fn from_ip(base: &str, ip: &str) -> Self {
        let http = reqwest::Client::builder()
            .local_address(ip.parse::<IpAddr>().unwrap())
            .build()
            .unwrap();
        Self {
            http,
            base: base.to_owned(),
            cookie: None,
        }
    }

    async fn call(&mut self, m: Method, path: &str, body: Option<Value>) -> Resp {
        let mut r = self.http.request(m.clone(), format!("{}{path}", self.base));
        if m != Method::GET {
            r = r.header("X-Requested-With", "nxt");
        }
        if let Some(c) = &self.cookie {
            r = r.header("Cookie", c);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let text = resp.text().await.unwrap();
        Resp {
            status,
            headers,
            body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        }
    }

    /// Envoi d'un corps binaire brut (fichier .torrent).
    async fn post_bytes(&mut self, path: &str, body: Vec<u8>) -> Resp {
        let mut r = self
            .http
            .post(format!("{}{path}", self.base))
            .header("X-Requested-With", "nxt")
            .header("Content-Type", "application/x-bittorrent")
            .body(body);
        if let Some(c) = &self.cookie {
            r = r.header("Cookie", c);
        }
        let resp = r.send().await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let text = resp.text().await.unwrap();
        Resp {
            status,
            headers,
            body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        }
    }

    async fn get(&mut self, path: &str) -> Resp {
        self.call(Method::GET, path, None).await
    }
    async fn post(&mut self, path: &str, body: Value) -> Resp {
        self.call(Method::POST, path, Some(body)).await
    }

    async fn login(&mut self, user: &str, pw: &str) -> Resp {
        let r = self
            .post("/api/login", json!({ "username": user, "password": pw }))
            .await;
        if r.status == StatusCode::OK {
            let sc = r.headers.get("set-cookie").unwrap().to_str().unwrap();
            self.cookie = Some(sc.split(';').next().unwrap().to_owned());
        }
        r
    }
}

async fn setup_and_login(s: &Srv) -> Client {
    let mut c = Client::new(&s.base);
    assert_eq!(
        c.post("/api/setup", json!({ "username": "alice", "password": PW }))
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(c.login("alice", PW).await.status, StatusCode::OK);
    c
}

#[tokio::test]
async fn first_start_setup_enforces_policy_and_only_one_account() {
    let s = start(AdminSettings::default()).await;
    let mut c = Client::new(&s.base);
    assert_eq!(c.get("/api/health").await.body["setup_required"], true);

    for (user, pw, why) in [
        ("alice", "court", "trop court"),
        ("alice", "aaaaaaaaaaaaaaaa", "répétitif"),
        ("alice", "alice-Secret-2024", "contient le nom"),
        ("a b", PW, "nom invalide"),
        ("ab", PW, "nom trop court"),
    ] {
        let r = c
            .post("/api/setup", json!({ "username": user, "password": pw }))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{why}: {}", r.body);
    }
    assert_eq!(
        c.get("/api/health").await.body["setup_required"],
        true,
        "rien n'a été créé"
    );

    assert_eq!(
        c.post("/api/setup", json!({ "username": "alice", "password": PW }))
            .await
            .status,
        StatusCode::CREATED
    );
    let again = c
        .post(
            "/api/setup",
            json!({ "username": "mallory", "password": PW }),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(c.get("/api/health").await.body["setup_required"], false);
    // Le mot de passe n'est stocké que haché (Argon2id).
    let h = s.core.store.admin_hash("alice").unwrap().unwrap();
    assert!(h.starts_with("$argon2id$") && !h.contains(PW));
}

#[tokio::test]
async fn simultaneous_setups_create_exactly_one_account() {
    let s = start(AdminSettings::default()).await;
    let (mut a, mut b) = (Client::new(&s.base), Client::new(&s.base));
    let (ra, rb) = tokio::join!(
        a.post("/api/setup", json!({ "username": "alice", "password": PW })),
        b.post("/api/setup", json!({ "username": "bobby", "password": PW })),
    );
    let mut got = [ra.status, rb.status];
    got.sort();
    assert_eq!(got, [StatusCode::CREATED, StatusCode::CONFLICT]);
    assert_eq!(s.core.store.admin_count().unwrap(), 1);
}

#[tokio::test]
async fn login_sets_a_hardened_cookie_and_gates_every_endpoint() {
    let s = start(AdminSettings::default()).await;
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.post("/api/setup", json!({ "username": "alice", "password": PW }))
            .await
            .status,
        StatusCode::CREATED
    );

    for path in ["/api/nodes", "/api/torrents", "/api/audit"] {
        assert_eq!(
            anon.get(path).await.status,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    assert_eq!(
        anon.post("/api/pairing-codes", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Compte inconnu et mauvais mot de passe : réponse strictement identique (pas d'énumération).
    let unknown = anon
        .post(
            "/api/login",
            json!({ "username": "nobody", "password": PW }),
        )
        .await;
    let wrong = anon
        .post(
            "/api/login",
            json!({ "username": "alice", "password": "Wrong-Password-1" }),
        )
        .await;
    assert_eq!((unknown.status, &unknown.body), (wrong.status, &wrong.body));
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);

    let ok = anon.login("alice", PW).await;
    assert_eq!(ok.status, StatusCode::OK);
    let cookie = ok
        .headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=28800"] {
        assert!(cookie.contains(flag), "{flag} absent : {cookie}");
    }
    assert!(
        !cookie.contains("Secure"),
        "boucle locale en HTTP : pas de Secure"
    );
    assert_eq!(
        cookie.split(';').next().unwrap().len(),
        "nxt_session=".len() + 64,
        "jeton de 256 bits"
    );

    let r = anon.get("/api/nodes").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    assert!(r.headers["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'none'"));
}

#[tokio::test]
async fn csrf_protection_requires_custom_header_and_same_origin() {
    let s = start(AdminSettings::default()).await;
    let c = setup_and_login(&s).await;
    let url = format!("{}/api/pairing-codes", s.base);
    let cookie = c.cookie.clone().unwrap();
    let raw = |hdrs: Vec<(&str, String)>| {
        let mut r = c.http.post(&url).header("Cookie", &cookie).json(&json!({}));
        for (k, v) in hdrs {
            r = r.header(k, v);
        }
        r.send()
    };
    // Sans l'en-tête personnalisé (ce que ferait un formulaire d'un site tiers) : refusé.
    assert_eq!(raw(vec![]).await.unwrap().status(), StatusCode::FORBIDDEN);
    // Avec l'en-tête mais une origine étrangère : refusé.
    let evil = raw(vec![
        ("X-Requested-With", "nxt".into()),
        ("Origin", "http://evil.example".into()),
    ])
    .await
    .unwrap();
    assert_eq!(evil.status(), StatusCode::FORBIDDEN);
    // Origine = celle du serveur : accepté.
    let host = s.base.trim_start_matches("http://").to_owned();
    let good = raw(vec![
        ("X-Requested-With", "nxt".into()),
        ("Origin", format!("http://{host}")),
    ])
    .await
    .unwrap();
    assert_eq!(good.status(), StatusCode::CREATED);
    // Un corps non JSON n'est pas accepté non plus (pas de formulaire classique).
    let form = c
        .http
        .post(&url)
        .header("Cookie", &cookie)
        .header("X-Requested-With", "nxt")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("labels=x")
        .send()
        .await
        .unwrap();
    assert!(form.status().is_client_error());
    // Corps énorme : refusé avant tout traitement.
    let big = c
        .http
        .post(&url)
        .header("Cookie", &cookie)
        .header("X-Requested-With", "nxt")
        .json(&json!({ "labels": ["a".repeat(200_000)] }))
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn brute_force_is_blocked_per_ip_with_retry_after_and_expires() {
    let s = start(AdminSettings {
        max_failures_per_ip: 3,
        lockout_window: Duration::from_millis(1500),
        ..AdminSettings::default()
    })
    .await;
    let mut c = Client::new(&s.base);
    c.post("/api/setup", json!({ "username": "alice", "password": PW }))
        .await;
    for _ in 0..3 {
        assert_eq!(
            c.login("alice", "Wrong-Password-1").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Bloqué : même le BON mot de passe est refusé.
    let blocked = c.login("alice", PW).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = blocked.headers["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&retry), "Retry-After = {retry}");
    for _ in 0..5 {
        assert_eq!(
            c.login("alice", "x").await.status,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    // Un seul « bloqué » dans l'audit malgré l'insistance (pas de remplissage du journal).
    let n = s
        .core
        .store
        .audit_actions()
        .unwrap()
        .iter()
        .filter(|a| *a == "admin_login_blocked")
        .count();
    assert_eq!(n, 1);
    // La fenêtre passée, la connexion légitime refonctionne.
    tokio::time::sleep(Duration::from_millis(1700)).await;
    assert_eq!(c.login("alice", PW).await.status, StatusCode::OK);
}

#[tokio::test]
async fn brute_force_is_also_blocked_per_account_across_ips() {
    let s = start(AdminSettings {
        max_failures_per_ip: 100,
        max_failures_per_user: 3,
        ..AdminSettings::default()
    })
    .await;
    let mut a = Client::from_ip(&s.base, "127.0.0.1");
    a.post("/api/setup", json!({ "username": "alice", "password": PW }))
        .await;
    for _ in 0..3 {
        a.login("alice", "Wrong-Password-1").await;
    }
    // Une autre « machine » (autre IP source) est aussi bloquée sur ce compte, mot de passe correct ou non.
    let mut b = Client::from_ip(&s.base, "127.0.0.2");
    assert_eq!(
        b.login("alice", PW).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Mais pas sur un autre compte (pas de blocage global).
    assert_eq!(b.login("bobby", PW).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_successful_login_resets_the_failure_counters() {
    let s = start(AdminSettings {
        max_failures_per_ip: 3,
        ..AdminSettings::default()
    })
    .await;
    let mut c = Client::new(&s.base);
    c.post("/api/setup", json!({ "username": "alice", "password": PW }))
        .await;
    for round in 0..3 {
        for _ in 0..2 {
            assert_eq!(
                c.login("alice", "Wrong-Password-1").await.status,
                StatusCode::UNAUTHORIZED,
                "tour {round}"
            );
        }
        assert_eq!(
            c.login("alice", PW).await.status,
            StatusCode::OK,
            "tour {round}"
        );
    }
}

#[tokio::test]
async fn sessions_expire_on_idle_and_on_absolute_lifetime() {
    let s = start(AdminSettings {
        idle_timeout: Duration::from_millis(500),
        absolute_timeout: Duration::from_millis(1400),
        ..AdminSettings::default()
    })
    .await;
    let mut c = setup_and_login(&s).await;
    // Activité CONTINUE (jamais plus de 300 ms de silence, donc jamais d'inactivité de 500 ms) :
    // la session doit tenir tant que la durée absolue (1,4 s) n'est pas atteinte, puis tomber.
    let start = std::time::Instant::now();
    let mut expired_at = None;
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if c.get("/api/nodes").await.status == StatusCode::UNAUTHORIZED {
            expired_at = Some(start.elapsed());
            break;
        }
    }
    let at =
        expired_at.expect("la session aurait dû expirer par sa durée absolue malgré l'activité");
    assert!(
        at >= Duration::from_millis(1200) && at <= Duration::from_millis(2400),
        "expirée après {at:?} (durée absolue = 1,4 s)"
    );
    // Inactivité seule.
    assert_eq!(c.login("alice", PW).await.status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        c.get("/api/nodes").await.status,
        StatusCode::UNAUTHORIZED,
        "inactivité dépassée"
    );
}

#[tokio::test]
async fn logout_and_password_change_close_sessions() {
    let s = start(AdminSettings::default()).await;
    let mut a = setup_and_login(&s).await;
    let mut b = Client::new(&s.base);
    assert_eq!(b.login("alice", PW).await.status, StatusCode::OK);

    // Changement de mot de passe depuis la session A : refus si le mot de passe actuel est faux ou le nouveau faible.
    assert_eq!(
        a.post(
            "/api/password",
            json!({ "current": "Wrong-Password-1", "new": "Brand-New-Pass-77" })
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        a.post("/api/password", json!({ "current": PW, "new": "court" }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        a.post(
            "/api/password",
            json!({ "current": PW, "new": "Brand-New-Pass-77" })
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        a.get("/api/nodes").await.status,
        StatusCode::OK,
        "la session qui change le mot de passe reste ouverte"
    );
    assert_eq!(
        b.get("/api/nodes").await.status,
        StatusCode::UNAUTHORIZED,
        "les autres sessions sont fermées"
    );
    let mut c = Client::new(&s.base);
    assert_eq!(
        c.login("alice", PW).await.status,
        StatusCode::UNAUTHORIZED,
        "ancien mot de passe invalide"
    );
    assert_eq!(
        c.login("alice", "Brand-New-Pass-77").await.status,
        StatusCode::OK
    );

    // Déconnexion : le jeton ne vaut plus rien et le cookie est effacé.
    let out = a.post("/api/logout", json!({})).await;
    assert_eq!(out.status, StatusCode::OK);
    assert!(out.headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(a.get("/api/nodes").await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pairing_codes_nodes_and_revocation_through_the_api() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;

    let bad = c
        .post("/api/pairing-codes", json!({ "labels": ["a b"] }))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let made = c
        .post("/api/pairing-codes", json!({ "labels": ["ssd"] }))
        .await;
    assert_eq!(made.status, StatusCode::CREATED);
    let code = made.body["code"].as_str().unwrap().to_owned();
    assert!(code.starts_with("CODE-"));

    // Un vrai agent s'appaire avec ce code : le nœud apparaît avec les labels de l'admin.
    let dir = tempfile::tempdir().unwrap();
    let node_id = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code: code.clone(),
        data_dir: dir.path().to_owned(),
        hostname: "nas-1".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;
    let nodes = c.get("/api/nodes").await.body;
    let n = &nodes.as_array().unwrap()[0];
    assert_eq!(
        (n["id"].as_str().unwrap(), n["hostname"].as_str().unwrap()),
        (node_id.as_str(), "nas-1")
    );
    assert_eq!(n["labels"], json!(["ssd"]));
    assert_eq!(n["state"], "Pending");
    assert_eq!(n["connected"], false);

    // Révocation : identifiants invalides ou inconnus -> 404, jamais d'effet.
    for id in [
        "../etc",
        "nimporte-quoi",
        "123e4567-e89b-12d3-a456-426614174000",
    ] {
        assert_eq!(
            c.post(&format!("/api/nodes/{id}/revoke"), json!({}))
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{id}"
        );
    }
    assert_eq!(
        c.post(&format!("/api/nodes/{node_id}/revoke"), json!({}))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(c.get("/api/nodes").await.body[0]["state"], "Revoked");

    // L'audit nomme l'auteur et ne contient JAMAIS le code d'appairage ni un mot de passe.
    let log = c.get("/api/audit?limit=200").await.body["items"].clone();
    let text = log.to_string();
    assert!(
        !text.contains(&code) && !text.contains(PW),
        "secret dans l'audit"
    );
    let find = |action: &str| {
        log.as_array()
            .unwrap()
            .iter()
            .find(|e| e["action"] == action)
            .cloned()
    };
    assert_eq!(
        find("pairing_code_created").unwrap()["actor"],
        "admin:alice"
    );
    assert_eq!(find("node_revoked").unwrap()["actor"], "admin:alice");
    assert!(
        find("node_revoked").unwrap()["title"]
            .as_str()
            .unwrap()
            .contains("révoqué"),
        "phrase lisible"
    );
    assert!(
        find("admin_created").is_some()
            && find("admin_login").is_some()
            && find("node_paired").is_some()
    );
}

#[tokio::test]
async fn torrent_management_validates_input_and_reports_state() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let ih = "1111111111111111111111111111111111111111";

    for (body, why) in [
        (json!({}), "aucune source"),
        (
            json!({ "magnet": "magnet:?xt=urn:btih:".to_owned() + ih, "infohash": ih }),
            "deux sources",
        ),
        (
            json!({ "magnet": "http://evil.example/x.torrent" }),
            "magnet invalide",
        ),
        (json!({ "infohash": "pas-un-hash" }), "infohash invalide"),
        (
            json!({ "infohash": ih, "save_subdir": "../x" }),
            "traversée de chemin",
        ),
        (
            json!({ "infohash": ih, "save_subdir": "/etc" }),
            "chemin absolu",
        ),
        (
            json!({ "infohash": ih, "requires": ["a b"] }),
            "label invalide",
        ),
    ] {
        let r = c.post("/api/torrents", body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{why}: {}", r.body);
    }
    assert_eq!(
        c.get("/api/torrents").await.body["total"],
        0,
        "rien n'a été créé"
    );

    let r = c.post("/api/torrents", json!({ "infohash": ih, "save_subdir": "movies", "requires": ["tor"], "replicate_group": "mirror" })).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["infohash"], ih);
    let t = c.get("/api/torrents").await.body["items"][0].clone();
    assert_eq!(
        t["status"], "pending",
        "aucun nœud conforme : reste Pending"
    );
    assert_eq!(
        (
            t["save_subdir"].as_str().unwrap(),
            t["paused"].as_bool().unwrap()
        ),
        ("movies", false)
    );
    assert_eq!(t["requires"], json!(["tor"]));

    assert_eq!(
        c.post(&format!("/api/torrents/{ih}/pause"), json!({}))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        c.get("/api/torrents").await.body["items"][0]["paused"],
        true
    );
    assert_eq!(
        c.post(&format!("/api/torrents/{ih}/resume"), json!({}))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        c.get("/api/torrents").await.body["items"][0]["paused"],
        false
    );
    assert_eq!(
        c.post(
            "/api/torrents/2222222222222222222222222222222222222222/pause",
            json!({})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        c.call(
            Method::DELETE,
            &format!("/api/torrents/{ih}?delete_files=true"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(c.get("/api/torrents").await.body["total"], 0);
    assert_eq!(
        c.call(
            Method::DELETE,
            "/api/torrents/2222222222222222222222222222222222222222",
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let actions: Vec<String> = c.get("/api/audit?limit=200").await.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| format!("{}:{}", e["actor"], e["action"]))
        .collect();
    for a in [
        "torrent_added",
        "torrent_paused",
        "torrent_resumed",
        "torrent_removed",
    ] {
        assert!(
            actions
                .iter()
                .any(|x| x.contains(a) && x.contains("admin:alice")),
            "{a} : {actions:?}"
        );
    }
}

#[tokio::test]
async fn audit_endpoint_paginates_filters_and_clamps() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    for _ in 0..4 {
        c.post("/api/pairing-codes", json!({})).await;
    }
    let all = c.get("/api/audit?limit=200").await.body["items"].clone();
    let ids: Vec<i64> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert!(
        ids.windows(2).all(|w| w[0] > w[1]),
        "les plus récents d'abord"
    );

    let page1 = c.get("/api/audit?limit=2").await.body;
    assert_eq!(page1["items"].as_array().unwrap().len(), 2);
    let last = page1["items"][1]["id"].as_i64().unwrap();
    assert_eq!(
        page1["next_before"].as_i64(),
        Some(last),
        "curseur de page suivante"
    );
    let page2 = c
        .get(&format!("/api/audit?limit=2&before={last}"))
        .await
        .body;
    assert!(page2["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["id"].as_i64().unwrap() < last));

    let only = c
        .get("/api/audit?action=pairing_code_created&limit=200")
        .await
        .body["items"]
        .clone();
    assert_eq!(only.as_array().unwrap().len(), 4);
    assert!(only
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["action"] == "pairing_code_created"));
    assert_eq!(
        c.get("/api/audit?limit=0").await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "limite minimale 1"
    );
    assert!(
        c.get("/api/audit?limit=999999").await.body["items"]
            .as_array()
            .unwrap()
            .len()
            <= 200,
        "limite maximale 200"
    );
    assert_eq!(
        c.get("/api/audit?limit=abc").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn torrent_files_are_validated_stored_and_listed_with_their_name() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;

    // Pas un .torrent / vide : refusés sans rien créer.
    for body in [
        b"pas du bencode".to_vec(),
        b"d4:infod4:name1:xee".to_vec(),
        vec![],
    ] {
        let r = c.post_bytes("/api/torrents/file", body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    }
    assert_eq!(c.get("/api/torrents").await.body["total"], 0);

    let file = testkit::torrent::sample("seedbox-file", 4096, 0x42);
    let ih = testkit::torrent::hash_from_file(&file).unwrap();
    let r = c
        .post_bytes(
            "/api/torrents/file?save_subdir=iso&requires=tor,ssd",
            file.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["infohash"], ih.as_str());

    let t = c.get("/api/torrents").await.body["items"][0].clone();
    assert_eq!(t["name"], "seedbox-file", "le nom vient du fichier");
    assert_eq!(t["size"], 4096);
    assert_eq!(t["save_subdir"], "iso");
    assert_eq!(t["requires"], json!(["tor", "ssd"]));
    assert_eq!(
        s.core.store.torrent_file(&ih).unwrap().as_deref(),
        Some(file.as_slice()),
        "le contenu est conservé pour être envoyé aux nœuds"
    );

    // Chemin hostile dans les options : refusé comme pour un magnet.
    let r = c
        .post_bytes("/api/torrents/file?save_subdir=../x", file)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Fichier trop gros : refusé par la limite de corps, pas par un plantage.
    let big = vec![b'd'; proto::metainfo::MAX_TORRENT_FILE + 10_000];
    let r = c.post_bytes("/api/torrents/file", big).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);

    // Authentification exigée.
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.post_bytes("/api/torrents/file", vec![1]).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn nodes_can_be_named_at_pairing_and_renamed_later() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let r = c
        .post("/api/pairing-codes", json!({ "name": "NAS du salon" }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["name"], "NAS du salon");
    let code = r.body["code"].as_str().unwrap().to_owned();
    let dir = tempfile::tempdir().unwrap();
    let node_id = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "host-1".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;

    let n = c.get("/api/nodes").await.body[0].clone();
    assert_eq!(
        (n["name"].as_str().unwrap(), n["hostname"].as_str().unwrap()),
        ("NAS du salon", "host-1")
    );

    // Renommage, puis retour au nom d'hôte avec un nom vide.
    let url = format!("/api/nodes/{node_id}/rename");
    assert_eq!(
        c.post(&url, json!({ "name": "Seedbox 2" })).await.status,
        StatusCode::OK
    );
    assert_eq!(c.get("/api/nodes").await.body[0]["name"], "Seedbox 2");
    assert_eq!(
        c.post(&url, json!({ "name": "  " })).await.status,
        StatusCode::OK
    );
    let n = c.get("/api/nodes").await.body[0].clone();
    assert_eq!(
        (n["name"].as_str().unwrap(), n["custom_name"].is_null()),
        ("host-1", true)
    );

    for bad in ["x".repeat(65), "a\nb".to_owned()] {
        assert_eq!(
            c.post(&url, json!({ "name": bad })).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        c.post(
            "/api/nodes/00000000-0000-4000-8000-000000000000/rename",
            json!({ "name": "x" })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Le journal le raconte en clair.
    let log = c.get("/api/audit?action=node_renamed").await.body;
    assert!(log["items"][0]["title"]
        .as_str()
        .unwrap()
        .contains("Seedbox 2"));

    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.post(&url, json!({ "name": "x" })).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn node_labels_are_admin_controlled_validated_and_audited() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let code = c.post("/api/pairing-codes", json!({})).await.body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = tempfile::tempdir().unwrap();
    let node_id = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "h".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;
    let url = format!("/api/nodes/{node_id}/labels");

    let r = c
        .post(&url, json!({ "labels": ["ssd", " vpn-ch ", "ssd", ""] }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body["labels"],
        json!(["ssd", "vpn-ch"]),
        "nettoyés, sans doublon"
    );
    assert_eq!(
        c.get("/api/nodes").await.body[0]["labels"],
        json!(["ssd", "vpn-ch"])
    );

    // Retirer tous les labels est permis.
    assert_eq!(
        c.post(&url, json!({ "labels": [] })).await.status,
        StatusCode::OK
    );
    assert_eq!(c.get("/api/nodes").await.body[0]["labels"], json!([]));

    for bad in [json!(["a b"]), json!(["../x"]), json!(["é"])] {
        assert_eq!(
            c.post(&url, json!({ "labels": bad })).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    let many: Vec<String> = (0..21).map(|i| format!("l{i}")).collect();
    assert_eq!(
        c.post(&url, json!({ "labels": many })).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.post(
            "/api/nodes/00000000-0000-4000-8000-000000000000/labels",
            json!({ "labels": ["x"] })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let log = c.get("/api/audit?action=node_labels_changed").await.body;
    assert!(log["items"][0]["title"]
        .as_str()
        .unwrap()
        .contains("Labels"));

    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.post(&url, json!({ "labels": ["x"] })).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn trackers_peers_location_and_node_comparison() {
    use proto::pb::TorrentInfo;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let code = c.post("/api/pairing-codes", json!({})).await.body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = tempfile::tempdir().unwrap();
    let node_id = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "h".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;

    // Ce que remonterait l'agent : torrents avec pairs, essaim et tracker.
    let t = |h: &str, tracker: &str, peers: u32| TorrentInfo {
        infohash: h.repeat(40),
        name: format!("torrent {h}"),
        status: proto::pb::TorrentStatus::Seeding as i32,
        progress: 1.0,
        size_bytes: 1000,
        peers_connected: peers,
        peers_active: peers / 2,
        swarm_seeds: 50,
        swarm_leechers: 12,
        tracker: tracker.into(),
        ..TorrentInfo::default()
    };
    s.core.record(&node_id, |tel| {
        tel.torrents = vec![
            t("a", "tracker.alpha.org", 20),
            t("b", "tracker.alpha.org", 5),
            t("c", "tracker.beta.org", 9),
            t("d", "", 0),
        ];
    });

    let p = c.get("/api/torrents?sort=name").await.body;
    assert_eq!(p["total"], 4);
    let facets = p["trackers"].as_array().unwrap();
    assert_eq!(
        facets[0],
        json!({ "host": "tracker.alpha.org", "count": 2 })
    );
    assert!(
        facets.iter().any(|f| f["host"] == "-" && f["count"] == 1),
        "tracker inconnu"
    );

    let p = c
        .get("/api/torrents?tracker=tracker.alpha.org&sort=peers&dir=desc")
        .await
        .body;
    assert_eq!(p["total"], 2);
    assert_eq!(p["items"][0]["peers_connected"], 20);
    assert_eq!(p["items"][0]["peers_active"], 10);
    assert_eq!(
        (
            p["items"][0]["swarm_seeds"].as_u64(),
            p["items"][0]["swarm_leechers"].as_u64()
        ),
        (Some(50), Some(12))
    );
    assert_eq!(c.get("/api/torrents?tracker=-").await.body["total"], 1);
    // Les facettes restent complètes quand on filtre sur un tracker (on peut en changer).
    assert_eq!(
        c.get("/api/torrents?tracker=tracker.beta.org").await.body["trackers"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Localisation (texte libre) : validée, journalisée.
    let url = format!("/api/nodes/{node_id}/location");
    assert_eq!(
        c.post(&url, json!({ "location": "Hetzner · Falkenstein" }))
            .await
            .status,
        StatusCode::OK
    );
    let n = c.get("/api/nodes").await.body[0].clone();
    assert_eq!(n["location"], "Hetzner · Falkenstein");
    assert_eq!(
        c.post(&url, json!({ "location": "x".repeat(81) }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.post(&url, json!({ "location": "a\nb" })).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.post(&url, json!({ "location": "" })).await.status,
        StatusCode::OK
    );
    assert!(c.get("/api/nodes").await.body[0]["location"].is_null());
    assert_eq!(
        c.post(
            "/api/nodes/00000000-0000-4000-8000-000000000000/location",
            json!({ "location": "x" })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Santé réseau présente (nœud hors ligne : aucun diagnostic périmé).
    assert_eq!(n["network"]["incoming"], "unknown");
    assert_eq!(n["network"]["peers"], 34);

    // Comparaison des nœuds : au moins un échantillon, avec les champs attendus.
    s.core.sample_metrics().unwrap();
    let st = c.get("/api/nodes/stats?range=24h").await;
    assert_eq!(st.status, StatusCode::OK, "{}", st.body);
    let row = &st.body["nodes"][0];
    assert_eq!(row["node_id"], node_id.as_str());
    for k in [
        "availability",
        "avg_ul",
        "up_bytes",
        "up_share",
        "underperforming",
    ] {
        assert!(!row[k].is_null(), "{k}");
    }
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/nodes/stats").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn torrent_list_is_filtered_sorted_and_paginated_on_the_server() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    for i in 0..120u32 {
        // Chaque fichier a un contenu (donc un infohash) distinct.
        let f = testkit::torrent::sample(&format!("linux-{i:03}"), 1000 + i, (i % 250) as u8 + 1);
        assert_eq!(
            c.post_bytes("/api/torrents/file", f).await.status,
            StatusCode::CREATED,
            "{i}"
        );
    }
    let p = c.get("/api/torrents?limit=50&sort=name").await.body;
    assert_eq!(p["total"], 120);
    assert_eq!(p["items"].as_array().unwrap().len(), 50);
    assert_eq!(p["items"][0]["name"], "linux-000");
    let p = c
        .get("/api/torrents?limit=10&offset=115&sort=name")
        .await
        .body;
    assert_eq!(
        p["items"].as_array().unwrap().len(),
        5,
        "dernière page partielle"
    );
    let p = c.get("/api/torrents?q=LINUX-07&sort=name").await.body;
    assert_eq!(p["total"], 10);
    let p = c.get("/api/torrents?sort=size&dir=desc&limit=1").await.body;
    assert_eq!(p["items"][0]["size"], 1119);
    let p = c.get("/api/torrents?status=seeding").await.body;
    assert_eq!(p["total"], 0);
    assert_eq!(
        p["counts"]["pending"], 120,
        "effectifs par statut pour les puces de filtre"
    );
    assert_eq!(
        c.get("/api/torrents?limit=99999").await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        120
    );

    // Actions groupées.
    let ihs: Vec<String> = c.get("/api/torrents?limit=3").await.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["infohash"].as_str().unwrap().to_owned())
        .collect();
    let r = c
        .post(
            "/api/torrents/bulk",
            json!({ "action": "pause", "infohashes": ihs }),
        )
        .await;
    assert_eq!(
        (r.body["done"].as_u64(), r.body["failed"].as_u64()),
        (Some(3), Some(0))
    );
    let r = c
        .post(
            "/api/torrents/bulk",
            json!({ "action": "remove", "infohashes": [ihs[0], "pas-un-hash"] }),
        )
        .await;
    assert_eq!(
        (r.body["done"].as_u64(), r.body["failed"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(c.get("/api/torrents").await.body["total"], 119);
    for bad in [
        json!({ "action": "format", "infohashes": ["x"] }),
        json!({ "action": "pause", "infohashes": [] }),
    ] {
        assert_eq!(
            c.post("/api/torrents/bulk", bad).await.status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn stats_and_metrics_history() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let st = c.get("/api/stats").await.body;
    assert_eq!(
        (st["nodes_online"].as_u64(), st["torrents_total"].as_u64()),
        (Some(0), Some(0))
    );
    s.core.sample_metrics().unwrap();
    let m = c.get("/api/metrics?range=1h").await.body;
    assert_eq!(m["points"].as_array().unwrap().len(), 1);
    assert_eq!(m["points"][0]["nodes_online"], 0);
    let mut anon = Client::new(&s.base);
    for p in ["/api/stats", "/api/metrics"] {
        assert_eq!(anon.get(p).await.status, StatusCode::UNAUTHORIZED, "{p}");
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> Value {
    use futures_util::StreamExt;
    let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(m.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn live_channel_requires_a_session_and_the_same_origin() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let addr = s.base.trim_start_matches("http://").to_owned();
    let upgrade = |origin: Option<&str>, cookie: Option<&str>| {
        let mut r = reqwest::Client::new()
            .get(format!("http://{addr}/api/live"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(o) = origin {
            r = r.header("Origin", o);
        }
        if let Some(c) = cookie {
            r = r.header("Cookie", c);
        }
        r
    };
    assert_eq!(
        upgrade(None, None).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = c.cookie.clone().unwrap();
    assert_eq!(
        upgrade(Some("http://evil.example"), Some(&cookie))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN,
        "un autre site ne peut pas ouvrir le canal avec le cookie de l'admin"
    );

    // Vrai client WebSocket : le premier message est un instantané complet.
    let mut req = format!("ws://{addr}/api/live")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("Cookie", cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let v = next_json(&mut ws).await;
    assert_eq!(v["type"], "tick");
    assert_eq!(v["stats"]["nodes_online"], 0);
    assert!(v["nodes"].as_array().unwrap().is_empty());

    // Un changement d'état est poussé sans que le client ne demande rien.
    c.post("/api/torrents", json!({ "infohash": "1".repeat(40) }))
        .await;
    let v = next_json(&mut ws).await;
    assert!(v["rev"].as_u64().unwrap() >= 1);
    assert_eq!(v["stats"]["torrents_pending"], 1);
}

#[tokio::test]
async fn two_factor_login_replay_protection_recovery_codes_and_disable() {
    use master::admin::totp;
    // Limites relevées : ce test enchaîne volontairement beaucoup d'échecs (le verrouillage a son propre test).
    let s = start(AdminSettings {
        max_failures_per_ip: 1000,
        max_failures_per_user: 1000,
        ..AdminSettings::default()
    })
    .await;
    let mut c = setup_and_login(&s).await;
    let now = || master::store::unix_now();
    let code_for = |secret: &[u8], step: i64| format!("{:06}", totp::code_at(secret, step));

    // Sans session : rien n'est possible.
    let mut anon = Client::new(&s.base);
    for (p, body) in [
        ("/api/totp/setup", json!({})),
        ("/api/totp/enable", json!({ "code": "123456" })),
        (
            "/api/totp/disable",
            json!({ "password": "x", "code": "123456" }),
        ),
    ] {
        assert_eq!(
            anon.post(p, body).await.status,
            StatusCode::UNAUTHORIZED,
            "{p}"
        );
    }

    assert_eq!(c.get("/api/account").await.body["totp_enabled"], false);

    // Configuration : le secret n'est actif qu'après un code correct.
    let setup = c.post("/api/totp/setup", json!({})).await;
    assert_eq!(setup.status, StatusCode::OK);
    assert!(setup.body["uri"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://totp/nxtTorrent:alice"));
    assert!(setup.body["qr"]
        .as_str()
        .unwrap()
        .starts_with("data:image/svg+xml;base64,"));
    let secret = data_encoding::BASE32_NOPAD
        .decode(setup.body["secret"].as_str().unwrap().as_bytes())
        .unwrap();
    assert_eq!(
        c.get("/api/account").await.body["totp_enabled"],
        false,
        "pas encore actif"
    );
    let bad = c
        .post("/api/totp/enable", json!({ "code": "000000" }))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let step = totp::step_of(now());
    let r = c
        .post(
            "/api/totp/enable",
            json!({ "code": code_for(&secret, step) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let recovery: Vec<String> = r.body["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(recovery.len(), 10);
    let acct = c.get("/api/account").await.body;
    assert_eq!(
        (
            acct["totp_enabled"].as_bool(),
            acct["recovery_remaining"].as_u64()
        ),
        (Some(true), Some(10))
    );
    assert_eq!(
        c.post("/api/totp/setup", json!({})).await.status,
        StatusCode::CONFLICT
    );

    // Connexion : le mot de passe seul ne donne PLUS de session.
    let login = |ip: &str| {
        let mut x = Client::from_ip(&s.base, ip);
        async move {
            let r = x
                .post("/api/login", json!({ "username": "alice", "password": PW }))
                .await;
            (x, r)
        }
    };
    let (mut x, r) = login("127.0.0.1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["needs_totp"], true);
    assert!(
        r.headers.get("set-cookie").is_none(),
        "aucune session avant le code"
    );
    assert_eq!(x.get("/api/me").await.status, StatusCode::UNAUTHORIZED);
    let ticket = r.body["ticket"].as_str().unwrap().to_owned();

    // Le code qui a servi à activer ne peut pas servir à se connecter (rejeu) ; un mauvais code non plus.
    for bad in [code_for(&secret, step), "123456".to_owned(), String::new()] {
        let r = x
            .post("/api/login/totp", json!({ "ticket": ticket, "code": bad }))
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad:?}");
    }
    // Un ticket inventé est refusé.
    assert_eq!(
        x.post(
            "/api/login/totp",
            json!({ "ticket": "f".repeat(48), "code": "123456" })
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    // Code correct (pas suivant) : session ouverte.
    let good = code_for(&secret, step + 1);
    let r = x
        .post("/api/login/totp", json!({ "ticket": ticket, "code": good }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let sc = r.headers.get("set-cookie").unwrap().to_str().unwrap();
    x.cookie = Some(sc.split(';').next().unwrap().to_owned());
    assert_eq!(x.get("/api/me").await.status, StatusCode::OK);
    // Le ticket est consommé, et le même code ne rejoue pas avec un nouveau ticket.
    let (mut y, r) = login("127.0.0.1").await;
    let t2 = r.body["ticket"].as_str().unwrap().to_owned();
    assert_eq!(
        y.post("/api/login/totp", json!({ "ticket": t2, "code": good }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    // Épuisement du ticket : 5 essais ratés, puis même le bon code est refusé.
    let (mut z, r) = login("127.0.0.1").await;
    let t3 = r.body["ticket"].as_str().unwrap().to_owned();
    for _ in 0..5 {
        assert_eq!(
            z.post("/api/login/totp", json!({ "ticket": t3, "code": "111111" }))
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    let late = code_for(&secret, step + 1);
    let _ = late;
    assert_eq!(
        z.post(
            "/api/login/totp",
            json!({ "ticket": t3, "code": code_for(&secret, step) })
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED,
        "ticket épuisé"
    );

    // Un ticket n'est valable que depuis l'adresse qui l'a obtenu.
    let (_, r) = login("127.0.0.1").await;
    let t4 = r.body["ticket"].as_str().unwrap().to_owned();
    let mut other = Client::from_ip(&s.base, "127.0.0.2");
    assert_eq!(
        other
            .post(
                "/api/login/totp",
                json!({ "ticket": t4, "code": code_for(&secret, step + 1) })
            )
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    // Code de secours : accepté une fois, puis plus jamais ; l'événement est journalisé.
    let (mut w, r) = login("127.0.0.1").await;
    let t5 = r.body["ticket"].as_str().unwrap().to_owned();
    let rc = recovery[0].to_lowercase().replace('-', " ");
    let r = w
        .post("/api/login/totp", json!({ "ticket": t5, "code": rc }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let (mut w2, r) = login("127.0.0.1").await;
    let t6 = r.body["ticket"].as_str().unwrap().to_owned();
    assert_eq!(
        w2.post(
            "/api/login/totp",
            json!({ "ticket": t6, "code": recovery[0] })
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(c.get("/api/account").await.body["recovery_remaining"], 9);

    // Désactivation : mot de passe ET code exigés. Le code du pas en cours a déjà servi (rejeu refusé) : on
    // utilise un code de secours, qui est aussi la voie normale quand le téléphone est perdu.
    let replayed = code_for(&secret, step + 1);
    let nope = c
        .post(
            "/api/totp/disable",
            json!({ "password": PW, "code": replayed }),
        )
        .await;
    assert_eq!(nope.status, StatusCode::FORBIDDEN, "code déjà utilisé");
    let nope = c
        .post(
            "/api/totp/disable",
            json!({ "password": "mauvais-Mot-2-passe", "code": recovery[1] }),
        )
        .await;
    assert_eq!(nope.status, StatusCode::FORBIDDEN, "mauvais mot de passe");
    let ok = c
        .post(
            "/api/totp/disable",
            json!({ "password": PW, "code": recovery[1] }),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(c.get("/api/account").await.body["totp_enabled"], false);
    let (_, r) = login("127.0.0.1").await;
    assert!(
        r.body["needs_totp"].is_null(),
        "plus de double authentification : connexion directe"
    );

    // Le journal raconte tout, sans aucun secret.
    let log = c.get("/api/audit?limit=200").await.body["items"].to_string();
    for t in [
        "Double authentification activée",
        "Code de double authentification refusé",
        "code de secours",
        "Double authentification désactivée",
    ] {
        assert!(log.contains(t), "{t} absent du journal");
    }
    assert!(
        !log.contains(&data_encoding::BASE32_NOPAD.encode(&secret)) && !log.contains(&recovery[0])
    );
}

#[tokio::test]
async fn repeated_wrong_codes_lock_the_account_even_across_new_tickets() {
    use master::admin::totp;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let setup = c.post("/api/totp/setup", json!({})).await;
    let secret = data_encoding::BASE32_NOPAD
        .decode(setup.body["secret"].as_str().unwrap().as_bytes())
        .unwrap();
    let step = totp::step_of(master::store::unix_now());
    let code = format!("{:06}", totp::code_at(&secret, step));
    assert_eq!(
        c.post("/api/totp/enable", json!({ "code": code }))
            .await
            .status,
        StatusCode::OK
    );

    // Un attaquant qui connaît le mot de passe enchaîne les tickets : le verrouillage par adresse l'arrête.
    let mut blocked = false;
    for _ in 0..12 {
        let mut x = Client::new(&s.base);
        let r = x
            .post("/api/login", json!({ "username": "alice", "password": PW }))
            .await;
        if r.status == StatusCode::TOO_MANY_REQUESTS {
            blocked = true;
            break;
        }
        let t = r.body["ticket"].as_str().unwrap().to_owned();
        let r = x
            .post("/api/login/totp", json!({ "ticket": t, "code": "000000" }))
            .await;
        if r.status == StatusCode::TOO_MANY_REQUESTS {
            blocked = true;
            break;
        }
    }
    assert!(blocked, "le verrouillage doit s'enclencher avant 12 essais");
    // Même le bon mot de passe est alors refusé temporairement.
    let mut x = Client::new(&s.base);
    let r = x
        .post("/api/login", json!({ "username": "alice", "password": PW }))
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn agent_install_script_is_generated_validated_and_never_logged() {
    let s = start(AdminSettings {
        agent_port: 7443,
        agent_hosts: vec!["master.exemple.org".into()],
        ..AdminSettings::default()
    })
    .await;
    let mut c = setup_and_login(&s).await;
    let body = |over: Value| {
        let mut b = json!({
            "master_addr": "master.exemple.org:7443", "client": "transmission",
            "client_url": "http://127.0.0.1:9091", "client_user": "admin", "root": "/srv/downloads",
            "name": "Seedbox Hetzner", "labels": ["ssd"],
        });
        for (k, v) in over.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };

    let info = c.get("/api/info").await.body;
    assert_eq!(info["agent_port"], 7443);
    assert_eq!(info["agent_hosts"], json!(["master.exemple.org"]));
    assert_eq!(info["binary_configured"], false);

    let r = c.post("/api/agent-install", body(json!({}))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let script = r.body["script"].as_str().unwrap();
    let cmd = r.body["command"].as_str().unwrap();
    assert!(script.starts_with("#!/bin/sh") && !script.contains("@@"));
    assert!(cmd.starts_with("sudo sh -c \"$(echo '") && cmd.contains("| base64 -d)\""));
    // La commande contient bien le script (base64) : pas de binaire ni d'URL externe cachés.
    let b64 = cmd.split('\'').nth(1).unwrap();
    assert_eq!(
        data_encoding::BASE64.decode(b64.as_bytes()).unwrap(),
        script.as_bytes()
    );
    let code = script
        .lines()
        .find_map(|l| l.strip_prefix("CODE='"))
        .unwrap()
        .trim_end_matches('\'')
        .to_owned();
    assert!(code.starts_with("CODE-"));

    // Le code est un vrai code d'appairage : un agent peut s'appairer avec, avec le nom prévu.
    let dir = tempfile::tempdir().unwrap();
    pair(PairOptions {
        addr: s.agent_addr.clone(),
        code: code.clone(),
        data_dir: dir.path().to_owned(),
        hostname: "vps".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap();
    let n = c.get("/api/nodes").await.body[0].clone();
    assert_eq!(
        (n["name"].as_str().unwrap(), n["labels"].clone()),
        ("Seedbox Hetzner", json!(["ssd"]))
    );

    // Entrées dangereuses ou invalides : refusées, et AUCUN code n'est créé pour elles.
    let before = s
        .core
        .store
        .audit_actions()
        .unwrap()
        .iter()
        .filter(|a| *a == "pairing_code_created")
        .count();
    for bad in [
        json!({ "client_url": "http://192.168.1.10:9091" }),
        json!({ "client_url": "http://127.0.0.1:9091/';id;'" }),
        json!({ "root": "/srv/../etc" }),
        json!({ "root": "/srv/x'; rm -rf / #" }),
        json!({ "master_addr": "master.exemple.org" }),
        json!({ "master_addr": "a b:7443" }),
        json!({ "client_user": "adm'in" }),
        json!({ "client": "emule" }),
        json!({ "labels": ["a b"] }),
    ] {
        let r = c.post("/api/agent-install", body(bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.body);
    }
    let after = s
        .core
        .store
        .audit_actions()
        .unwrap()
        .iter()
        .filter(|a| *a == "pairing_code_created")
        .count();
    assert_eq!(
        before, after,
        "aucune requête refusée ne crée de code d'appairage"
    );

    // Journal : l'événement est décrit, sans le code.
    let log = c.get("/api/audit?limit=100").await.body["items"].to_string();
    assert!(log.contains("Script d'installation d'un agent généré"));
    assert!(
        !log.contains(&code),
        "le code d'appairage ne doit jamais figurer dans le journal"
    );

    let mut anon = Client::new(&s.base);
    assert_eq!(anon.get("/api/info").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        anon.post("/api/agent-install", body(json!({})))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn agent_install_embeds_the_verified_binary_download_when_configured() {
    let sha = "ab".repeat(32);
    let s = start(AdminSettings {
        agent_binary_url: "https://exemple.org/nxt-agent-linux-amd64".into(),
        agent_binary_sha256: sha.clone(),
        ..AdminSettings::default()
    })
    .await;
    let mut c = setup_and_login(&s).await;
    let r = c
        .post(
            "/api/agent-install",
            json!({ "master_addr": "m.example.org:7443", "client": "qbittorrent", "client_url": "http://127.0.0.1:8080", "root": "/data" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let script = r.body["script"].as_str().unwrap();
    assert!(script.contains("BIN_URL='https://exemple.org/nxt-agent-linux-amd64'"));
    assert!(script.contains(&format!("BIN_SHA256='{sha}'")));
    assert_eq!(r.body["binary_configured"], true);
}

#[tokio::test]
async fn alert_channels_are_secret_tested_and_the_engine_notifies_once_then_resolves() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let sink = testkit::webhook::WebhookSink::start().await;
    let secret_url = format!("{}?token=TOP-SECRET-TOKEN", sink.url());

    // Entrées invalides.
    for bad in [
        json!({ "kind": "sms", "url": secret_url }),
        json!({ "kind": "webhook", "url": "ftp://x.org/a" }),
        json!({ "kind": "webhook", "url": "pas une url" }),
    ] {
        assert_eq!(
            c.post("/api/alerts/channels", bad).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    // Ajout : l'URL n'est JAMAIS renvoyée (elle peut contenir un secret), seul l'hôte l'est.
    let r = c
        .post(
            "/api/alerts/channels",
            json!({ "kind": "webhook", "url": secret_url, "label": "Mon relais" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = r.body["id"].as_str().unwrap().to_owned();
    let listing = c.get("/api/alerts").await;
    let text = listing.body.to_string();
    assert!(
        !text.contains("TOP-SECRET-TOKEN") && !text.contains("token="),
        "secret exposé : {text}"
    );
    assert_eq!(
        listing.body["channels"][0]["hint"],
        format!("http://{}/…", sink.addr)
    );
    assert_eq!(listing.body["channels"][0]["label"], "Mon relais");
    assert_eq!(
        listing.body["rules"]["node_offline"]["enabled"], true,
        "règles par défaut"
    );

    // Test d'envoi : le récepteur reçoit un vrai message.
    assert_eq!(
        c.post(&format!("/api/alerts/channels/{id}/test"), json!({}))
            .await
            .status,
        StatusCode::OK
    );
    assert!(sink.bodies().iter().any(|b| b.contains("Test d'alerte")));
    // Un canal injoignable : erreur claire, jamais l'URL.
    let dead = c
        .post(
            "/api/alerts/channels",
            json!({ "kind": "webhook", "url": "http://127.0.0.1:9/x?k=SECRET2" }),
        )
        .await;
    let dead_id = dead.body["id"].as_str().unwrap().to_owned();
    let t = c
        .post(&format!("/api/alerts/channels/{dead_id}/test"), json!({}))
        .await;
    assert_eq!(t.status, StatusCode::BAD_GATEWAY);
    assert!(!t.body.to_string().contains("SECRET2"));
    assert_eq!(
        c.call(
            Method::DELETE,
            &format!("/api/alerts/channels/{dead_id}"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );

    // Règles : validées.
    let mut rules = c.get("/api/alerts").await.body["rules"].clone();
    rules["node_offline"]["after_secs"] = json!(5);
    assert_eq!(
        c.post("/api/alerts/rules", rules.clone()).await.status,
        StatusCode::BAD_REQUEST
    );
    rules["node_offline"]["after_secs"] = json!(30);
    assert_eq!(
        c.post("/api/alerts/rules", rules).await.status,
        StatusCode::OK
    );

    // Moteur : un nœud hors ligne depuis assez longtemps déclenche UNE alerte, puis un rétablissement.
    s.core
        .store
        .insert_node(&master::store::NodeRecord {
            id: "11111111-2222-4333-8444-555555555555".into(),
            hostname: "nas".into(),
            agent_version: "v".into(),
            labels: vec![],
            name: Some("NAS du salon".into()),
        })
        .unwrap();
    let nid = "11111111-2222-4333-8444-555555555555";
    s.core.store.set_state(nid, "Offline", 0).unwrap();
    let wait = || tokio::time::sleep(Duration::from_millis(400));
    let before = sink.bodies().len();
    s.core.alert_tick_at(1_000).await;
    s.core.alert_tick_at(1_020).await; // moins de 30 s : pas encore
    wait().await;
    assert_eq!(
        sink.bodies().len(),
        before,
        "délai de confirmation non écoulé"
    );
    s.core.alert_tick_at(1_031).await;
    wait().await;
    let got: Vec<String> = sink.bodies()[before..].to_vec();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].contains("\"state\":\"firing\"")
            && got[0].contains("NAS du salon")
            && got[0].contains("hors ligne")
    );
    s.core.alert_tick_at(1_500).await;
    wait().await;
    assert_eq!(
        sink.bodies().len(),
        before + 1,
        "pas de répétition tant que ça dure"
    );
    // Rétabli.
    s.core.store.set_state(nid, "Online", 0).unwrap();
    s.core.alert_tick_at(1_600).await;
    wait().await;
    assert!(sink
        .bodies()
        .last()
        .unwrap()
        .contains("\"state\":\"resolved\""));
    // Canal désactivé : plus rien n'est envoyé.
    assert_eq!(
        c.post(
            &format!("/api/alerts/channels/{id}/enabled"),
            json!({ "enabled": false })
        )
        .await
        .status,
        StatusCode::OK
    );
    s.core.store.set_state(nid, "Offline", 0).unwrap();
    let n = sink.bodies().len();
    s.core.alert_tick_at(2_000).await;
    s.core.alert_tick_at(2_040).await;
    wait().await;
    assert_eq!(sink.bodies().len(), n, "canal désactivé");

    // Le journal raconte les alertes, sans aucun secret.
    let log = c.get("/api/audit?limit=200").await.body["items"].to_string();
    for t in [
        "Alerte : Le nœud « NAS du salon » est hors ligne",
        "Rétabli : Le nœud « NAS du salon » est hors ligne",
        "Canal de notification ajouté",
    ] {
        assert!(log.contains(t), "{t} absent du journal");
    }
    assert!(!log.contains("TOP-SECRET-TOKEN") && !log.contains("SECRET2"));

    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/alerts").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.post(
            "/api/alerts/channels",
            json!({ "kind": "webhook", "url": "http://x.org/a" })
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn node_speed_limits_api_validates_stores_and_audits() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let code = c.post("/api/pairing-codes", json!({})).await.body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = tempfile::tempdir().unwrap();
    let node_id = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "h".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;
    let n = c.get("/api/nodes").await.body[0].clone();
    assert_eq!(
        (n["limits_managed"].as_bool(), n["dl_limit"].as_u64()),
        (Some(false), Some(0)),
        "rien de géré par défaut"
    );
    let url = format!("/api/nodes/{node_id}/limits");
    let r = c
        .post(
            &url,
            json!({ "dl_limit": 5_000_000u64, "ul_limit": 1_000_000u64 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body["applied"], false,
        "nœud hors ligne : appliqué à la prochaine connexion"
    );
    let n = c.get("/api/nodes").await.body[0].clone();
    assert_eq!(
        (
            n["limits_managed"].as_bool(),
            n["dl_limit"].as_u64(),
            n["ul_limit"].as_u64()
        ),
        (Some(true), Some(5_000_000), Some(1_000_000))
    );
    assert_eq!(
        c.post(&url, json!({ "dl_limit": u64::MAX, "ul_limit": 0 }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.post(
            "/api/nodes/00000000-0000-4000-8000-000000000000/limits",
            json!({ "dl_limit": 1, "ul_limit": 1 })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let log = c.get("/api/audit?action=node_limits_changed").await.body;
    assert!(log["items"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("↓ 5 Mo/s"));
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.post(&url, json!({ "dl_limit": 1, "ul_limit": 1 }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Revérification : un torrent inconnu est refusé, sans nœud connecté aussi.
    let h = "1".repeat(40);
    c.post("/api/torrents", json!({ "infohash": h })).await;
    let r = c
        .post(
            "/api/torrents/bulk",
            json!({ "action": "recheck", "infohashes": [h] }),
        )
        .await;
    assert_eq!(
        (r.body["done"].as_u64(), r.body["failed"].as_u64()),
        (Some(0), Some(1))
    );
}

#[tokio::test]
async fn backup_download_needs_password_and_second_factor_and_restores_elsewhere() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let pass = "une phrase de passe solide";
    let get = |c: &Client, body: Value| {
        let mut r = c
            .http
            .post(format!("{}/api/backup", s.base))
            .header("X-Requested-With", "nxt")
            .json(&body);
        if let Some(ck) = &c.cookie {
            r = r.header("Cookie", ck);
        }
        r
    };
    // Mauvais mot de passe / phrase trop courte : refusé.
    assert_eq!(
        get(
            &c,
            json!({ "password": "faux-Mot-2-passe", "passphrase": pass })
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&c, json!({ "password": PW, "passphrase": "court" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let anon = Client::new(&s.base);
    assert_eq!(
        get(&anon, json!({ "password": PW, "passphrase": pass }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let r = get(&c, json!({ "password": PW, "passphrase": pass }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .contains("nxttorrent-backup-"));
    let bytes = r.bytes().await.unwrap().to_vec();
    // Opaque : ni la clé de CA ni le nom d'utilisateur en clair.
    assert!(
        !bytes.windows(11).any(|w| w == b"PRIVATE KEY") && !bytes.windows(5).any(|w| w == b"alice")
    );
    // Restaurable ailleurs : le compte administrateur est revenu.
    let out = tempfile::tempdir().unwrap();
    master::backup::restore(out.path(), &bytes, pass, false).unwrap();
    let restored = Store::open(&out.path().join("master.db")).unwrap();
    assert!(restored.admin_hash("alice").unwrap().is_some());
    assert!(out.path().join("ca.key").exists());
    assert!(master::backup::restore(out.path(), &bytes, "autre phrase de passe", true).is_err());

    // Avec la double authentification : le mot de passe seul ne suffit plus.
    let setup = c.post("/api/totp/setup", json!({})).await;
    let secret = data_encoding::BASE32_NOPAD
        .decode(setup.body["secret"].as_str().unwrap().as_bytes())
        .unwrap();
    let step = master::admin::totp::step_of(master::store::unix_now());
    let code = |st: i64| format!("{:06}", master::admin::totp::code_at(&secret, st));
    assert_eq!(
        c.post("/api/totp/enable", json!({ "code": code(step) }))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        get(&c, json!({ "password": PW, "passphrase": pass }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN,
        "code absent"
    );
    assert_eq!(
        get(
            &c,
            json!({ "password": PW, "code": code(step + 1), "passphrase": pass })
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    let log = c.get("/api/audit?limit=100").await.body["items"].to_string();
    assert!(
        log.contains("Sauvegarde chiffrée téléchargée")
            && log.contains("Téléchargement de sauvegarde refusé")
    );
    assert!(!log.contains(pass));
}

// ---------- Ligne de commande ----------

fn master(data: &std::path::Path, args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_master"));
    c.arg("--data-dir").arg(data).args(args);
    c
}

#[test]
fn cli_set_password_enforces_policy_and_creates_a_working_account() {
    let d = tempfile::tempdir().unwrap();
    let weak = master(d.path(), &["admin", "set-password", "--username", "root"])
        .env("MASTER_ADMIN_PASSWORD", "court")
        .output()
        .unwrap();
    assert!(!weak.status.success(), "mot de passe faible refusé");
    assert!(String::from_utf8_lossy(&weak.stderr).contains("trop court"));

    let ok = master(d.path(), &["admin", "set-password", "--username", "root"])
        .env("MASTER_ADMIN_PASSWORD", PW)
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let store = Store::open(&d.path().join("master.db")).unwrap();
    let h = store.admin_hash("root").unwrap().unwrap();
    assert!(master::admin::password::verify(PW, &h));
    assert!(store
        .audit_actions()
        .unwrap()
        .contains(&"admin_password_set".to_owned()));
}

#[test]
fn cli_backup_and_restore_round_trip_and_refuse_to_overwrite() {
    let d = tempfile::tempdir().unwrap();
    let ok = master(d.path(), &["admin", "set-password", "--username", "root"])
        .env("MASTER_ADMIN_PASSWORD", PW)
        .output()
        .unwrap();
    assert!(ok.status.success());
    // Le dossier de données du master existe déjà avec une CA : on en crée une pour que la sauvegarde soit complète.
    Ca::load_or_create(d.path()).unwrap();
    let file = d.path().join("sauvegarde.nxtbak");
    let pass = "une phrase de passe solide";
    let out = master(
        d.path(),
        &["admin", "backup", "--out", file.to_str().unwrap()],
    )
    .env("MASTER_BACKUP_PASSPHRASE", pass)
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(file.exists());
    // Écraser une sauvegarde existante est refusé.
    let again = master(
        d.path(),
        &["admin", "backup", "--out", file.to_str().unwrap()],
    )
    .env("MASTER_BACKUP_PASSPHRASE", pass)
    .output()
    .unwrap();
    assert!(!again.status.success());
    // Mauvaise phrase de passe : échec sans rien écrire. Bonne phrase : restauration dans un dossier neuf.
    let target = tempfile::tempdir().unwrap();
    let bad = master(
        target.path(),
        &["admin", "restore", "--from", file.to_str().unwrap()],
    )
    .env("MASTER_BACKUP_PASSPHRASE", "mauvaise phrase de passe")
    .output()
    .unwrap();
    assert!(!bad.status.success() && !target.path().join("master.db").exists());
    let good = master(
        target.path(),
        &["admin", "restore", "--from", file.to_str().unwrap()],
    )
    .env("MASTER_BACKUP_PASSPHRASE", pass)
    .output()
    .unwrap();
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    let restored = Store::open(&target.path().join("master.db")).unwrap();
    assert!(master::admin::password::verify(
        PW,
        &restored.admin_hash("root").unwrap().unwrap()
    ));
    // Un dossier qui contient déjà des données n'est pas écrasé sans --force.
    let refuse = master(
        target.path(),
        &["admin", "restore", "--from", file.to_str().unwrap()],
    )
    .env("MASTER_BACKUP_PASSPHRASE", pass)
    .output()
    .unwrap();
    assert!(!refuse.status.success());
    assert!(String::from_utf8_lossy(&refuse.stderr).contains("--force"));
}

#[test]
fn serving_the_admin_api_on_a_non_local_address_is_refused_by_default() {
    let d = tempfile::tempdir().unwrap();
    let out = master(
        d.path(),
        &[
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--san",
            "127.0.0.1",
            "--admin-listen",
            "0.0.0.0:0",
        ],
    )
    .output()
    .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("boucle locale"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Régression : une variable d'environnement booléenne `=1` (celle que documentent l'image Docker et
/// l'ADR) faisait échouer le démarrage ; `--admin-allow-remote` seul doit aussi continuer de marcher.
#[test]
fn allow_remote_flag_accepts_1_true_and_the_bare_flag() {
    for (env, flag) in [
        (Some("1"), false),
        (Some("true"), false),
        (Some("yes"), false),
        (None, true),
    ] {
        let d = tempfile::tempdir().unwrap();
        let mut c = master(
            d.path(),
            &[
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--san",
                "127.0.0.1",
                "--admin-listen",
                "0.0.0.0:0",
            ],
        );
        if let Some(v) = env {
            c.env("MASTER_ADMIN_ALLOW_REMOTE", v);
        }
        if flag {
            c.arg("--admin-allow-remote");
        }
        let mut child = c
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            running,
            "le master aurait dû démarrer (env={env:?}, flag={flag})"
        );
    }
    // `false` / absent : toujours refusé.
    let d = tempfile::tempdir().unwrap();
    let out = master(
        d.path(),
        &[
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--san",
            "127.0.0.1",
            "--admin-listen",
            "0.0.0.0:0",
        ],
    )
    .env("MASTER_ADMIN_ALLOW_REMOTE", "0")
    .output()
    .unwrap();
    assert!(!out.status.success());
}

// ---------- Interface web ----------

#[tokio::test]
async fn me_reports_the_session_user() {
    let s = start(AdminSettings::default()).await;
    let mut c = Client::new(&s.base);
    c.post("/api/setup", json!({ "username": "alice", "password": PW }))
        .await;
    assert_eq!(c.get("/api/me").await.status, StatusCode::UNAUTHORIZED);
    c.login("alice", PW).await;
    assert_eq!(c.get("/api/me").await.body["username"], "alice");
}

#[tokio::test]
async fn web_ui_is_served_with_a_strict_csp_and_safe_routing() {
    let s = start(AdminSettings::default()).await;
    let http = reqwest::Client::new();
    let get = |p: &str| http.get(format!("{}{p}", s.base)).send();

    let r = get("/").await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        csp.contains("script-src 'self'") && csp.contains("style-src 'self'"),
        "{csp}"
    );
    assert!(
        !csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"),
        "CSP trop permissive : {csp}"
    );
    assert!(csp.contains("frame-ancestors 'none'") && csp.contains("default-src 'none'"));
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    let html = r.text().await.unwrap();
    assert!(!html.contains("<script>"), "aucun script en ligne");

    // Routes de l'application (sans extension) : l'application elle-même, pour que l'actualisation fonctionne.
    for p in ["/torrents", "/nodes", "/audit"] {
        let r = get(p).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{p}");
        assert_eq!(r.text().await.unwrap(), html, "{p}");
    }
    // Une route d'API inconnue est un 404 JSON, jamais une page HTML.
    let r = get("/api/inexistant").await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .contains("json"));
    // Fichier statique inconnu : 404.
    assert_eq!(
        get("/assets/absent-123.js").await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    // Aucune évasion de chemin : seuls les fichiers embarqués sont servis.
    for p in [
        "/../Cargo.toml",
        "/%2e%2e/%2e%2e/Cargo.toml",
        "/assets/..%2f..%2fCargo.toml",
        "/..%5cCargo.toml",
    ] {
        let r = get(p).await.unwrap();
        let body = r.text().await.unwrap_or_default();
        assert!(
            !body.contains("[workspace]") && !body.contains("[package]"),
            "{p} a servi un fichier hors de l'interface"
        );
    }
    // Une requête qui modifie l'état sans l'en-tête anti-CSRF est refusée même sur les chemins statiques.
    assert_eq!(
        http.post(format!("{}/", s.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    // Si l'interface est construite : ses fichiers sont servis avec le bon type et un cache longue durée.
    if let Some(i) = html.find("/assets/") {
        let path: String = html[i..].chars().take_while(|c| *c != '"').collect();
        let js = get(&path).await.unwrap();
        assert_eq!(js.status(), StatusCode::OK, "{path}");
        assert!(js.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable"));
    } else {
        assert!(
            html.contains("interface web"),
            "page de secours attendue quand l'interface n'est pas construite"
        );
    }
}

/// Régression : dans Docker (API ouverte au réseau, accès en HTTP simple) le cookie était marqué `Secure`
/// et n'était plus renvoyé par certains clients : impossible de rester connecté.
#[tokio::test]
async fn session_cookie_is_secure_only_when_served_over_https_behind_a_proxy() {
    let s = start(AdminSettings {
        allow_remote: true,
        ..AdminSettings::default()
    })
    .await;
    let mut c = Client::new(&s.base);
    c.post("/api/setup", json!({ "username": "alice", "password": PW }))
        .await;

    let plain = c.login("alice", PW).await;
    let cookie = plain.headers["set-cookie"].to_str().unwrap().to_owned();
    assert!(
        !cookie.contains("Secure"),
        "HTTP simple : pas de Secure ({cookie})"
    );

    let http = reqwest::Client::new();
    let proxied = http
        .post(format!("{}/api/login", s.base))
        .header("X-Requested-With", "nxt")
        .header("X-Forwarded-Proto", "https")
        .json(&json!({ "username": "alice", "password": PW }))
        .send()
        .await
        .unwrap();
    let cookie = proxied.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(
        cookie.contains("; Secure")
            && cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict"),
        "{cookie}"
    );
}

#[tokio::test]
async fn replication_settings_are_validated_stored_audited_and_reflect_the_tracker() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    // Sans tracker : désactivée ; un torrent à copier fait apparaître l'avertissement côté interface.
    let r = c.get("/api/replication").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        (
            r.body["enabled"].as_bool(),
            r.body["copy_dl_limit"].as_u64(),
            r.body["with_group"].as_u64()
        ),
        (Some(false), Some(0), Some(0))
    );
    c.post(
        "/api/torrents",
        json!({ "infohash": "2".repeat(40), "replicate_group": "mirror" }),
    )
    .await;
    let r = c.get("/api/replication").await;
    assert_eq!(r.body["with_group"], 1);
    assert_eq!(
        r.body["magnet_waiting"], 0,
        "sans tracker, personne n'attend de .torrent"
    );
    let row = c.get("/api/torrents").await.body["items"][0].clone();
    assert_eq!(
        row["replication"], "shared",
        "fichiers supposés présents sans tracker"
    );

    // Plafond : enregistré, relu, borné, journalisé, réservé aux sessions.
    let r = c
        .post("/api/replication", json!({ "copy_dl_limit": 5_000_000u64 }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        c.get("/api/replication").await.body["copy_dl_limit"],
        5_000_000
    );
    assert_eq!(
        c.post("/api/replication", json!({ "copy_dl_limit": u64::MAX }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.get("/api/replication").await.body["copy_dl_limit"],
        5_000_000,
        "refus = inchangé"
    );
    let log = c
        .get("/api/audit?action=replication_limit_changed")
        .await
        .body;
    assert!(
        log["items"][0]["title"]
            .as_str()
            .unwrap()
            .contains("5 Mo/s"),
        "{log}"
    );
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/replication").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.post("/api/replication", json!({ "copy_dl_limit": 1 }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cross_seed_api_validates_hides_the_key_audits_and_groups_the_list() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let idx = testkit::torznab::MockTorznab::start("clé-secrète-42").await;

    // État initial : simulation par défaut, aucun indexeur.
    let r = c.get("/api/crossseed").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["mode"], "dry");
    assert_eq!(r.body["indexers"].as_array().unwrap().len(), 0);

    // Test de configuration : bonne clé / mauvaise clé, sans rien enregistrer.
    let ok = c
        .post(
            "/api/crossseed/indexers/test",
            json!({ "name": "Mock", "url": idx.url(), "api_key": "clé-secrète-42" }),
        )
        .await;
    assert_eq!(
        (ok.status, ok.body["ok"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{}",
        ok.body
    );
    // Deux tests d'affilée : refusés avec Retry-After (chaque essai est une tentative d'accès chez l'indexeur).
    let too_fast = c
        .post(
            "/api/crossseed/indexers/test",
            json!({ "name": "Mock", "url": idx.url(), "api_key": "clé-secrète-42" }),
        )
        .await;
    assert_eq!(
        too_fast.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        too_fast.body
    );
    assert_eq!(
        idx.requests(),
        1,
        "le test refusé n'a rien envoyé à l'indexeur"
    );
    s.core.xseed_reset_test_guard();
    let bad = c
        .post(
            "/api/crossseed/indexers/test",
            json!({ "name": "Mock", "url": idx.url(), "api_key": "mauvaise" }),
        )
        .await;
    assert_eq!(bad.body["ok"], false);
    assert!(
        !bad.body.to_string().contains("mauvaise"),
        "la clé n'est jamais renvoyée"
    );
    // Après un accès refusé : on n'insiste pas (30 s), pour ne pas faire bannir le compte.
    let again = c
        .post(
            "/api/crossseed/indexers/test",
            json!({ "name": "Mock", "url": idx.url(), "api_key": "mauvaise" }),
        )
        .await;
    assert_eq!(again.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(idx.requests(), 2, "toujours rien d'envoyé");
    s.core.xseed_reset_test_guard();
    assert_eq!(
        c.get("/api/crossseed").await.body["indexers"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // Validation : URL avec identifiants / requête / schéma, nom ou clé vides.
    for body in [
        json!({ "name": "x", "url": "http://u:p@h/", "api_key": "k" }),
        json!({ "name": "x", "url": "http://h/?apikey=k", "api_key": "k" }),
        json!({ "name": "x", "url": "ftp://h/", "api_key": "k" }),
        json!({ "name": " ", "url": "http://h/", "api_key": "k" }),
        json!({ "name": "x", "url": "http://h/", "api_key": " " }),
    ] {
        assert_eq!(
            c.post("/api/crossseed/indexers", body.clone()).await.status,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }

    // Ajout : la clé n'est JAMAIS renvoyée, ni en liste, ni dans le journal ; elle est chiffrée en base.
    let r = c
        .post(
            "/api/crossseed/indexers",
            json!({ "name": "Mon indexeur", "url": idx.url(), "api_key": "clé-secrète-42" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = r.body["id"].as_str().unwrap().to_owned();
    let all = c.get("/api/crossseed").await;
    assert!(!all.body.to_string().contains("clé-secrète-42"));
    assert_eq!(all.body["indexers"][0]["name"], "Mon indexeur");
    assert_eq!(
        s.core.store.indexer_key(&id).unwrap().as_deref(),
        Some("clé-secrète-42")
    );
    let audit = c.get("/api/audit?limit=50").await.body.to_string();
    assert!(
        !audit.contains("clé-secrète-42") && !audit.contains(&idx.url()),
        "ni clé ni URL dans le journal"
    );
    assert!(audit.contains("Mon indexeur"));

    // Activation / suppression d'un indexeur inconnu : 404.
    assert_eq!(
        c.post(
            "/api/crossseed/indexers/inconnu",
            json!({ "enabled": false })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.post(
            &format!("/api/crossseed/indexers/{id}"),
            json!({ "enabled": false })
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        c.get("/api/crossseed").await.body["indexers"][0]["enabled"],
        false
    );

    // Mode : borné, journalisé en clair ; lancement refusé si désactivé.
    assert_eq!(
        c.post("/api/crossseed/mode", json!({ "mode": "turbo" }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.post("/api/crossseed/mode", json!({ "mode": "off" }))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        c.post("/api/crossseed/run", json!({})).await.status,
        StatusCode::CONFLICT
    );
    c.post("/api/crossseed/mode", json!({ "mode": "active" }))
        .await;
    let log = c.get("/api/audit?action=xseed_mode_changed").await.body;
    assert!(
        log["items"][0]["title"]
            .as_str()
            .unwrap()
            .contains("Multi-trackers"),
        "{log}"
    );

    // Réservé aux sessions.
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/crossseed").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.post("/api/crossseed/mode", json!({ "mode": "off" }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.post("/api/crossseed/run", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Suppression.
    let r = c
        .post(
            &format!("/api/crossseed/indexers/{id}"),
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let del = c
        .call(
            Method::DELETE,
            &format!("/api/crossseed/indexers/{id}"),
            None,
        )
        .await;
    assert_eq!(del.status, StatusCode::OK);
    assert_eq!(
        c.get("/api/crossseed").await.body["indexers"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn cross_seeded_torrents_show_as_one_row_and_actions_apply_to_the_whole_group() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    c.post("/api/torrents", json!({ "infohash": a, "name": "Film" }))
        .await;
    c.post(
        "/api/torrents",
        json!({ "infohash": b, "name": "Film (autre tracker)" }),
    )
    .await;
    s.core.store.set_xseed_of(&b, &a).unwrap();

    // Une seule ligne par contenu ; le membre est dans la ligne, pas à côté.
    let list = c.get("/api/torrents").await.body;
    assert_eq!(list["total"], 1, "{list}");
    let row = &list["items"][0];
    assert_eq!(row["infohash"], a.as_str());
    assert_eq!(row["members"][0]["infohash"], b.as_str());
    assert_eq!(row["members"][0]["name"], "Film (autre tracker)");
    // La recherche par infohash du membre retrouve la ligne du contenu.
    let found = c.get(&format!("/api/torrents?q={}", &b[..10])).await.body;
    assert_eq!(found["items"][0]["infohash"], a.as_str());

    // Pause de la ligne : s'applique aux deux. Suppression : les deux, sans fichiers pour le membre.
    let r = c
        .post(
            "/api/torrents/bulk",
            json!({ "action": "pause", "infohashes": [a] }),
        )
        .await;
    assert_eq!(r.body["done"], 1);
    assert!(s.core.store.get_torrent(&a).unwrap().unwrap().paused);
    assert!(
        s.core.store.get_torrent(&b).unwrap().unwrap().paused,
        "le membre suit"
    );
    let r = c
        .post(
            "/api/torrents/bulk",
            json!({ "action": "remove", "infohashes": [a], "delete_files": true }),
        )
        .await;
    assert_eq!(r.body["done"], 1);
    assert!(s.core.store.get_torrent(&a).unwrap().unwrap().deleted);
    assert!(
        s.core.store.get_torrent(&b).unwrap().unwrap().deleted,
        "le membre part avec la ligne"
    );
    assert_eq!(c.get("/api/torrents").await.body["total"], 0);
}

#[tokio::test]
async fn indexer_rate_limit_settings_are_bounded_conservative_by_default_and_audited() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    // Défauts conservateurs quand rien n'est précisé.
    let r = c
        .post(
            "/api/crossseed/indexers",
            json!({ "name": "A", "url": "http://idx.test:9696/1", "api_key": "k" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = r.body["id"].as_str().unwrap().to_owned();
    let ix = c.get("/api/crossseed").await.body["indexers"][0].clone();
    assert_eq!(
        (
            ix["min_interval_ms"].as_u64(),
            ix["max_per_hour"].as_u64(),
            ix["max_per_day"].as_u64(),
            ix["max_candidates"].as_u64(),
            ix["state"].as_str()
        ),
        (Some(3000), Some(60), Some(500), Some(10), Some("ok")),
        "{ix}"
    );
    assert_eq!(
        (ix["used_hour"].as_u64(), ix["used_day"].as_u64()),
        (Some(0), Some(0))
    );

    // Réglage par indexeur : relu tel quel, journalisé lisiblement.
    let url = format!("/api/crossseed/indexers/{id}/limits");
    let r = c
        .post(
            &url,
            json!({ "min_interval_ms": 10_000, "max_per_hour": 20, "max_per_day": 150, "max_candidates": 5 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let ix = c.get("/api/crossseed").await.body["indexers"][0].clone();
    assert_eq!(
        (
            ix["min_interval_ms"].as_u64(),
            ix["max_per_hour"].as_u64(),
            ix["max_per_day"].as_u64(),
            ix["max_candidates"].as_u64()
        ),
        (Some(10_000), Some(20), Some(150), Some(5))
    );
    let log = c.get("/api/audit?action=xseed_indexer_limits").await.body;
    let item = &log["items"][0];
    assert!(
        item["detail"]
            .as_str()
            .unwrap()
            .contains("10.0 s entre deux requêtes"),
        "{item}"
    );
    assert!(
        item["detail"].as_str().unwrap().contains("20/heure"),
        "{item}"
    );

    // Bornes : refus net (un 0 ou une faute de frappe ne doit pas ouvrir les vannes ni bloquer l'indexeur).
    for body in [
        json!({ "min_interval_ms": 100 }),
        json!({ "min_interval_ms": 99_999_999u64 }),
        json!({ "max_per_hour": 0 }),
        json!({ "max_per_hour": 100_000 }),
        json!({ "max_per_day": 0 }),
        json!({ "max_per_day": 1_000_000 }),
        json!({ "max_candidates": 0 }),
        json!({ "max_candidates": 500 }),
    ] {
        assert_eq!(
            c.post(&url, body.clone()).await.status,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    assert_eq!(
        c.get("/api/crossseed").await.body["indexers"][0]["max_per_hour"],
        20,
        "un refus ne modifie rien"
    );
    // À l'ajout aussi, et un indexeur inconnu est refusé.
    assert_eq!(
        c.post(
            "/api/crossseed/indexers",
            json!({ "name": "B", "url": "http://idx.test/2", "api_key": "k", "max_per_hour": 0 })
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    let r = c
        .post(
            "/api/crossseed/indexers",
            json!({ "name": "B", "url": "http://idx.test/2", "api_key": "k", "min_interval_ms": 8000, "max_per_hour": 12 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let all = c.get("/api/crossseed").await.body["indexers"].clone();
    let by = all
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "B")
        .unwrap();
    assert_eq!(
        (
            by["min_interval_ms"].as_u64(),
            by["max_per_hour"].as_u64(),
            by["max_per_day"].as_u64()
        ),
        (Some(8000), Some(12), Some(500))
    );
    // On retire B : la suite ne s'intéresse qu'à A (l'ordre d'affichage de deux ajouts simultanés n'est pas garanti).
    let bid = by["id"].as_str().unwrap().to_owned();
    assert_eq!(
        c.call(
            Method::DELETE,
            &format!("/api/crossseed/indexers/{bid}"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        c.post(
            "/api/crossseed/indexers/inconnu/limits",
            json!({ "max_per_hour": 5 })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.post(&url, json!({ "max_per_hour": 5 })).await.status,
        StatusCode::UNAUTHORIZED
    );

    // État de santé exposé : en pause, puis désactivé automatiquement ; réactiver efface tout.
    let now = master::store::unix_now();
    s.core
        .store
        .indexer_failure(&id, "l'indexeur demande de ralentir", now + 600)
        .unwrap();
    let ix = c.get("/api/crossseed").await.body["indexers"][0].clone();
    assert_eq!(ix["state"], "paused");
    assert!(ix["paused_until"].as_i64().unwrap() > now);
    assert_eq!(
        (ix["fail_count"].as_u64(), ix["last_error"].as_str()),
        (Some(1), Some("l'indexeur demande de ralentir"))
    );
    s.core
        .store
        .indexer_auto_disable(&id, "HTTP 403 : désactivé automatiquement")
        .unwrap();
    let ix = c.get("/api/crossseed").await.body["indexers"][0].clone();
    assert_eq!(
        (ix["state"].as_str(), ix["enabled"].as_bool()),
        (Some("disabled"), Some(false))
    );
    assert!(ix["disabled_reason"]
        .as_str()
        .unwrap()
        .contains("automatiquement"));
    let r = c
        .post(
            &format!("/api/crossseed/indexers/{id}"),
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let ix = c.get("/api/crossseed").await.body["indexers"][0].clone();
    assert_eq!(ix["state"], "ok");
    assert_eq!(
        (
            ix["fail_count"].as_u64(),
            ix["disabled_reason"].as_str(),
            ix["paused_until"].as_i64()
        ),
        (Some(0), Some(""), Some(0))
    );
    // Ni l'adresse ni la clé ne sortent jamais dans l'API de santé.
    assert!(!c
        .get("/api/crossseed")
        .await
        .body
        .to_string()
        .contains("\"api_key\""));
}

#[tokio::test]
async fn torrent_creation_api_requires_a_session_validates_input_and_refuses_unusable_nodes() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let code = c.post("/api/pairing-codes", json!({})).await.body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = tempfile::tempdir().unwrap();
    let node = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "h".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id; // appairé mais SANS tunnel : hors ligne

    // Réservé aux sessions.
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get(&format!("/api/nodes/{node}/browse")).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.post("/api/torrent-jobs", json!({ "node_id": node, "path": "a" }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.get("/api/torrent-jobs").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.get("/api/torrent-jobs/x/file").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.call(Method::DELETE, "/api/torrent-jobs/x", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    // Entrées refusées AVANT toute sollicitation du nœud (400).
    let base = |extra: Value| {
        let mut b =
            json!({ "node_id": node, "path": "movies/Film", "trackers": [["https://t.test/a"]] });
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for (what, body) in [
        ("chemin vide", base(json!({ "path": "" }))),
        ("racine", base(json!({ "path": "." }))),
        ("traversée", base(json!({ "path": "../x" }))),
        ("absolu", base(json!({ "path": "/etc/passwd" }))),
        ("lecteur", base(json!({ "path": "C:\\Windows" }))),
        (
            "schéma ftp",
            base(json!({ "trackers": [["ftp://x.test/a"]] })),
        ),
        (
            "script",
            base(json!({ "trackers": [["javascript:alert(1)"]] })),
        ),
        ("palier vide", base(json!({ "trackers": [[]] }))),
        (
            "privé sans tracker",
            base(json!({ "private": true, "trackers": [] })),
        ),
        ("source sans privé", base(json!({ "source": "X" }))),
        ("taille de pièce", base(json!({ "piece_length": 1000 }))),
        (
            "commentaire multi-lignes",
            base(json!({ "comment": "a\nb" })),
        ),
    ] {
        let r = c.post("/api/torrent-jobs", body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what} : {}", r.body);
    }
    // Valide mais le nœud est hors ligne : 409. Nœud inconnu ou identifiant mal formé : 404.
    assert_eq!(
        c.post("/api/torrent-jobs", base(json!({}))).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        c.get(&format!("/api/nodes/{node}/browse?path=movies"))
            .await
            .status,
        StatusCode::CONFLICT
    );
    for bad in [
        "00000000-0000-4000-8000-000000000000",
        "pas-un-uuid",
        "..%2f..%2fx",
    ] {
        assert_eq!(
            c.get(&format!("/api/nodes/{bad}/browse")).await.status,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
    assert_eq!(
        c.post(
            "/api/torrent-jobs",
            json!({ "node_id": "00000000-0000-4000-8000-000000000000", "path": "a" })
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Tâches inconnues.
    assert_eq!(
        c.get("/api/torrent-jobs/inconnue").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get("/api/torrent-jobs/inconnue/file").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.call(Method::DELETE, "/api/torrent-jobs/inconnue", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get("/api/torrent-jobs").await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Aucune tâche n'a été créée par les refus ci-dessus, et le journal n'a rien retenu d'adresse de tracker.
    let audit = c.get("/api/audit?limit=100").await.body.to_string();
    assert!(
        !audit.contains("t.test") && !audit.contains("torrent_create_started"),
        "rien n'a démarré"
    );
}

#[tokio::test]
async fn the_created_torrent_is_downloaded_as_a_safe_uncached_attachment_and_can_be_forgotten() {
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    // Nom dangereux : guillemet, retour à la ligne, barres obliques, point initial.
    let bytes = testkit::torrent::sample("..\"evil\"\r\nname/x\\y.mkv", 4096, 3);
    s.core
        .insert_finished_job_for_tests("job1", "n1", bytes.clone());
    let list = c.get("/api/torrent-jobs").await.body;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(
        !list.to_string().contains("torrent_file"),
        "la liste ne contient jamais le fichier"
    );
    let v = c.get("/api/torrent-jobs/job1").await;
    assert_eq!(
        (v.body["state"].as_str(), v.body["size"].as_u64()),
        (Some("done"), Some(4096))
    );
    assert!(v.body["magnet"]
        .as_str()
        .unwrap()
        .starts_with("magnet:?xt=urn:btih:"));

    let f = c.get("/api/torrent-jobs/job1/file").await;
    assert_eq!(f.status, StatusCode::OK);
    assert_eq!(f.headers["content-type"], "application/x-bittorrent");
    assert_eq!(
        f.headers["cache-control"], "no-store",
        "un .torrent privé contient un passkey : jamais en cache"
    );
    let cd = f.headers["content-disposition"].to_str().unwrap();
    assert!(
        cd.starts_with("attachment; filename=\"") && cd.ends_with(".torrent\""),
        "{cd}"
    );
    let fname = cd
        .trim_start_matches("attachment; filename=\"")
        .trim_end_matches('"');
    assert!(
        !fname.contains(['"', '\r', '\n', '/', '\\']) && !fname.starts_with('.'),
        "nom assaini : {fname}"
    );
    assert_eq!(
        f.body.as_str().map(str::as_bytes),
        Some(bytes.as_slice()),
        "contenu exact"
    );

    // Oublier la tâche : plus de fichier, plus de vue.
    assert_eq!(
        c.call(Method::DELETE, "/api/torrent-jobs/job1", None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        c.get("/api/torrent-jobs/job1/file").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get("/api/torrent-jobs/job1").await.status,
        StatusCode::NOT_FOUND
    );
}

// ----- Statistiques : volumes, ratio, trackers, inactifs, disques, gain de Multi-trackers -----

/// Appaire un vrai nœud (il existe alors dans la base) et renvoie son identifiant.
async fn paired_node(s: &Srv, c: &mut Client) -> String {
    let code = c.post("/api/pairing-codes", json!({})).await.body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = tempfile::tempdir().unwrap();
    let id = pair(PairOptions {
        addr: s.agent_addr.clone(),
        code,
        data_dir: dir.path().to_owned(),
        hostname: "seedbox".into(),
        labels: vec![],
        force: false,
    })
    .await
    .unwrap()
    .node_id;
    // Le dossier de l'agent doit survivre à l'appairage uniquement le temps de l'appel.
    drop(dir);
    id
}

fn stat_torrent(
    h: &str,
    tracker: &str,
    up: u64,
    down: u64,
    status: proto::pb::TorrentStatus,
) -> proto::pb::TorrentInfo {
    proto::pb::TorrentInfo {
        infohash: h.repeat(40),
        name: format!("torrent {h}"),
        status: status as i32,
        progress: 1.0,
        size_bytes: 1000,
        tracker: tracker.into(),
        uploaded: up,
        downloaded: down,
        ..Default::default()
    }
}

fn set_state(core: &Core, node: &str, torrents: Vec<proto::pb::TorrentInfo>, disk_free: u64) {
    core.record(node, |t| {
        t.torrents = torrents;
        t.disk_free = disk_free;
        // Sans état complet reçu, le nœud est ignoré par la comptabilité (voir traffic.rs).
        t.full_states = 1;
    });
}

#[tokio::test]
async fn traffic_volumes_are_differences_of_the_clients_counters_per_tracker_and_node() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();

    // 1er relevé = ligne de base : les 9 Go envoyés AVANT la surveillance ne sont pas du trafic d'aujourd'hui.
    set_state(
        &s.core,
        &node,
        vec![
            stat_torrent("a", "alpha.example", 9_000_000_000, 500, Seeding),
            stat_torrent("b", "beta.example", 0, 0, Seeding),
        ],
        50_000_000_000,
    );
    s.core.sample_traffic_at(now - 7200).unwrap();
    let r = c.get("/api/stats/traffic?range=24h").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        r.body["totals"]["up"], 0,
        "la ligne de base n'est pas du trafic"
    );

    // Puis 4 000 octets envoyés sur alpha et 1 500 sur beta, 700 téléchargés sur beta.
    set_state(
        &s.core,
        &node,
        vec![
            stat_torrent("a", "alpha.example", 9_000_004_000, 500, Seeding),
            stat_torrent("b", "beta.example", 1_500, 700, Seeding),
        ],
        50_000_000_000,
    );
    s.core.sample_traffic_at(now - 3600).unwrap();

    let r = c.get("/api/stats/traffic?range=24h").await.body;
    assert_eq!(r["bucket"], "hour");
    assert_eq!(
        r["series"].as_array().unwrap().len(),
        24,
        "une tranche par heure, creuses comprises"
    );
    assert_eq!(
        (r["totals"]["up"].as_u64(), r["totals"]["down"].as_u64()),
        (Some(5_500), Some(700))
    );
    let trackers: Vec<(&str, u64)> = r["by_tracker"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["up"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        trackers,
        vec![("alpha.example", 4_000), ("beta.example", 1_500)],
        "trié par envoi décroissant"
    );
    assert_eq!(r["by_node"][0]["id"], node.as_str());
    assert_eq!(
        r["by_node"][0]["name"], "seedbox",
        "nom d'affichage du nœud"
    );
    assert!(
        r["tracking_since"].as_i64().is_some(),
        "date de début des mesures"
    );

    // Résumé : cumul des clients et ratio (envoyé / téléchargé, ou / taille si rien n'a été téléchargé).
    let sm = c.get("/api/stats/summary").await.body;
    assert_eq!(sm["uploaded_total"], 9_000_005_500u64);
    assert_eq!(sm["up_24h"], 5_500);
    assert_eq!(sm["down_24h"], 700);
    let ratio = sm["ratio"].as_f64().unwrap();
    // a : 9_000_004_000 / 500 ; b : 1_500 / 700 -> (9_000_005_500) / 1_200
    assert!((ratio - 9_000_005_500f64 / 1_200f64).abs() < 1.0, "{ratio}");

    // Vue par tracker : état actuel et volumes.
    let t = c.get("/api/stats/trackers").await.body;
    let alpha = t
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["tracker"] == "alpha.example")
        .unwrap();
    assert_eq!(
        (
            alpha["torrents"].as_u64(),
            alpha["seeding"].as_u64(),
            alpha["errors"].as_u64()
        ),
        (Some(1), Some(1), Some(0))
    );
    assert_eq!(alpha["uploaded"], 9_000_004_000u64);
    assert_eq!(alpha["up_30d"], 4_000);
    assert_eq!(alpha["nodes"], 1);

    // Paramètres invalides et accès non authentifié.
    assert_eq!(
        c.get("/api/stats/traffic?range=2y").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.get("/api/stats/traffic?range=7d&tz=9999").await.status,
        StatusCode::BAD_REQUEST
    );
    let mut anon = Client::new(&s.base);
    for p in [
        "summary",
        "traffic",
        "trackers",
        "idle",
        "disks",
        "multitrackers",
    ] {
        assert_eq!(
            anon.get(&format!("/api/stats/{p}")).await.status,
            StatusCode::UNAUTHORIZED,
            "{p}"
        );
    }
}

#[tokio::test]
async fn a_restart_of_the_master_does_not_lose_or_double_count_volumes() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    set_state(
        &s.core,
        &node,
        vec![stat_torrent("a", "alpha.example", 1_000, 0, Seeding)],
        1,
    );
    s.core.sample_traffic_at(now - 7200).unwrap();

    // « Redémarrage » : la mémoire du master est vide (registre et télémétrie), la base est conservée.
    *s.core.traffic_book_for_tests() = None;
    s.core.record(&node, |t| *t = Default::default());
    // Tant que l'état complet du nœud n'est pas revenu, rien n'est effacé ni compté.
    s.core.sample_traffic_at(now - 5400).unwrap();
    // Le nœud se reconnecte avec 600 octets de plus : seule la différence avec le relevé d'avant est comptée.
    set_state(
        &s.core,
        &node,
        vec![stat_torrent("a", "alpha.example", 1_600, 0, Seeding)],
        1,
    );
    s.core.sample_traffic_at(now - 3600).unwrap();
    let r = c.get("/api/stats/traffic?range=24h").await.body;
    assert_eq!(r["totals"]["up"], 600, "ni perte ni double comptage : {r}");
}

#[tokio::test]
async fn daily_volumes_follow_the_users_timezone_and_fill_quiet_days() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    set_state(
        &s.core,
        &node,
        vec![stat_torrent("a", "alpha.example", 0, 0, Seeding)],
        1,
    );
    s.core.sample_traffic_at(now - 4 * 86_400).unwrap();
    set_state(
        &s.core,
        &node,
        vec![stat_torrent("a", "alpha.example", 700, 0, Seeding)],
        1,
    );
    s.core.sample_traffic_at(now - 3 * 86_400).unwrap();

    let r = c.get("/api/stats/traffic?range=7d&tz=120").await.body;
    assert_eq!(r["bucket"], "day");
    let series = r["series"].as_array().unwrap();
    assert_eq!(
        series.len(),
        7,
        "un point par jour, jours sans trafic compris : {r}"
    );
    assert_eq!(
        series
            .iter()
            .map(|p| p["up"].as_u64().unwrap())
            .sum::<u64>(),
        700
    );
    assert_eq!(
        series
            .iter()
            .filter(|p| p["up"].as_u64().unwrap() > 0)
            .count(),
        1,
        "un seul jour actif"
    );
    // Début de jour = minuit LOCAL (UTC+2) exprimé en secondes Unix : modulo 86 400 = 22 h UTC.
    assert!(
        series
            .iter()
            .all(|p| p["ts"].as_i64().unwrap().rem_euclid(86_400) == 79_200),
        "{series:?}"
    );
    // Hors période : rien.
    let r = c.get("/api/stats/traffic?range=24h").await.body;
    assert_eq!(r["totals"]["up"], 0);
}

#[tokio::test]
async fn idle_torrents_are_seeding_ones_without_observed_upload_for_a_long_time() {
    use proto::pb::TorrentStatus::{Paused, Seeding};
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let t0 = master::store::unix_now() - 60 * 86_400;
    let day = 86_400;
    let set = |a_up: u64| {
        set_state(
            &s.core,
            &node,
            vec![
                stat_torrent("a", "alpha.example", a_up, 0, Seeding), // envoie encore
                stat_torrent("b", "beta.example", 50, 0, Seeding),    // plus rien depuis le début
                stat_torrent("c", "beta.example", 50, 0, Paused),     // en pause : un autre sujet
            ],
            1,
        )
    };
    set(100);
    s.core.sample_traffic_at(t0).unwrap();
    set(900);
    s.core.sample_traffic_at(t0 + 55 * day).unwrap(); // « a » a envoyé il y a 5 jours (vu de t0 + 60 j)

    let now = t0 + 60 * day;
    let idle = s.core.stats_idle_at(now, 30, 10).unwrap();
    assert_eq!(idle.len(), 1, "seul « b » : {idle:?}");
    assert_eq!(idle[0].infohash, "b".repeat(40));
    assert!((idle[0].idle_days - 60.0).abs() < 0.1);
    assert!((idle[0].observed_days - 60.0).abs() < 0.1);
    assert_eq!(idle[0].node, "seedbox");
    // Avec un seuil plus court, « a » (5 jours) apparaît aussi, après « b » (le plus ancien d'abord).
    let idle = s.core.stats_idle_at(now, 3, 10).unwrap();
    assert_eq!(
        idle.iter()
            .map(|i| i.infohash.chars().next().unwrap())
            .collect::<String>(),
        "ba"
    );
    assert_eq!(s.core.stats_summary_at(now).unwrap().idle_count, 1);
    // Via l'API (heure réelle = `now`) : même résultat, avec les champs utiles à l'affichage.
    let api = c.get("/api/stats/idle?days=30").await.body;
    assert_eq!(api.as_array().unwrap().len(), 1);
    assert_eq!(api[0]["name"], "torrent b");
    assert_eq!(api[0]["tracker"], "beta.example");
}

#[tokio::test]
async fn the_disk_forecast_announces_days_left_only_for_a_steady_fill() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    // L'espace disque n'est enregistré que pour un nœud en ligne (celui d'un nœud hors ligne serait périmé).
    s.core.store.set_state(&node, "Online", now).unwrap();
    // 3 jours d'historique, un relevé toutes les 6 h : 20 Go libres au début, -1 Go par jour.
    for i in 0..12u64 {
        let free = 23_000_000_000 - i * 250_000_000;
        set_state(
            &s.core,
            &node,
            vec![stat_torrent("a", "alpha.example", 0, 0, Seeding)],
            free,
        );
        s.core
            .sample_traffic_at(now - 3 * 86_400 + (i as i64) * 21_600)
            .unwrap();
    }
    let d = c.get("/api/stats/disks").await.body;
    let me = d
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["node_id"] == node.as_str())
        .unwrap();
    assert_eq!(me["trend"], "falling");
    assert!(
        (me["per_day"].as_f64().unwrap() + 1e9).abs() < 5e7,
        "≈ -1 Go/jour : {me}"
    );
    let left = me["days_left"]
        .as_f64()
        .expect("remplissage régulier : une date est annoncée");
    assert!((left - 20.25).abs() < 1.5, "≈ 20 jours : {left}");
    assert!(me["confidence"].as_f64().unwrap() > 0.9);
    assert!(me["series"].as_array().unwrap().len() >= 10);
    assert_eq!(me["name"], "seedbox");
}

#[tokio::test]
async fn multi_tracker_gain_counts_saved_space_and_the_extra_trackers_uploads() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    let (parent, child) = ("a".repeat(40), "b".repeat(40));
    for h in [&parent, &child] {
        assert_eq!(
            c.post("/api/torrents", json!({ "infohash": h }))
                .await
                .status,
            StatusCode::CREATED
        );
    }
    s.core.store.set_xseed_of(&child, &parent).unwrap();

    set_state(
        &s.core,
        &node,
        vec![
            stat_torrent("a", "alpha.example", 0, 0, Seeding),
            stat_torrent("b", "beta.example", 0, 0, Seeding),
        ],
        1,
    );
    s.core.sample_traffic_at(now - 7200).unwrap();
    set_state(
        &s.core,
        &node,
        vec![
            stat_torrent("a", "alpha.example", 3_000, 0, Seeding),
            stat_torrent("b", "beta.example", 2_000, 0, Seeding),
        ],
        1,
    );
    s.core.sample_traffic_at(now - 3600).unwrap();

    let g = c.get("/api/stats/multitrackers").await.body;
    assert_eq!(
        (g["members"].as_u64(), g["seeding"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(
        g["bytes_saved"], 1000,
        "la taille du torrent ajouté n'est pas dupliquée sur le disque"
    );
    assert_eq!(g["uploaded_total"], 2_000);
    assert_eq!(
        g["up_7d"], 2_000,
        "seul l'envoi du torrent ajouté compte, pas celui de l'original"
    );
    assert_eq!(g["up_30d"], 2_000);
    assert_eq!(g["trackers"], json!(["beta.example"]));
}

#[tokio::test]
async fn torrent_rows_carry_uploaded_ratio_and_sort_by_them() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let mut a = stat_torrent("a", "alpha.example", 3_000, 1_000, Seeding); // ratio 3
    a.added_on = 1_700_000_000;
    let b = stat_torrent("b", "alpha.example", 500, 0, Seeding); // rien téléchargé : 500 / taille 1000 = 0,5
    let d = stat_torrent("d", "", 0, 0, Seeding); // 0
    set_state(&s.core, &node, vec![a, b, d], 1);

    let p = c.get("/api/torrents?sort=ratio&dir=desc").await.body;
    let names: Vec<&str> = p["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["torrent a", "torrent b", "torrent d"]);
    assert_eq!(p["items"][0]["ratio"], 3.0);
    assert_eq!(p["items"][1]["ratio"], 0.5);
    assert_eq!(p["items"][0]["uploaded"], 3_000);
    assert_eq!(p["items"][0]["downloaded"], 1_000);
    assert_eq!(p["items"][0]["added_on"], 1_700_000_000u64);
    let p = c.get("/api/torrents?sort=uploaded&dir=asc").await.body;
    assert_eq!(p["items"][0]["name"], "torrent d");
}

// ----- rc.2 : classement par volume, heures de pointe, période précédente, détail par nœud -----

fn with_rate(mut t: proto::pb::TorrentInfo, ul_rate: u64) -> proto::pb::TorrentInfo {
    t.ul_rate = ul_rate;
    t
}

#[tokio::test]
async fn top_torrents_rank_by_volume_not_by_instantaneous_rate_and_group_multi_tracker_copies() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    for h in [&a, &b] {
        assert_eq!(
            c.post("/api/torrents", json!({ "infohash": h }))
                .await
                .status,
            StatusCode::CREATED
        );
    }
    s.core.store.set_xseed_of(&b, &a).unwrap(); // « b » = copie de « a » sur un autre tracker

    // Il y a 4 jours : « c » envoie beaucoup (hors des 24 h, dans les 7 jours).
    let state = |ua: u64, ub: u64, uc: u64, ra: u64, rb: u64, rc: u64| {
        set_state(
            &s.core,
            &node,
            vec![
                with_rate(stat_torrent("a", "alpha.example", ua, 0, Seeding), ra),
                with_rate(stat_torrent("b", "beta.example", ub, 0, Seeding), rb),
                with_rate(stat_torrent("c", "gamma.example", uc, 0, Seeding), rc),
            ],
            1,
        )
    };
    state(0, 0, 0, 0, 0, 0);
    s.core.sample_traffic_at(now - 4 * 86_400).unwrap();
    state(0, 0, 10_000, 0, 0, 0);
    s.core.sample_traffic_at(now - 3 * 86_400).unwrap();
    // Il y a 2 h : « a » +3 000 et sa copie « b » +2 000, « c » +4 000.
    state(3_000, 2_000, 14_000, 0, 0, 0);
    s.core.sample_traffic_at(now - 7200).unwrap();

    // Sur 24 h : « a » (avec sa copie : 5 000) devant « c » (4 000), MÊME si « c » a un débit actuel bien plus élevé.
    state(3_000, 2_000, 14_000, 100, 50, 9_000_000);
    let top = c.get("/api/stats/top?range=24h").await.body;
    assert_eq!(top["range"], "24h");
    let items = top["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        2,
        "une ligne par CONTENU : « a » et sa copie « b » sont regroupés : {top}"
    );
    assert_eq!(items[0]["infohash"], a.as_str());
    assert_eq!(items[0]["up"], 5_000);
    assert_eq!(
        items[0]["trackers"],
        json!(["alpha.example", "beta.example"])
    );
    assert_eq!(
        items[0]["rate"], 150,
        "débit actuel du contenu d'origine + celui de sa copie"
    );
    assert_eq!(items[0]["name"], "torrent a");
    assert_eq!(items[0]["nodes"], json!(["seedbox"]));
    assert_eq!(items[1]["infohash"], "c".repeat(40));
    assert_eq!(items[1]["up"], 4_000);
    assert_eq!(top["total_up"], 9_000);

    // Le débit actuel ne change pas l'ordre : on inverse tous les débits, le classement reste identique.
    state(3_000, 2_000, 14_000, 9_000_000, 9_000_000, 1);
    let again = c.get("/api/stats/top?range=24h").await.body;
    let order = |v: &Value| {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["infohash"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&top), order(&again), "classement stable");

    // Sur 7 jours : les 10 000 de « c » d'il y a 3 jours le font passer devant (14 000 contre 5 000).
    let week = c.get("/api/stats/top?range=7d").await.body;
    assert_eq!(week["items"][0]["infohash"], "c".repeat(40));
    assert_eq!(week["items"][0]["up"], 14_000);
    assert_eq!(week["items"][1]["up"], 5_000);
    // `limit` borne la liste.
    assert_eq!(
        c.get("/api/stats/top?range=7d&limit=1").await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    assert_eq!(
        c.get("/api/stats/top?range=1y").await.status,
        StatusCode::BAD_REQUEST
    );
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/stats/top").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_week_pattern_places_volumes_on_the_users_local_weekday_and_hour() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    set_state(
        &s.core,
        &node,
        vec![stat_torrent("a", "alpha.example", 0, 0, Seeding)],
        1,
    );
    s.core.sample_traffic_at(now - 3 * 86_400).unwrap();
    set_state(
        &s.core,
        &node,
        vec![stat_torrent("a", "alpha.example", 700, 0, Seeding)],
        1,
    );
    let t = now - 2 * 86_400;
    s.core.sample_traffic_at(t).unwrap();

    for tz_min in [0i64, 120, -300] {
        let p = c.get(&format!("/api/stats/pattern?tz={tz_min}")).await.body;
        let (day, hod) = master::traffic::week_slot(t.div_euclid(3600), tz_min * 60);
        assert_eq!(p["cells"][day][hod], 700, "tz={tz_min} : {p}");
        let total: u64 = p["cells"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r.as_array().unwrap())
            .map(|v| v.as_u64().unwrap())
            .sum();
        assert_eq!(total, 700, "rien d'autre n'est compté");
    }
    let p = c.get("/api/stats/pattern?tz=0").await.body;
    assert_eq!(p["cells"].as_array().unwrap().len(), 7);
    assert_eq!(p["cells"][0].as_array().unwrap().len(), 24);
    // Environ 3-4 jours mesurés : les jours de semaine observés sont comptés, les autres restent à 0.
    let days: u64 = p["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert!((3..=5).contains(&days), "jours mesurés : {days} ({p})");
    assert!(p["tracking_since"].as_i64().is_some());

    assert_eq!(
        c.get("/api/stats/pattern?tz=9999").await.status,
        StatusCode::BAD_REQUEST
    );
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/stats/pattern").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn traffic_compares_with_the_previous_period_only_when_it_was_measured() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let node = paired_node(&s, &mut c).await;
    let now = master::store::unix_now();
    let up = |u: u64| {
        set_state(
            &s.core,
            &node,
            vec![stat_torrent("a", "alpha.example", u, 0, Seeding)],
            1,
        )
    };
    up(0);
    s.core.sample_traffic_at(now - 60 * 3600).unwrap(); // la mesure commence il y a 60 h
    up(800);
    s.core.sample_traffic_at(now - 36 * 3600).unwrap(); // 24-48 h en arrière : période PRÉCÉDENTE des 24 h
    up(1_100);
    s.core.sample_traffic_at(now - 2 * 3600).unwrap(); // dans les dernières 24 h

    let r = c.get("/api/stats/traffic?range=24h").await.body;
    assert_eq!(r["totals"]["up"], 300);
    assert_eq!(r["previous"]["up"], 800);
    assert_eq!(
        r["previous_covered"], true,
        "60 h de mesure couvrent les 48 h nécessaires"
    );
    // Sur 7 jours, la période précédente (jours 8 à 14) n'a pas été mesurée : on ne compare pas.
    let w = c.get("/api/stats/traffic?range=7d").await.body;
    assert_eq!(w["previous_covered"], false);
}

#[tokio::test]
async fn each_node_of_a_torrent_reports_its_own_rates_and_counters() {
    use proto::pb::TorrentStatus::Seeding;
    let s = start(AdminSettings::default()).await;
    let mut c = setup_and_login(&s).await;
    let n1 = paired_node(&s, &mut c).await;
    let n2 = paired_node(&s, &mut c).await;
    // Le même torrent sur deux nœuds, avec des débits et des compteurs différents.
    let mut t1 = with_rate(
        stat_torrent("a", "alpha.example", 4_000, 1_000, Seeding),
        300,
    );
    t1.dl_rate = 20;
    let t2 = with_rate(stat_torrent("a", "alpha.example", 600, 0, Seeding), 50); // rien téléchargé : ratio sur la taille
    set_state(&s.core, &n1, vec![t1], 1);
    set_state(&s.core, &n2, vec![t2], 1);

    let p = c.get("/api/torrents").await.body;
    let row = &p["items"][0];
    assert_eq!(row["ul_rate"], 350, "la ligne additionne les nœuds");
    assert_eq!(row["uploaded"], 4_600);
    let nodes = row["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    let of = |id: &str| nodes.iter().find(|n| n["node_id"] == id).unwrap();
    assert_eq!(
        (of(&n1)["ul_rate"].as_u64(), of(&n1)["dl_rate"].as_u64()),
        (Some(300), Some(20))
    );
    assert_eq!(
        (of(&n1)["uploaded"].as_u64(), of(&n1)["downloaded"].as_u64()),
        (Some(4_000), Some(1_000))
    );
    assert_eq!(of(&n1)["ratio"], 4.0);
    assert_eq!(
        (of(&n2)["ul_rate"].as_u64(), of(&n2)["uploaded"].as_u64()),
        (Some(50), Some(600))
    );
    assert_eq!(of(&n2)["ratio"], 0.6, "rien téléchargé : 600 / taille 1000");
}

// ----- Comptes multiples : création, mot de passe provisoire, réinitialisation, suppression -----

const TEMP: &str = "Temporary-Pass-77";
const FINAL: &str = "Definitive-Pass-88";

async fn new_user(c: &mut Client, name: &str, pw: &str) -> Resp {
    c.post("/api/users", json!({ "username": name, "password": pw }))
        .await
}

#[tokio::test]
async fn a_new_account_can_only_change_its_temporary_password_until_it_does() {
    let s = start(AdminSettings::default()).await;
    let mut alice = setup_and_login(&s).await;
    assert_eq!(
        new_user(&mut alice, "bob", TEMP).await.status,
        StatusCode::CREATED
    );

    let list = alice.get("/api/users").await.body;
    let users = list.as_array().unwrap();
    assert_eq!(users.len(), 2);
    let me = users.iter().find(|u| u["username"] == "alice").unwrap();
    let bob = users.iter().find(|u| u["username"] == "bob").unwrap();
    assert_eq!(
        (me["is_me"].as_bool(), bob["is_me"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(bob["must_change"], true);
    assert_eq!(me["must_change"], false);
    assert!(bob["last_login_at"].is_null(), "jamais connecté");
    assert!(
        !list.to_string().contains("hash") && !list.to_string().contains(TEMP),
        "aucun secret dans la liste"
    );

    // Bob se connecte avec le mot de passe provisoire : il n'accède à rien d'autre qu'au changement de mot de passe.
    let mut bob = Client::new(&s.base);
    assert_eq!(bob.login("bob", TEMP).await.status, StatusCode::OK);
    let me = bob.get("/api/me").await.body;
    assert_eq!(
        (
            me["username"].as_str(),
            me["must_change_password"].as_bool()
        ),
        (Some("bob"), Some(true))
    );
    for path in [
        "/api/nodes",
        "/api/stats",
        "/api/torrents",
        "/api/users",
        "/api/audit",
        "/api/account",
    ] {
        assert_eq!(
            bob.get(path).await.status,
            StatusCode::FORBIDDEN,
            "{path} bloqué tant que le mot de passe est provisoire"
        );
    }
    assert_eq!(
        new_user(&mut bob, "eve", TEMP).await.status,
        StatusCode::FORBIDDEN,
        "créer un compte est bloqué aussi"
    );

    // Un mot de passe faible ou identique n'est pas accepté, et le blocage demeure.
    let weak = bob
        .post("/api/password", json!({ "current": TEMP, "new": "123456" }))
        .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    assert_eq!(bob.get("/api/nodes").await.status, StatusCode::FORBIDDEN);
    let wrong = bob
        .post(
            "/api/password",
            json!({ "current": "Pas-le-bon-mdp-1", "new": FINAL }),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);

    // Changement : tout s'ouvre, sans nouvelle connexion.
    let ok = bob
        .post("/api/password", json!({ "current": TEMP, "new": FINAL }))
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(bob.get("/api/nodes").await.status, StatusCode::OK);
    assert_eq!(bob.get("/api/me").await.body["must_change_password"], false);
    // L'ancien mot de passe ne marche plus ; le nouveau, oui ; la dernière connexion est enregistrée.
    let mut again = Client::new(&s.base);
    assert_eq!(
        again.login("bob", TEMP).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(again.login("bob", FINAL).await.status, StatusCode::OK);
    assert_eq!(
        again.get("/api/nodes").await.status,
        StatusCode::OK,
        "plus de blocage à la connexion suivante"
    );
    let list = alice.get("/api/users").await.body;
    let bob = list
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["username"] == "bob")
        .unwrap();
    assert!(bob["last_login_at"].as_i64().is_some());
    assert_eq!(bob["must_change"], false);

    // Le journal attribue la création à son auteur, et ne contient jamais de mot de passe.
    let log = alice.get("/api/audit?action=user_created").await.body;
    let item = &log["items"][0];
    assert!(item["title"].as_str().unwrap().contains("bob"), "{item}");
    assert_eq!(item["actor_label"], "alice");
    assert!(!log.to_string().contains(TEMP) && !log.to_string().contains(FINAL));
}

#[tokio::test]
async fn account_creation_is_validated_bounded_and_authenticated() {
    let s = start(AdminSettings::default()).await;
    let mut alice = setup_and_login(&s).await;
    assert_eq!(
        new_user(&mut alice, "ab", TEMP).await.status,
        StatusCode::BAD_REQUEST,
        "nom trop court"
    );
    assert_eq!(
        new_user(&mut alice, "bob smith", TEMP).await.status,
        StatusCode::BAD_REQUEST,
        "caractère interdit"
    );
    assert_eq!(
        new_user(&mut alice, "bob", "123456").await.status,
        StatusCode::BAD_REQUEST,
        "mot de passe faible"
    );
    assert_eq!(
        new_user(&mut alice, "bob", "bob-bob-bob-bob-1")
            .await
            .status,
        StatusCode::BAD_REQUEST,
        "mot de passe dérivé du nom"
    );
    assert_eq!(
        new_user(&mut alice, "bob", TEMP).await.status,
        StatusCode::CREATED
    );
    // Un doublon est refusé sans tenir compte de la casse (deux « bob » indiscernables pour un humain).
    assert_eq!(
        new_user(&mut alice, "BOB", TEMP).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        new_user(&mut alice, "alice", TEMP).await.status,
        StatusCode::CONFLICT
    );

    // Plafond : 20 comptes (alice + bob + 18 autres), le 21e est refusé.
    for i in 0..18 {
        assert_eq!(
            new_user(&mut alice, &format!("user{i:02}"), TEMP)
                .await
                .status,
            StatusCode::CREATED,
            "user{i}"
        );
    }
    let over = new_user(&mut alice, "trop-nombreux", TEMP).await;
    assert_eq!(over.status, StatusCode::BAD_REQUEST);
    assert!(
        over.body["error"].as_str().unwrap().contains("20"),
        "{:?}",
        over.body
    );

    // Sans session : rien n'est possible.
    let mut anon = Client::new(&s.base);
    assert_eq!(
        anon.get("/api/users").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        new_user(&mut anon, "mallory", TEMP).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.post("/api/users/bob/password", json!({ "password": TEMP }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.call(Method::DELETE, "/api/users/bob", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anon.call(Method::DELETE, "/api/users/bob/totp", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn resetting_a_password_closes_the_sessions_and_forces_a_new_change() {
    let s = start(AdminSettings::default()).await;
    let mut alice = setup_and_login(&s).await;
    new_user(&mut alice, "bob", TEMP).await;
    let mut bob = Client::new(&s.base);
    bob.login("bob", TEMP).await;
    bob.post("/api/password", json!({ "current": TEMP, "new": FINAL }))
        .await;
    assert_eq!(bob.get("/api/nodes").await.status, StatusCode::OK);

    // Alice réinitialise : la session de Bob est fermée, et il doit rechoisir un mot de passe.
    let r = alice
        .post(
            "/api/users/bob/password",
            json!({ "password": "Reset-Pass-4242" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        bob.get("/api/nodes").await.status,
        StatusCode::UNAUTHORIZED,
        "session fermée"
    );
    let mut bob2 = Client::new(&s.base);
    assert_eq!(
        bob2.login("bob", FINAL).await.status,
        StatusCode::UNAUTHORIZED,
        "l'ancien mot de passe ne marche plus"
    );
    assert_eq!(
        bob2.login("bob", "Reset-Pass-4242").await.status,
        StatusCode::OK
    );
    assert_eq!(
        bob2.get("/api/nodes").await.status,
        StatusCode::FORBIDDEN,
        "mot de passe provisoire à changer"
    );

    // Garde-fous : pas soi-même (« Mon compte » exige le mot de passe actuel), pas d'inconnu, pas de mot de passe faible.
    assert_eq!(
        alice
            .post(
                "/api/users/alice/password",
                json!({ "password": "Reset-Pass-4242" })
            )
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        alice
            .post(
                "/api/users/inconnu/password",
                json!({ "password": "Reset-Pass-4242" })
            )
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        alice
            .post("/api/users/bob/password", json!({ "password": "123456" }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let log = alice
        .get("/api/audit?action=user_password_reset")
        .await
        .body;
    assert!(log["items"][0]["title"].as_str().unwrap().contains("bob"));
    assert!(!log.to_string().contains("Reset-Pass-4242"));
}

#[tokio::test]
async fn deleting_an_account_closes_its_sessions_and_never_removes_your_own() {
    let s = start(AdminSettings::default()).await;
    let mut alice = setup_and_login(&s).await;
    new_user(&mut alice, "bob", TEMP).await;
    let mut bob = Client::new(&s.base);
    bob.login("bob", TEMP).await;
    bob.post("/api/password", json!({ "current": TEMP, "new": FINAL }))
        .await;
    assert_eq!(bob.get("/api/nodes").await.status, StatusCode::OK);

    assert_eq!(
        alice
            .call(Method::DELETE, "/api/users/alice", None)
            .await
            .status,
        StatusCode::BAD_REQUEST,
        "pas son propre compte"
    );
    assert_eq!(
        alice
            .call(Method::DELETE, "/api/users/inconnu", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        alice
            .call(Method::DELETE, "/api/users/bob", None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        bob.get("/api/nodes").await.status,
        StatusCode::UNAUTHORIZED,
        "session fermée"
    );
    assert_eq!(
        Client::new(&s.base).login("bob", FINAL).await.status,
        StatusCode::UNAUTHORIZED,
        "ne peut plus se connecter"
    );
    let users = alice.get("/api/users").await.body;
    assert_eq!(
        users.as_array().unwrap().len(),
        1,
        "il reste au moins un compte"
    );
    let log = alice.get("/api/audit?action=user_deleted").await.body;
    assert!(log["items"][0]["title"].as_str().unwrap().contains("bob"));
    // Le nom redevient disponible.
    assert_eq!(
        new_user(&mut alice, "bob", TEMP).await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn an_admin_can_disable_another_accounts_two_factor_but_not_their_own() {
    let s = start(AdminSettings::default()).await;
    let mut alice = setup_and_login(&s).await;
    new_user(&mut alice, "bob", TEMP).await;
    // Bob active la double authentification (secret quelconque : seul l'état compte ici).
    assert!(s
        .core
        .store
        .admin_totp_enable("bob", b"01234567890123456789", &["h1".to_owned()])
        .unwrap());
    let users = alice.get("/api/users").await.body;
    assert_eq!(users[1]["totp_enabled"], true);

    assert_eq!(
        alice
            .call(Method::DELETE, "/api/users/alice/totp", None)
            .await
            .status,
        StatusCode::BAD_REQUEST,
        "pas le sien par ce chemin"
    );
    assert_eq!(
        alice
            .call(Method::DELETE, "/api/users/inconnu/totp", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        alice
            .call(Method::DELETE, "/api/users/bob/totp", None)
            .await
            .status,
        StatusCode::OK
    );
    let users = alice.get("/api/users").await.body;
    assert_eq!(users[1]["totp_enabled"], false);
    let log = alice.get("/api/audit?action=user_totp_reset").await.body;
    let item = &log["items"][0];
    assert!(item["title"].as_str().unwrap().contains("bob"));
    assert_eq!(
        item["severity"], "warning",
        "événement de sécurité : mis en évidence"
    );
}
