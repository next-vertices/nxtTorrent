//! Faux ruTorrent : XML-RPC de rTorrent sur `/plugins/httprpc/action.php` + JSON `diskspace`.
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use xmlrpc_lite::{encode_fault, encode_response, parse_call, Value};

#[derive(Clone, Debug)]
pub struct MockR {
    pub hash: String, // MAJUSCULES, comme rTorrent
    pub active: bool,
    pub complete: bool,
    pub hashing: bool,
    pub dir: String,
    pub save_key: String,
}

#[derive(Default)]
struct St {
    t: BTreeMap<String, MockR>,
    limits: (i64, i64),
}

#[derive(Clone)]
pub struct MockRutorrent {
    pub addr: SocketAddr,
    st: Arc<Mutex<St>>,
}

fn lock(s: &Arc<Mutex<St>>) -> std::sync::MutexGuard<'_, St> {
    s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn xml(body: String) -> Response {
    ([("Content-Type", "text/xml")], body).into_response()
}

async fn action(State(s): State<Arc<Mutex<St>>>, body: String) -> Response {
    let Ok((method, p)) = parse_call(&body) else {
        return xml(encode_fault(-1, "XML invalide"));
    };
    let mut st = lock(&s);
    let text = |i: usize| {
        p.get(i)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let int = |b: bool| Value::Int(i64::from(b));
    xml(match method.as_str() {
        "system.client_version" => encode_response(&Value::Str("0.16.25-mock".into())),
        "d.multicall2" => {
            // Colonnes demandées dans l'ordre : hash, name, is_active, complete, size, bytes_done,
            // directory, down.rate, up.rate, hashing, custom(nxt_save), peers_connected, message, up.total, down.total.
            let rows =
                st.t.values()
                    .map(|t| {
                        Value::Array(vec![
                            Value::Str(t.hash.clone()),
                            Value::Str(format!("t-{}", &t.hash[..6])),
                            int(t.active),
                            int(t.complete),
                            Value::Int(1000),
                            Value::Int(if t.complete { 1000 } else { 0 }),
                            // d.directory inclut le nom pour un multi-fichiers : volontairement différent.
                            Value::Str(format!("{}/torrent-name", t.dir)),
                            Value::Int(0),
                            Value::Int(0),
                            int(t.hashing),
                            Value::Str(t.save_key.clone()),
                            Value::Int(0),             // d.peers_connected
                            Value::Str(String::new()), // d.message
                            Value::Int(9000),          // d.up.total
                            Value::Int(4000),          // d.down.total
                        ])
                    })
                    .collect();
            encode_response(&Value::Array(rows))
        }
        m @ ("load.start" | "load.normal" | "load.raw" | "load.raw_start") => {
            let raw = m.starts_with("load.raw");
            let hash = if raw {
                crate::torrent::hash_from_b64(&text(1))
            } else {
                crate::torrent::hash_from_magnet(&text(1))
            };
            let Some(hash) = hash.map(|h| h.to_uppercase()) else {
                return xml(encode_fault(-1, "torrent invalide"));
            };
            let dir = text(2)
                .strip_prefix("d.directory.set=")
                .unwrap_or_default()
                .to_owned();
            let key = text(3)
                .strip_prefix("d.custom.set=nxt_save,")
                .unwrap_or_default()
                .to_owned();
            st.t.entry(hash.clone()).or_insert(MockR {
                hash,
                active: m.ends_with("start"),
                complete: false,
                hashing: false,
                dir,
                save_key: key,
            });
            encode_response(&Value::Int(0))
        }
        m @ ("d.stop" | "d.start" | "d.check_hash" | "d.erase") => {
            let h = text(0);
            if m == "d.erase" {
                return xml(match st.t.remove(&h) {
                    Some(_) => encode_response(&Value::Int(0)),
                    None => encode_fault(-506, "Could not find info-hash."),
                });
            }
            match st.t.get_mut(&h) {
                None => encode_fault(-506, "Could not find info-hash."),
                Some(t) => {
                    match m {
                        "d.stop" => t.active = false,
                        "d.start" => t.active = true,
                        _ => t.hashing = true,
                    }
                    encode_response(&Value::Int(0))
                }
            }
        }
        "throttle.global_down.max_rate.set_kb" => {
            st.limits.0 = p.get(1).and_then(Value::as_i64).unwrap_or(-1);
            encode_response(&Value::Int(0))
        }
        "throttle.global_up.max_rate.set_kb" => {
            st.limits.1 = p.get(1).and_then(Value::as_i64).unwrap_or(-1);
            encode_response(&Value::Int(0))
        }
        _ => encode_fault(-3, "Method not defined"),
    })
}

async fn diskspace() -> Response {
    Json(json!({ "total": 2_000_000_000u64, "free": 1_500_000_000u64 })).into_response()
}

impl MockRutorrent {
    pub async fn start() -> Self {
        let st = Arc::new(Mutex::new(St::default()));
        let app = Router::new()
            .route("/plugins/httprpc/action.php", post(action))
            .route("/plugins/diskspace/action.php", get(diskspace))
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
    pub fn get(&self, hash_upper: &str) -> Option<MockR> {
        lock(&self.st).t.get(hash_upper).cloned()
    }
    pub fn limits(&self) -> (i64, i64) {
        lock(&self.st).limits
    }
    /// Simule la fin d'une vérification / d'un téléchargement.
    pub fn finish(&self, hash_upper: &str) {
        if let Some(t) = lock(&self.st).t.get_mut(hash_upper) {
            t.complete = true;
            t.hashing = false;
        }
    }
}
