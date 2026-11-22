//! Faux serveur qBittorrent WebUI API v2 (auth par cookie SID, v4 `pause/resume` et v5 `stop/start`).
use axum::extract::{Form, Multipart, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize)]
pub struct MockTorrent {
    pub hash: String,
    pub name: String,
    pub state: String,
    pub progress: f64,
    pub save_path: String,
    pub size: u64,
    pub dlspeed: u64,
    pub upspeed: u64,
    pub num_seeds: u32,
    pub num_leechs: u32,
    pub num_complete: i64,
    pub num_incomplete: i64,
    pub tracker: String,
}

#[derive(Default)]
struct St {
    trackers: BTreeMap<String, serde_json::Value>,
    tracker_calls: u32,
    connection_status: String,
    dht_nodes: u64,
    torrents: BTreeMap<String, MockTorrent>,
    sid: Option<String>,
    next_sid: u32,
    logins: u32,
    v5: bool,
    free: u64,
    limits: (i64, i64),
    unauth_401: bool,
    /// Fichiers .torrent reçus par `torrents/add` (hash, contenu), dans l'ordre.
    added_files: Vec<(String, Vec<u8>)>,
    /// Trackers ajoutés par `torrents/addTrackers` (hash, url), dans l'ordre.
    added_trackers: Vec<(String, String)>,
    /// Contenu renvoyé par `torrents/export` (hash -> .torrent).
    exports: BTreeMap<String, Vec<u8>>,
    /// Suppressions demandées (hash, avec les fichiers ?), dans l'ordre.
    deleted: Vec<(String, bool)>,
    /// Plafonds par torrent (hash -> (téléchargement, envoi)), posés par `torrents/set*Limit`.
    torrent_limits: BTreeMap<String, (i64, i64)>,
    export_calls: u32,
}

#[derive(Clone)]
pub struct MockQbit {
    pub addr: SocketAddr,
    st: Arc<Mutex<St>>,
}

type S = State<Arc<Mutex<St>>>;

fn lock(s: &Arc<Mutex<St>>) -> std::sync::MutexGuard<'_, St> {
    s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn authed(st: &St, h: &HeaderMap) -> bool {
    let want = match &st.sid {
        Some(s) => format!("SID={s}"),
        None => return false,
    };
    h.get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.split(';').any(|p| p.trim() == want))
}

macro_rules! guard {
    ($st:expr, $h:expr) => {
        if !authed(&$st, &$h) {
            return if $st.unauth_401 {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::FORBIDDEN
            }
            .into_response();
        }
    };
}

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

async fn login(State(s): S, Form(f): Form<Login>) -> Response {
    if f.username != "admin" || f.password != "adminadmin" {
        return "Fails.".into_response();
    }
    let mut st = lock(&s);
    st.next_sid += 1;
    st.logins += 1;
    let sid = format!("sid{}", st.next_sid);
    st.sid = Some(sid.clone());
    let body = if st.unauth_401 { "" } else { "Ok." };
    let code = if st.unauth_401 {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::OK
    };
    (
        code,
        [(header::SET_COOKIE, format!("SID={sid}; path=/"))],
        body,
    )
        .into_response()
}

async fn info(State(s): S, h: HeaderMap) -> Response {
    let st = lock(&s);
    guard!(st, h);
    Json(st.torrents.values().cloned().collect::<Vec<_>>()).into_response()
}

