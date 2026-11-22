//! Faux Deluge Web UI (JSON-RPC `/json`) : cookie de session, `web.connect` obligatoire, méthodes 2.x.
use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct MockD {
    pub hash: String,
    pub state: String,
    pub progress: f64,
    pub save_path: String,
}

#[derive(Default)]
struct St {
    t: BTreeMap<String, MockD>,
    sid: Option<String>,
    n: u32,
    logins: u32,
    connected: bool,
    config: Value,
}

#[derive(Clone)]
pub struct MockDeluge {
    pub addr: SocketAddr,
    st: Arc<Mutex<St>>,
}

fn lock(s: &Arc<Mutex<St>>) -> std::sync::MutexGuard<'_, St> {
    s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn res(id: &Value, r: Value) -> Value {
    json!({ "result": r, "error": null, "id": id })
}
fn err(id: &Value, code: i64, m: &str) -> Value {
    json!({ "result": null, "error": { "message": m, "code": code }, "id": id })
}

async fn json_rpc(
    State(s): State<Arc<Mutex<St>>>,
    h: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    let id = req["id"].clone();
    let p = &req["params"];
    let mut st = lock(&s);
    let method = req["method"].as_str().unwrap_or_default();
    if method == "auth.login" {
        if p[0].as_str() != Some("deluge") {
            return Json(res(&id, json!(false))).into_response();
        }
        st.n += 1;
        st.logins += 1;
        let sid = format!("dsid{}", st.n);
        st.sid = Some(sid.clone());
        return (
            [(header::SET_COOKIE, format!("_session_id={sid}; Path=/"))],
            Json(res(&id, json!(true))),
        )
            .into_response();
    }
    let authed = st.sid.as_ref().is_some_and(|sid| {
        h.get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| {
                c.split(';')
                    .any(|x| x.trim() == format!("_session_id={sid}"))
            })
    });
    if !authed {
        // Comme le vrai Web UI : les méthodes `daemon.*` n'existent qu'une fois la session reliée au démon,
        // l'appel non authentifié reçoit donc « Unknown method » (code 2) et non « Not authenticated » (code 1).
        if method.starts_with("daemon.") {
            return Json(err(&id, 2, "Unknown method")).into_response();
        }
        return Json(err(&id, 1, "Not authenticated")).into_response();
    }
    let ids = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let out = match method {
        "web.connected" => res(&id, json!(st.connected)),
        "web.get_hosts" => res(&id, json!([["host1", "127.0.0.1", 58846, "localclient"]])),
        "web.connect" => {
            st.connected = true;
            res(&id, json!(["core.get_torrents_status"]))
        }
        _ if !st.connected => err(&id, 3, "Not connected to a daemon"),
        "daemon.get_version" => res(&id, json!("2.2.0-mock")),
        "core.get_free_space" => res(&id, json!(555_000_000u64)),
        "core.set_config" => {
            st.config = p[0].clone();
            res(&id, json!(null))
        }
        "core.get_torrents_status" => {
            let m: serde_json::Map<String, Value> = st
                .t
                .values()
                .map(|t| {
                    (
                        t.hash.clone(),
                        json!({ "name": format!("t-{}", &t.hash[..6]), "state": t.state, "progress": t.progress * 100.0,
                                "save_path": t.save_path, "total_size": 1000,
                                "download_payload_rate": 0, "upload_payload_rate": 0,
                                "total_uploaded": 7000, "all_time_download": 3000, "time_added": 1_700_000_001.5 }),
                    )
                })
                .collect();
            res(&id, Value::Object(m))
        }
        m @ ("core.add_torrent_magnet" | "core.add_torrent_file") => {
            let (hash, opts) = if m == "core.add_torrent_file" {
                (
                    crate::torrent::hash_from_b64(p[1].as_str().unwrap_or_default()),
                    &p[2],
                )
            } else {
                (
                    crate::torrent::hash_from_magnet(p[0].as_str().unwrap_or_default()),
                    &p[1],
                )
            };
            let Some(hash) = hash else {
                return Json(err(&id, 4, "bad torrent")).into_response();
            };
            if st.t.contains_key(&hash) {
                res(&id, json!(null)) // doublon : Deluge renvoie null
            } else {
                let state = if opts["add_paused"].as_bool().unwrap_or(false) {
                    "Paused"
                } else {
                    "Downloading"
                };
                st.t.insert(
                    hash.clone(),
                    MockD {
                        hash: hash.clone(),
                        state: state.into(),
                        progress: 0.0,
                        save_path: opts["download_location"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    },
                );
                res(&id, json!(hash))
            }
        }
        m @ ("core.pause_torrents" | "core.resume_torrents" | "core.force_recheck") => {
            for h in ids(&p[0]) {
                if let Some(t) = st.t.get_mut(&h) {
                    t.state = match m {
                        "core.pause_torrents" => "Paused",
                        "core.force_recheck" => "Checking",
                        _ => {
                            if t.progress >= 1.0 {
                                "Seeding"
                            } else {
                                "Downloading"
                            }
                        }
                    }
                    .into();
                }
            }
            res(&id, json!(null))
        }
        "core.remove_torrents" => {
            for h in ids(&p[0]) {
                st.t.remove(&h);
            }
            res(&id, json!([]))
        }
        _ => err(&id, 2, "Unknown method"),
    };
    Json(out).into_response()
}

impl MockDeluge {
    pub async fn start() -> Self {
        let st = Arc::new(Mutex::new(St::default()));
        let app = Router::new()
            .route("/json", post(json_rpc))
            .with_state(st.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr = l.local_addr().unwrap_or_else(|e| panic!("addr: {e}"));
        tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        Self { addr, st }
    }
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn get(&self, hash: &str) -> Option<MockD> {
        lock(&self.st).t.get(hash).cloned()
    }
    pub fn logins(&self) -> u32 {
        lock(&self.st).logins
    }
    /// Invalide la session (les appels suivants reçoivent « Not authenticated »).
    pub fn expire_session(&self) {
        lock(&self.st).sid = None;
    }
    pub fn config(&self) -> Value {
        lock(&self.st).config.clone()
    }
}
