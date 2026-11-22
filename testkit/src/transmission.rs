//! Faux Transmission RPC : handshake 409 `X-Transmission-Session-Id`, auth basique admin/adminadmin.
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct MockT {
    pub hash: String,
    pub status: i64,
    pub percent: f64,
    pub dir: String,
}

#[derive(Default)]
struct St {
    t: BTreeMap<String, MockT>,
    sid: u32,
    limits: (i64, bool, i64, bool),
}

#[derive(Clone)]
pub struct MockTransmission {
    pub addr: SocketAddr,
    st: Arc<Mutex<St>>,
}

fn lock(s: &Arc<Mutex<St>>) -> std::sync::MutexGuard<'_, St> {
    s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn ok(args: Value) -> Response {
    Json(json!({ "result": "success", "arguments": args })).into_response()
}

async fn rpc(State(s): State<Arc<Mutex<St>>>, h: HeaderMap, Json(req): Json<Value>) -> Response {
    // Basic admin:adminadmin = YWRtaW46YWRtaW5hZG1pbg==
    if h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        != Some("Basic YWRtaW46YWRtaW5hZG1pbg==")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut st = lock(&s);
    let want = format!("sid-{}", st.sid);
    if h.get("X-Transmission-Session-Id")
        .and_then(|v| v.to_str().ok())
        != Some(want.as_str())
    {
        return (
            StatusCode::CONFLICT,
            [("X-Transmission-Session-Id", want)],
            "409: Conflict",
        )
            .into_response();
    }
    let a = &req["arguments"];
    let ids: Vec<String> = a["ids"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    match req["method"].as_str().unwrap_or_default() {
        "session-get" => ok(json!({ "version": "4.1.4 (mock)", "peer-port": 51413 })),
        "free-space" => ok(json!({ "path": a["path"], "size-bytes": 987_654_321u64 })),
        "session-set" => {
            st.limits = (
                a["speed-limit-down"].as_i64().unwrap_or(0),
                a["speed-limit-down-enabled"].as_bool().unwrap_or(false),
                a["speed-limit-up"].as_i64().unwrap_or(0),
                a["speed-limit-up-enabled"].as_bool().unwrap_or(false),
            );
            ok(json!({}))
        }
        "torrent-get" => {
            let list: Vec<Value> = st
                .t
                .values()
                .map(|t| {
                    json!({ "hashString": t.hash, "name": format!("t-{}", &t.hash[..6]), "status": t.status,
                            "percentDone": t.percent, "downloadDir": t.dir, "totalSize": 1000,
                            "rateDownload": 0, "rateUpload": 0, "error": 0, "isStalled": false,
                            "peersConnected": 14, "peersSendingToUs": 2, "peersGettingFromUs": 1,
                            "peersFrom": { "fromIncoming": 3, "fromDht": 5 },
                            "trackerStats": [{ "host": "https://tracker.example.org:443",
                                               "announce": "https://tracker.example.org/SECRETPASSKEY/announce",
                                               "seederCount": 100, "leecherCount": 20 }],
                            "uploadedEver": 5000, "downloadedEver": 2000, "addedDate": 1_700_000_000 })
                })
                .collect();
            ok(json!({ "torrents": list }))
        }
        "torrent-add" => {
            let hash = a["metainfo"].as_str().map_or_else(
                || crate::torrent::hash_from_magnet(a["filename"].as_str().unwrap_or_default()),
                crate::torrent::hash_from_b64,
            );
            let Some(hash) = hash else {
                return Json(json!({ "result": "invalid or corrupt torrent file" }))
                    .into_response();
            };
            if st.t.contains_key(&hash) {
                return ok(json!({ "torrent-duplicate": { "hashString": hash } }));
            }
            let status = if a["paused"].as_bool().unwrap_or(false) {
                0
            } else {
                4
            };
            st.t.insert(
                hash.clone(),
                MockT {
                    hash: hash.clone(),
                    status,
                    percent: 0.0,
                    dir: a["download-dir"].as_str().unwrap_or_default().into(),
                },
            );
            ok(json!({ "torrent-added": { "hashString": hash } }))
        }
        m @ ("torrent-stop" | "torrent-start-now" | "torrent-verify" | "torrent-remove") => {
            for id in ids {
                match m {
                    "torrent-remove" => {
                        st.t.remove(&id);
                    }
                    _ => {
                        if let Some(t) = st.t.get_mut(&id) {
                            t.status = match m {
                                "torrent-stop" => 0,
                                "torrent-verify" => 2,
                                _ => {
                                    if t.percent >= 1.0 {
                                        6
                                    } else {
                                        4
                                    }
                                }
                            };
                        }
                    }
                }
            }
            ok(json!({}))
        }
        _ => Json(json!({ "result": "method name not recognized" })).into_response(),
    }
}

impl MockTransmission {
    pub async fn start() -> Self {
        let st = Arc::new(Mutex::new(St::default()));
        let app = Router::new()
            .route("/transmission/rpc", post(rpc))
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
    pub fn get(&self, hash: &str) -> Option<MockT> {
        lock(&self.st).t.get(hash).cloned()
    }
    /// Force un nouvel identifiant de session (le client reçoit un 409 et doit le reprendre).
    pub fn rotate_session(&self) {
        lock(&self.st).sid += 1;
    }
    pub fn limits(&self) -> (i64, bool, i64, bool) {
        lock(&self.st).limits
    }
}