async fn add(State(s): S, h: HeaderMap, mut mp: Multipart) -> Response {
    {
        let st = lock(&s);
        guard!(st, h);
    }
    let (mut urls, mut savepath, mut paused) = (String::new(), String::new(), false);
    let mut file: Option<Vec<u8>> = None;
    while let Ok(Some(f)) = mp.next_field().await {
        let name = f.name().unwrap_or_default().to_owned();
        if name == "torrents" {
            file = f.bytes().await.ok().map(|b| b.to_vec());
            continue;
        }
        let v = f.text().await.unwrap_or_default();
        match name.as_str() {
            "urls" => urls = v,
            "savepath" => savepath = v,
            "paused" | "stopped" => paused |= v == "true",
            _ => {}
        }
    }
    let hash = match &file {
        Some(b) => crate::torrent::hash_from_file(b),
        None => crate::torrent::hash_from_magnet(&urls),
    };
    let Some(hash) = hash else {
        return "Fails.".into_response();
    };
    let mut st = lock(&s);
    if let Some(b) = file {
        st.added_files.push((hash.clone(), b));
    }
    st.torrents.entry(hash.clone()).or_insert(MockTorrent {
        name: format!("torrent-{}", &hash[..8.min(hash.len())]),
        hash,
        state: if paused { "pausedDL" } else { "downloading" }.into(),
        progress: 0.0,
        save_path: savepath,
        size: 1000,
        dlspeed: 0,
        upspeed: 0,
        num_seeds: 0,
        num_leechs: 0,
        num_complete: 0,
        num_incomplete: 0,
        tracker: String::new(),
    });
    "Ok.".into_response()
}

#[derive(Deserialize)]
struct Hashes {
    hashes: String,
    #[serde(rename = "deleteFiles")]
    delete_files: Option<String>,
}

fn each(st: &mut St, hashes: &str, f: impl Fn(&mut MockTorrent)) {
    for h in hashes.split('|') {
        if let Some(t) = st.torrents.get_mut(&h.to_lowercase()) {
            f(t);
        }
    }
}

async fn pause_impl(s: Arc<Mutex<St>>, h: HeaderMap, f: Hashes, new_name: bool) -> Response {
    let mut st = lock(&s);
    if st.v5 != new_name {
        return StatusCode::NOT_FOUND.into_response();
    }
    guard!(st, h);
    each(&mut st, &f.hashes, |t| {
        t.state = if t.progress >= 1.0 {
            "pausedUP"
        } else {
            "pausedDL"
        }
        .into();
    });
    "".into_response()
}

async fn resume_impl(s: Arc<Mutex<St>>, h: HeaderMap, f: Hashes, new_name: bool) -> Response {
    let mut st = lock(&s);
    if st.v5 != new_name {
        return StatusCode::NOT_FOUND.into_response();
    }
    guard!(st, h);
    each(&mut st, &f.hashes, |t| {
        t.state = if t.progress >= 1.0 {
            "stalledUP"
        } else {
            "downloading"
        }
        .into();
    });
    "".into_response()
}

async fn pause(State(s): S, h: HeaderMap, Form(f): Form<Hashes>) -> Response {
    pause_impl(s, h, f, false).await
}
async fn stop(State(s): S, h: HeaderMap, Form(f): Form<Hashes>) -> Response {
    pause_impl(s, h, f, true).await
}
async fn resume(State(s): S, h: HeaderMap, Form(f): Form<Hashes>) -> Response {
    resume_impl(s, h, f, false).await
}
async fn start(State(s): S, h: HeaderMap, Form(f): Form<Hashes>) -> Response {
    resume_impl(s, h, f, true).await
}

async fn recheck(State(s): S, h: HeaderMap, Form(f): Form<Hashes>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    each(&mut st, &f.hashes, |t| t.state = "checkingDL".into());
    "".into_response()
}

#[derive(Deserialize)]
struct AddTrackers {
    hash: String,
    urls: String,
}

async fn add_trackers(State(s): S, h: HeaderMap, Form(f): Form<AddTrackers>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    let hash = f.hash.to_lowercase();
    if !st.torrents.contains_key(&hash) {
        return StatusCode::NOT_FOUND.into_response();
    }
    for u in f.urls.split('\n').filter(|u| !u.is_empty()) {
        st.added_trackers.push((hash.clone(), u.to_owned()));
    }
    "".into_response()
}

#[derive(Deserialize)]
struct TLimit {
    hashes: String,
    limit: i64,
}

async fn t_set_dl(State(s): S, h: HeaderMap, Form(f): Form<TLimit>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    for x in f.hashes.split('|') {
        st.torrent_limits.entry(x.to_lowercase()).or_default().0 = f.limit;
    }
    "".into_response()
}

