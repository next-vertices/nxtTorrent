//! Driver Deluge (Web UI JSON-RPC `/json`, Deluge 2.x). Le Web UI n'a pas de compte : un seul mot de
//! passe ; il doit en outre être connecté au démon (`web.connect`), ce que le driver fait seul.
use crate::client::{parse_local_url, AddRequest, AddSource, ClientError, TorrentClient};
use crate::paths::{relative_to, OUTSIDE_ROOT};
use proto::pb::{TorrentInfo, TorrentStatus};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub struct DelugeClient {
    http: Client,
    url: Url,
    password: String,
    root: PathBuf,
    next_id: AtomicU64,
    /// Génération d'authentification : un seul ré-login par expiration de session.
    auth_gen: tokio::sync::Mutex<u64>,
}

impl std::fmt::Debug for DelugeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelugeClient")
            .field("url", &self.url.as_str())
            .finish_non_exhaustive()
    }
}

pub fn map_state(s: &str) -> TorrentStatus {
    match s {
        "Downloading" => TorrentStatus::Downloading,
        "Seeding" => TorrentStatus::Seeding,
        "Paused" => TorrentStatus::Paused,
        "Checking" | "Allocating" | "Moving" => TorrentStatus::Checking,
        "Queued" => TorrentStatus::Queued,
        "Error" => TorrentStatus::Error,
        _ => TorrentStatus::Unspecified,
    }
}

/// Message d'un torrent Deluge : le champ `message` (« OK » quand tout va bien), sinon l'état du tracker s'il
/// annonce une erreur, sinon une phrase générique si l'état est « Error ». Toujours nettoyé (aucune URL).
pub fn pick_message(state: &str, message: &str, tracker_status: &str) -> String {
    let m = message.trim();
    if !m.is_empty() && !m.eq_ignore_ascii_case("ok") {
        return crate::client::clean_message(m);
    }
    let ts = tracker_status.trim();
    if ts.to_ascii_lowercase().starts_with("error") {
        return crate::client::clean_message(ts);
    }
    if state == "Error" {
        return "Deluge signale une erreur sur ce torrent (sans détail)".to_owned();
    }
    String::new()
}

fn http_err(e: reqwest::Error) -> ClientError {
    ClientError::Http(e.without_url().to_string())
}

/// Deluge exprime ses limites en KiB/s ; -1 = illimité.
fn kib_limit(bytes: u64) -> f64 {
    if bytes == 0 {
        -1.0
    } else {
        bytes.div_ceil(1024) as f64
    }
}

impl DelugeClient {
    pub fn new(base_url: &str, password: &str, root: PathBuf) -> Result<Self, ClientError> {
        let base = parse_local_url(base_url)?;
        let url = base
            .join("json")
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        let http = Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_err)?;
        Ok(Self {
            http,
            url,
            password: password.to_owned(),
            root,
            next_id: AtomicU64::new(1),
            auth_gen: tokio::sync::Mutex::new(0),
        })
    }

    /// Un appel JSON-RPC brut. `Err(Auth)` si la session est absente/expirée.
    async fn raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let r = self
            .http
            .post(self.url.clone())
            .json(&json!({ "method": method, "params": params, "id": id }))
            .send()
            .await
            .map_err(http_err)?;
        if !r.status().is_success() {
            return Err(ClientError::Protocol(format!("statut HTTP {}", r.status())));
        }
        let v: Value = r
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        if let Some(code) = v["error"]["code"].as_i64() {
            // Code 1 : « Not authenticated ». Code 2 : « Unknown method » — un Web UI fraîchement démarré (ou dont la
            // session n'est pas reliée au démon) ne connaît pas encore les méthodes `daemon.*` : on se (re)connecte
            // puis on réessaie. Une méthode réellement inconnue échoue de nouveau après la connexion.
            if code == 1 || code == 2 {
                return Err(ClientError::Auth);
            }
            let msg = v["error"]["message"]
                .as_str()
                .unwrap_or("erreur")
                .lines()
                .next()
                .unwrap_or("erreur");
            return Err(ClientError::Protocol(format!("deluge: {msg}")));
        }
        Ok(v["result"].clone())
    }

    async fn login(&self) -> Result<(), ClientError> {
        if self.raw("auth.login", json!([self.password])).await? != json!(true) {
            return Err(ClientError::Auth);
        }
        // Le Web UI doit être relié à un démon.
        if self.raw("web.connected", json!([])).await? != json!(true) {
            let hosts = self.raw("web.get_hosts", json!([])).await?;
            let id = hosts[0][0].as_str().ok_or_else(|| {
                ClientError::Protocol("aucun démon Deluge configuré dans le Web UI".into())
            })?;
            self.raw("web.connect", json!([id])).await?;
        }
        Ok(())
    }

    /// Appel avec ré-authentification automatique (sérialisée entre appels concurrents).
    async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let generation = *self.auth_gen.lock().await;
        match self.raw(method, params.clone()).await {
            Err(ClientError::Auth) => {}
            other => return other,
        }
        {
            let mut g = self.auth_gen.lock().await;
            if *g == generation {
                self.login().await?;
                *g += 1;
            }
        }
        self.raw(method, params).await
    }
}

