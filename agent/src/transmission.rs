//! Driver Transmission (RPC JSON, `/transmission/rpc`, handshake `X-Transmission-Session-Id`).
use crate::client::{
    parse_local_url, reachability, tracker_host, AddRequest, AddSource, ClientError, TorrentClient,
};
use crate::paths::{relative_to, OUTSIDE_ROOT};
use proto::pb::{NodeHealth, TorrentInfo, TorrentStatus};
use reqwest::{Client, StatusCode, Url};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

pub struct TransmissionClient {
    http: Client,
    rpc: Url,
    user: String,
    password: String,
    root: PathBuf,
    session_id: tokio::sync::Mutex<String>,
}

// Le mot de passe ne doit jamais apparaître dans un log.
impl std::fmt::Debug for TransmissionClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransmissionClient")
            .field("rpc", &self.rpc.as_str())
            .finish_non_exhaustive()
    }
}

/// Statuts Transmission : 0 arrêté, 1/2 vérification, 3/5 file d'attente, 4 téléchargement, 6 seed.
pub fn map_status(status: i64, error: i64, stalled: bool) -> TorrentStatus {
    // `error` : 1 avertissement du tracker, 2 erreur du tracker, 3 erreur locale (disque, droits, fichiers).
    // Seule l'erreur locale est bloquante ; un tracker qui ne connaît pas le torrent (« not registered »)
    // n'empêche pas Transmission de télécharger via le DHT et les autres pairs.
    if error == 3 {
        return TorrentStatus::Error;
    }
    match status {
        0 => TorrentStatus::Paused,
        1 | 2 => TorrentStatus::Checking,
        3 | 5 => TorrentStatus::Queued,
        4 if stalled => TorrentStatus::Stalled,
        4 => TorrentStatus::Downloading,
        6 => TorrentStatus::Seeding,
        _ => TorrentStatus::Unspecified,
    }
}

fn http_err(e: reqwest::Error) -> ClientError {
    ClientError::Http(e.without_url().to_string())
}

/// Transmission exprime ses limites en kB/s ; 0 doit signifier « illimité ».
fn kbps(bytes: u64) -> u64 {
    bytes.div_ceil(1000)
}

impl TransmissionClient {
    /// `base_url` : racine du serveur (ex. `http://127.0.0.1:9091`), obligatoirement en boucle locale.
    pub fn new(
        base_url: &str,
        user: &str,
        password: &str,
        root: PathBuf,
    ) -> Result<Self, ClientError> {
        let base = parse_local_url(base_url)?;
        let rpc = base
            .join("transmission/rpc")
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_err)?;
        Ok(Self {
            http,
            rpc,
            user: user.to_owned(),
            password: password.to_owned(),
            root,
            session_id: tokio::sync::Mutex::new(String::new()),
        })
    }

    /// Appel RPC ; gère le 409 (identifiant de session à reprendre dans l'en-tête de la réponse).
    async fn call(&self, method: &str, arguments: Value) -> Result<Value, ClientError> {
        let body = json!({ "method": method, "arguments": arguments });
        for _ in 0..3 {
            let sid = self.session_id.lock().await.clone();
            let mut req = self
                .http
                .post(self.rpc.clone())
                .header("X-Transmission-Session-Id", sid)
                .json(&body);
            if !self.user.is_empty() {
                req = req.basic_auth(&self.user, Some(&self.password));
            }
            let r = req.send().await.map_err(http_err)?;
            match r.status() {
                StatusCode::CONFLICT => {
                    if let Some(v) = r
                        .headers()
                        .get("X-Transmission-Session-Id")
                        .and_then(|v| v.to_str().ok())
                    {
                        *self.session_id.lock().await = v.to_owned();
                    }
                }
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(ClientError::Auth),
                s if !s.is_success() => {
                    return Err(ClientError::Protocol(format!("statut HTTP {s}")))
                }
                _ => {
                    let v: Value = r
                        .json()
                        .await
                        .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
                    return match v["result"].as_str() {
                        Some("success") => Ok(v["arguments"].clone()),
                        Some(other) => Err(ClientError::Protocol(format!("transmission: {other}"))),
                        None => Err(ClientError::Protocol("réponse sans résultat".into())),
                    };
                }
            }
        }
        Err(ClientError::Protocol("handshake de session refusé".into()))
    }

    async fn on_hash(&self, method: &str, hash: &str, mut extra: Value) -> Result<(), ClientError> {
        extra["ids"] = json!([hash]);
        self.call(method, extra).await.map(|_| ())
    }
}