async fn t_set_ul(State(s): S, h: HeaderMap, Form(f): Form<TLimit>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    for x in f.hashes.split('|') {
        st.torrent_limits.entry(x.to_lowercase()).or_default().1 = f.limit;
    }
    "".into_response()
}

#[derive(Deserialize)]
struct ExportQ {
    hash: String,
}

async fn export(
    State(s): S,
    h: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ExportQ>,
) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    st.export_calls += 1;
    match st.exports.get(&q.hash.to_lowercase()) {
        Some(b) => b.clone().into_response(),
        None => StatusCode::CONFLICT.into_response(),
    }
}

async fn delete(State(s): S, h: HeaderMap, Form(f): Form<Hashes>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    let with_files = f.delete_files.as_deref() == Some("true");
    for x in f.hashes.split('|') {
        st.deleted.push((x.to_lowercase(), with_files));
        st.torrents.remove(&x.to_lowercase());
    }
    "".into_response()
}

#[derive(Deserialize)]
struct Limit {
    limit: i64,
}

async fn set_dl(State(s): S, h: HeaderMap, Form(f): Form<Limit>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    st.limits.0 = f.limit;
    "".into_response()
}

async fn set_ul(State(s): S, h: HeaderMap, Form(f): Form<Limit>) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    st.limits.1 = f.limit;
    "".into_response()
}

async fn maindata(State(s): S, h: HeaderMap) -> Response {
    let st = lock(&s);
    guard!(st, h);
    Json(serde_json::json!({ "server_state": { "free_space_on_disk": st.free } })).into_response()
}

#[derive(Deserialize)]
struct HashQuery {
    hash: String,
}