#[async_trait::async_trait]
impl TorrentClient for DelugeClient {
    fn kind(&self) -> &'static str {
        "deluge"
    }

    async fn list(&self) -> Result<Vec<TorrentInfo>, ClientError> {
        let fields = [
            "name",
            "state",
            "progress",
            "save_path",
            "total_size",
            "download_payload_rate",
            "upload_payload_rate",
            "num_peers",
            "num_seeds",
            "total_peers",
            "total_seeds",
            "tracker_host",
            "message",
            "tracker_status",
            "total_uploaded",
            "all_time_download",
            "time_added",
        ];
        let r = self
            .call("core.get_torrents_status", json!([{}, fields]))
            .await?;
        let map = r
            .as_object()
            .ok_or_else(|| ClientError::Protocol("liste de torrents invalide".into()))?;
        Ok(map
            .iter()
            .map(|(hash, t)| TorrentInfo {
                infohash: hash.to_ascii_lowercase(),
                name: t["name"].as_str().unwrap_or_default().to_owned(),
                status: map_state(t["state"].as_str().unwrap_or_default()) as i32,
                // Deluge exprime la progression en pourcentage (0-100).
                progress: (t["progress"].as_f64().unwrap_or(0.0) / 100.0) as f32,
                save_subdir: relative_to(&self.root, t["save_path"].as_str().unwrap_or_default())
                    .unwrap_or_else(|| OUTSIDE_ROOT.to_owned()),
                size_bytes: t["total_size"].as_u64().unwrap_or(0),
                dl_rate: t["download_payload_rate"].as_u64().unwrap_or(0),
                ul_rate: t["upload_payload_rate"].as_u64().unwrap_or(0),
                message: pick_message(
                    t["state"].as_str().unwrap_or_default(),
                    t["message"].as_str().unwrap_or_default(),
                    t["tracker_status"].as_str().unwrap_or_default(),
                ),
                peers_connected: (t["num_peers"].as_u64().unwrap_or(0)
                    + t["num_seeds"].as_u64().unwrap_or(0)) as u32,
                peers_active: 0,
                swarm_seeds: t["total_seeds"].as_i64().unwrap_or(0).max(0) as u32,
                swarm_leechers: t["total_peers"].as_i64().unwrap_or(0).max(0) as u32,
                tracker: crate::client::tracker_host(
                    t["tracker_host"].as_str().unwrap_or_default(),
                ),
                uploaded: t["total_uploaded"].as_u64().unwrap_or(0),
                downloaded: t["all_time_download"].as_u64().unwrap_or(0),
                // Deluge donne la date en secondes (décimales possibles).
                added_on: t["time_added"].as_f64().map_or(0, |v| v.max(0.0) as u64),
            })
            .collect())
    }

    async fn add(&self, req: AddRequest) -> Result<(), ClientError> {
        let options = json!({ "download_location": req.save_path.to_string_lossy(), "add_paused": req.paused });
        // Résultat nul si le torrent existe déjà : succès (ajout idempotent).
        match &req.source {
            AddSource::Magnet(m) => {
                self.call("core.add_torrent_magnet", json!([m, options]))
                    .await
            }
            AddSource::File(data) => {
                self.call(
                    "core.add_torrent_file",
                    json!(["nxt.torrent", data_encoding::BASE64.encode(data), options]),
                )
                .await
            }
        }
        .map(|_| ())
    }

    async fn pause(&self, h: &str) -> Result<(), ClientError> {
        self.call("core.pause_torrents", json!([[h]]))
            .await
            .map(|_| ())
    }

    async fn resume(&self, h: &str) -> Result<(), ClientError> {
        self.call("core.resume_torrents", json!([[h]]))
            .await
            .map(|_| ())
    }

    async fn recheck(&self, h: &str) -> Result<(), ClientError> {
        self.call("core.force_recheck", json!([[h]]))
            .await
            .map(|_| ())
    }

    async fn set_torrent_limits(&self, h: &str, dl: u64, ul: u64) -> Result<(), ClientError> {
        self.call(
            "core.set_torrent_options",
            json!([[h], { "max_download_speed": kib_limit(dl), "max_upload_speed": kib_limit(ul) }]),
        )
        .await
        .map(|_| ())
    }

    async fn add_tracker(&self, h: &str, url: &str) -> Result<(), ClientError> {
        // `set_torrent_trackers` REMPLACE la liste : on relit l'existante et on y ajoute la nouvelle.
        let st = self
            .call("core.get_torrent_status", json!([h, ["trackers"]]))
            .await?;
        let mut list: Vec<Value> = st["trackers"].as_array().cloned().unwrap_or_default();
        if list.iter().any(|t| t["url"].as_str() == Some(url)) {
            return Ok(());
        }
        list.push(json!({ "url": url, "tier": list.len() }));
        self.call("core.set_torrent_trackers", json!([h, list]))
            .await?;
        // Annonce immédiate au nouveau tracker (au mieux).
        let _ = self.call("core.force_reannounce", json!([[h]])).await;
        Ok(())
    }

    async fn remove(&self, h: &str, delete_files: bool) -> Result<(), ClientError> {
        // `remove_torrents` renvoie la liste des échecs ; un torrent absent n'est pas une erreur ici.
        self.call("core.remove_torrents", json!([[h], delete_files]))
            .await
            .map(|_| ())
    }

    async fn set_limits(&self, dl: u64, ul: u64) -> Result<(), ClientError> {
        self.call(
            "core.set_config",
            json!([{ "max_download_speed": kib_limit(dl), "max_upload_speed": kib_limit(ul) }]),
        )
        .await
        .map(|_| ())
    }

    async fn disk_free(&self) -> Result<u64, ClientError> {
        self.call("core.get_free_space", json!([self.root.to_string_lossy()]))
            .await?
            .as_u64()
            .ok_or_else(|| ClientError::Protocol("espace libre invalide".into()))
    }

    async fn health(&self) -> Result<proto::pb::NodeHealth, ClientError> {
        let st = self
            .call(
                "core.get_session_status",
                json!([["has_incoming_connections", "dht_nodes", "num_peers"]]),
            )
            .await?;
        let port = self
            .call("core.get_listen_port", json!([]))
            .await?
            .as_u64()
            .unwrap_or(0);
        let conns = st["num_peers"].as_u64().unwrap_or(0) as u32;
        let torrents = self.list().await?;
        let active = torrents.iter().any(|t| {
            t.status == TorrentStatus::Downloading as i32
                || t.status == TorrentStatus::Seeding as i32
        });
        let incoming = if st["has_incoming_connections"].as_bool().unwrap_or(false) {
            crate::client::reachability(active, conns, 1)
        } else {
            crate::client::reachability(active, conns, 0)
        };
        Ok(proto::pb::NodeHealth {
            known: true,
            incoming: incoming as i32,
            listen_port: port as u32,
            dht_nodes: st["dht_nodes"].as_u64().unwrap_or(0) as u32,
            connections: conns,
        })
    }

    async fn version(&self) -> Result<String, ClientError> {
        Ok(self
            .call("daemon.get_version", json!([]))
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping() {
        use TorrentStatus::*;
        for (s, w) in [
            ("Downloading", Downloading),
            ("Seeding", Seeding),
            ("Paused", Paused),
            ("Checking", Checking),
            ("Allocating", Checking),
            ("Queued", Queued),
            ("Error", Error),
            ("???", Unspecified),
        ] {
            assert_eq!(map_state(s), w, "{s}");
        }
    }

    #[test]
    fn error_messages_come_from_the_client_and_never_carry_a_url() {
        assert_eq!(pick_message("Seeding", "OK", "Announce OK"), "");
        assert_eq!(
            pick_message(
                "Seeding",
                "OK",
                "Error: Torrent not registered with this tracker"
            ),
            "Error: Torrent not registered with this tracker"
        );
        assert_eq!(
            pick_message(
                "Error",
                "Could not connect to http://t.org/SECRETPASS/announce",
                ""
            ),
            "Could not connect to [t.org]"
        );
        assert!(pick_message("Error", "OK", "").contains("sans détail"));
    }

    #[test]
    fn limits_unit() {
        assert_eq!(kib_limit(0), -1.0, "0 = illimité");
        assert_eq!(kib_limit(1024), 1.0);
        assert_eq!(kib_limit(1025), 2.0);
    }

    #[test]
    fn refuses_non_loopback_and_hides_password() {
        assert!(DelugeClient::new("http://10.0.0.5:8112", "x", ".".into()).is_err());
        let c = DelugeClient::new("http://127.0.0.1:8112", "secret", ".".into()).unwrap();
        assert!(!format!("{c:?}").contains("secret"));
    }
}