#[async_trait::async_trait]
impl TorrentClient for TransmissionClient {
    fn kind(&self) -> &'static str {
        "transmission"
    }

    async fn list(&self) -> Result<Vec<TorrentInfo>, ClientError> {
        let a = self
            .call(
                "torrent-get",
                json!({ "fields": ["hashString", "name", "status", "percentDone", "downloadDir",
                                    "totalSize", "rateDownload", "rateUpload", "error", "errorString", "isStalled",
                                    "peersConnected", "peersSendingToUs", "peersGettingFromUs", "trackerStats",
                                    "uploadedEver", "downloadedEver", "addedDate"] }),
            )
            .await?;
        let list = a["torrents"]
            .as_array()
            .ok_or_else(|| ClientError::Protocol("torrents absent".into()))?;
        Ok(list
            .iter()
            .filter_map(|t| {
                Some(TorrentInfo {
                    infohash: t["hashString"].as_str()?.to_ascii_lowercase(),
                    name: t["name"].as_str().unwrap_or_default().to_owned(),
                    status: map_status(
                        t["status"].as_i64().unwrap_or(-1),
                        t["error"].as_i64().unwrap_or(0),
                        t["isStalled"].as_bool().unwrap_or(false),
                    ) as i32,
                    progress: t["percentDone"].as_f64().unwrap_or(0.0) as f32,
                    save_subdir: relative_to(
                        &self.root,
                        t["downloadDir"].as_str().unwrap_or_default(),
                    )
                    .unwrap_or_else(|| OUTSIDE_ROOT.to_owned()),
                    size_bytes: t["totalSize"].as_u64().unwrap_or(0),
                    dl_rate: t["rateDownload"].as_u64().unwrap_or(0),
                    ul_rate: t["rateUpload"].as_u64().unwrap_or(0),
                    peers_connected: t["peersConnected"].as_u64().unwrap_or(0) as u32,
                    peers_active: (t["peersSendingToUs"].as_u64().unwrap_or(0)
                        + t["peersGettingFromUs"].as_u64().unwrap_or(0))
                        as u32,
                    // Essaim annoncé : la valeur la plus haute parmi les trackers (-1 = inconnu).
                    swarm_seeds: t["trackerStats"].as_array().map_or(0, |v| {
                        v.iter()
                            .filter_map(|x| x["seederCount"].as_u64())
                            .max()
                            .unwrap_or(0)
                    }) as u32,
                    swarm_leechers: t["trackerStats"].as_array().map_or(0, |v| {
                        v.iter()
                            .filter_map(|x| x["leecherCount"].as_u64())
                            .max()
                            .unwrap_or(0)
                    }) as u32,
                    tracker: t["trackerStats"]
                        .as_array()
                        .and_then(|v| v.first())
                        .and_then(|x| x["host"].as_str().or_else(|| x["announce"].as_str()))
                        .map(tracker_host)
                        .unwrap_or_default(),
                    uploaded: t["uploadedEver"].as_u64().unwrap_or(0),
                    downloaded: t["downloadedEver"].as_u64().unwrap_or(0),
                    added_on: t["addedDate"].as_u64().unwrap_or(0),
                    message: if t["error"].as_i64().unwrap_or(0) != 0 {
                        crate::client::clean_message(t["errorString"].as_str().unwrap_or_default())
                    } else {
                        String::new()
                    },
                })
            })
            .collect())
    }

    async fn add(&self, req: AddRequest) -> Result<(), ClientError> {
        // `torrent-duplicate` est un succès : l'ajout est idempotent.
        let mut args =
            json!({ "download-dir": req.save_path.to_string_lossy(), "paused": req.paused });
        match &req.source {
            AddSource::Magnet(m) => args["filename"] = json!(m),
            // Transmission reçoit le contenu du .torrent en base64 (`metainfo`).
            AddSource::File(data) => args["metainfo"] = json!(data_encoding::BASE64.encode(data)),
        }
        self.call("torrent-add", args).await.map(|_| ())
    }

    async fn pause(&self, h: &str) -> Result<(), ClientError> {
        self.on_hash("torrent-stop", h, json!({})).await
    }

    async fn resume(&self, h: &str) -> Result<(), ClientError> {
        // « now » : démarre sans passer par la file d'attente.
        self.on_hash("torrent-start-now", h, json!({})).await
    }

    async fn recheck(&self, h: &str) -> Result<(), ClientError> {
        self.on_hash("torrent-verify", h, json!({})).await
    }

    async fn set_torrent_limits(&self, h: &str, dl: u64, ul: u64) -> Result<(), ClientError> {
        self.on_hash(
            "torrent-set",
            h,
            json!({
                "downloadLimit": kbps(dl), "downloadLimited": dl > 0,
                "uploadLimit": kbps(ul), "uploadLimited": ul > 0,
            }),
        )
        .await
    }

    async fn export_torrent(&self, h: &str) -> Result<Vec<u8>, ClientError> {
        // Champ `metainfo` (base64) : absent de certaines versions (rejeté par 4.1.4 testé) ; l'API ne donne sinon
        // que le CHEMIN du .torrent dans le dossier de configuration, que l'agent ne lit pas.
        let unsupported = || {
            ClientError::Protocol("ce Transmission ne fournit pas le .torrent par son API".into())
        };
        let a = self
            .call("torrent-get", json!({ "ids": [h], "fields": ["metainfo"] }))
            .await
            .map_err(|_| unsupported())?;
        let b64 = a["torrents"][0]["metainfo"]
            .as_str()
            .ok_or_else(unsupported)?;
        data_encoding::BASE64
            .decode(b64.as_bytes())
            .map_err(|_| ClientError::Protocol("export illisible".into()))
    }

    async fn add_tracker(&self, h: &str, url: &str) -> Result<(), ClientError> {
        // Transmission refuse un doublon : on lit d'abord les annonces existantes.
        let a = self
            .call("torrent-get", json!({ "ids": [h], "fields": ["trackers"] }))
            .await?;
        let present = a["torrents"][0]["trackers"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x["announce"].as_str() == Some(url)));
        if present {
            return Ok(());
        }
        self.on_hash("torrent-set", h, json!({ "trackerAdd": [url] }))
            .await?;
        // Un tracker ajouté après coup n'est pas annoncé tout de suite : on le demande (au mieux).
        let _ = self.on_hash("torrent-reannounce", h, json!({})).await;
        Ok(())
    }

    async fn remove(&self, h: &str, delete_files: bool) -> Result<(), ClientError> {
        self.on_hash(
            "torrent-remove",
            h,
            json!({ "delete-local-data": delete_files }),
        )
        .await
    }

    async fn set_limits(&self, dl: u64, ul: u64) -> Result<(), ClientError> {
        self.call(
            "session-set",
            json!({
                "speed-limit-down": kbps(dl), "speed-limit-down-enabled": dl > 0,
                "speed-limit-up": kbps(ul), "speed-limit-up-enabled": ul > 0,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn disk_free(&self) -> Result<u64, ClientError> {
        let a = self
            .call("free-space", json!({ "path": self.root.to_string_lossy() }))
            .await?;
        a["size-bytes"]
            .as_u64()
            .ok_or_else(|| ClientError::Protocol("size-bytes absent".into()))
    }

    async fn version(&self) -> Result<String, ClientError> {
        let a = self
            .call("session-get", json!({ "fields": ["version"] }))
            .await?;
        Ok(a["version"].as_str().unwrap_or_default().to_owned())
    }

    async fn health(&self) -> Result<NodeHealth, ClientError> {
        let port = self
            .call("session-get", json!({ "fields": ["peer-port"] }))
            .await?["peer-port"]
            .as_u64()
            .unwrap_or(0);
        let a = self
            .call(
                "torrent-get",
                json!({ "fields": ["status", "peersConnected", "peersFrom"] }),
            )
            .await?;
        let (mut conns, mut incoming, mut active) = (0u32, 0u32, false);
        for t in a["torrents"].as_array().map_or(&[][..], Vec::as_slice) {
            conns += t["peersConnected"].as_u64().unwrap_or(0) as u32;
            incoming += t["peersFrom"]["fromIncoming"].as_u64().unwrap_or(0) as u32;
            active |= matches!(t["status"].as_i64(), Some(4 | 6));
        }
        Ok(NodeHealth {
            known: true,
            incoming: reachability(active, conns, incoming) as i32,
            listen_port: port as u32,
            dht_nodes: 0, // Transmission n'expose pas la taille de sa table DHT
            connections: conns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        use TorrentStatus::*;
        assert_eq!(map_status(0, 0, false), Paused);
        assert_eq!(map_status(2, 0, false), Checking);
        assert_eq!(map_status(3, 0, false), Queued);
        assert_eq!(map_status(4, 0, false), Downloading);
        assert_eq!(map_status(4, 0, true), Stalled);
        assert_eq!(map_status(6, 0, false), Seeding);
        assert_eq!(
            map_status(6, 3, false),
            Error,
            "une erreur prime sur le statut"
        );
        assert_eq!(map_status(99, 0, false), Unspecified);
        // Erreur de tracker : le torrent continue de fonctionner, il n'est pas « en erreur ».
        assert_eq!(map_status(4, 2, false), Downloading);
        assert_eq!(map_status(6, 1, false), Seeding);
    }

    #[test]
    fn limits_unit_and_unlimited() {
        assert_eq!(kbps(0), 0);
        assert_eq!(kbps(1), 1);
        assert_eq!(kbps(1000), 1);
        assert_eq!(kbps(1001), 2);
    }

    #[test]
    fn refuses_non_loopback_and_hides_password() {
        assert!(TransmissionClient::new("http://192.168.1.2:9091", "a", "b", ".".into()).is_err());
        let c =
            TransmissionClient::new("http://127.0.0.1:9091", "a", "secret", ".".into()).unwrap();
        assert!(!format!("{c:?}").contains("secret"));
    }
}