async fn trackers(
    State(s): S,
    h: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<HashQuery>,
) -> Response {
    let mut st = lock(&s);
    guard!(st, h);
    st.tracker_calls += 1;
    Json(
        st.trackers
            .get(&q.hash)
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .into_response()
}

async fn transfer_info(State(s): S, h: HeaderMap) -> Response {
    let st = lock(&s);
    guard!(st, h);
    Json(serde_json::json!({
        "connection_status": if st.connection_status.is_empty() { "connected" } else { &st.connection_status },
        "dht_nodes": st.dht_nodes,
    }))
    .into_response()
}

async fn preferences(State(s): S, h: HeaderMap) -> Response {
    let st = lock(&s);
    guard!(st, h);
    Json(serde_json::json!({ "listen_port": 6881 })).into_response()
}

async fn version(State(s): S, h: HeaderMap) -> Response {
    let st = lock(&s);
    guard!(st, h);
    "v4.6.0".into_response()
}

impl MockQbit {
    pub async fn start() -> Self {
        let st = Arc::new(Mutex::new(St {
            free: 123_456_789,
            ..St::default()
        }));
        let app = Router::new()
            .route("/api/v2/auth/login", post(login))
            .route("/api/v2/torrents/info", get(info))
            .route("/api/v2/torrents/add", post(add))
            .route("/api/v2/torrents/pause", post(pause))
            .route("/api/v2/torrents/stop", post(stop))
            .route("/api/v2/torrents/resume", post(resume))
            .route("/api/v2/torrents/start", post(start))
            .route("/api/v2/torrents/recheck", post(recheck))
            .route("/api/v2/torrents/delete", post(delete))
            .route("/api/v2/torrents/export", get(export))
            .route("/api/v2/torrents/setDownloadLimit", post(t_set_dl))
            .route("/api/v2/torrents/setUploadLimit", post(t_set_ul))
            .route("/api/v2/torrents/addTrackers", post(add_trackers))
            .route("/api/v2/transfer/setDownloadLimit", post(set_dl))
            .route("/api/v2/transfer/setUploadLimit", post(set_ul))
            .route("/api/v2/sync/maindata", get(maindata))
            .route("/api/v2/app/version", get(version))
            .route("/api/v2/transfer/info", get(transfer_info))
            .route("/api/v2/torrents/trackers", get(trackers))
            .route("/api/v2/app/preferences", get(preferences))
            .with_state(st.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("addr: {e}"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, st }
    }

    /// Suppressions demandées au client : (hash, fichiers supprimés aussi ?).
    pub fn deleted(&self) -> Vec<(String, bool)> {
        lock(&self.st).deleted.clone()
    }
    /// Fixe le .torrent que renverra `torrents/export` pour ce hash.
    pub fn set_export(&self, hash: &str, bytes: Vec<u8>) {
        lock(&self.st).exports.insert(hash.to_lowercase(), bytes);
    }
    /// Plafonds (téléchargement, envoi) posés sur ce torrent ; `None` = jamais posés.
    pub fn torrent_limits(&self, hash: &str) -> Option<(i64, i64)> {
        lock(&self.st).torrent_limits.get(hash).copied()
    }
    pub fn export_calls(&self) -> u32 {
        lock(&self.st).export_calls
    }
    /// .torrent reçus à l'ajout (hash, contenu).
    pub fn added_files(&self) -> Vec<(String, Vec<u8>)> {
        lock(&self.st).added_files.clone()
    }
    /// Trackers ajoutés après coup (hash, url).
    pub fn added_trackers(&self) -> Vec<(String, String)> {
        lock(&self.st).added_trackers.clone()
    }
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn torrents(&self) -> Vec<MockTorrent> {
        lock(&self.st).torrents.values().cloned().collect()
    }
    pub fn get(&self, hash: &str) -> Option<MockTorrent> {
        lock(&self.st).torrents.get(hash).cloned()
    }
    pub fn insert(&self, t: MockTorrent) {
        lock(&self.st).torrents.insert(t.hash.clone(), t);
    }
    /// Simule la fin du téléchargement (passage en seed).
    pub fn complete(&self, hash: &str) {
        if let Some(t) = lock(&self.st).torrents.get_mut(hash) {
            t.progress = 1.0;
            t.state = "stalledUP".into();
        }
    }
    /// Simule la fin d'une vérification (recheck) : données complètes, torrent en pause.
    pub fn finish_check(&self, hash: &str) {
        if let Some(t) = lock(&self.st).torrents.get_mut(hash) {
            t.progress = 1.0;
            t.state = "pausedUP".into();
        }
    }
    /// Supprime un torrent « à la main » (simule une dérive locale).
    pub fn remove(&self, hash: &str) {
        lock(&self.st).torrents.remove(hash);
    }
    /// Invalide la session : les requêtes suivantes reçoivent 403 jusqu'à un nouveau login.
    pub fn expire_session(&self) {
        lock(&self.st).sid = None;
    }
    pub fn set_free(&self, bytes: u64) {
        lock(&self.st).free = bytes;
    }
    /// Comportement des versions récentes : 401 (et non 403) sans session, login = 204 sans corps.
    /// Réponse de `torrents/trackers` pour un torrent.
    pub fn set_trackers(&self, hash: &str, v: serde_json::Value) {
        lock(&self.st).trackers.insert(hash.into(), v);
    }
    pub fn tracker_calls(&self) -> u32 {
        lock(&self.st).tracker_calls
    }
    /// État réseau annoncé par le faux client : « connected », « firewalled » ou « disconnected ».
    pub fn set_connection(&self, status: &str, dht_nodes: u64) {
        let mut st = lock(&self.st);
        st.connection_status = status.into();
        st.dht_nodes = dht_nodes;
    }
    pub fn set_unauth_401(&self, on: bool) {
        lock(&self.st).unauth_401 = on;
    }
    pub fn set_v5(&self, v5: bool) {
        lock(&self.st).v5 = v5;
    }
    pub fn logins(&self) -> u32 {
        lock(&self.st).logins
    }
    pub fn limits(&self) -> (i64, i64) {
        lock(&self.st).limits
    }
}

impl MockTorrent {
    pub fn new(hash: &str, state: &str, progress: f64, save_path: &str) -> Self {
        Self {
            hash: hash.into(),
            name: format!("t-{}", &hash[..6.min(hash.len())]),
            state: state.into(),
            progress,
            save_path: save_path.into(),
            size: 1000,
            dlspeed: 0,
            upspeed: 0,
            num_seeds: 0,
            num_leechs: 0,
            num_complete: 0,
            num_incomplete: 0,
            tracker: String::new(),
        }
    }
}
