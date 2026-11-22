//! Driver qBittorrent via WebUI API v2 (cookie de session, ré-authentification automatique).
use crate::client::{AddRequest, AddSource, ClientError, TorrentClient};
use crate::paths::relative_to;
use proto::pb::{TorrentInfo, TorrentStatus};
use reqwest::{multipart, Client, RequestBuilder, Response, StatusCode, Url};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

pub struct QbitClient {
    http: Client,
    base: Url,
    user: String,
    password: String,
    root: PathBuf,
    /// Génération d'authentification : sérialise les logins concurrents (un seul ré-login par expiration).
    auth_gen: tokio::sync::Mutex<u64>,
    /// Messages de tracker déjà récupérés (infohash -> (quand, message)) : un appel par torrent est coûteux.
    tracker_msgs: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>,
}

// Le mot de passe ne doit jamais apparaître dans un log.
impl std::fmt::Debug for QbitClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QbitClient")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

/// Mappe les états qBittorrent vers l'enum stable du protocole.
pub fn map_state(s: &str) -> TorrentStatus {
    match s {
        "error" | "missingFiles" => TorrentStatus::Error,
        "uploading" | "stalledUP" | "forcedUP" => TorrentStatus::Seeding,
        "pausedUP" | "stoppedUP" | "pausedDL" | "stoppedDL" => TorrentStatus::Paused,
        "checkingUP" | "checkingDL" | "checkingResumeData" | "allocating" | "moving" => {
            TorrentStatus::Checking
        }
        "downloading" | "forcedDL" | "metaDL" | "forcedMetaDL" => TorrentStatus::Downloading,
        "stalledDL" => TorrentStatus::Stalled,
        "queuedUP" | "queuedDL" => TorrentStatus::Queued,
        _ => TorrentStatus::Unspecified,
    }
}

/// qBittorrent n'expose pas de texte d'erreur : seuls l'état et le tracker en disent plus.
pub fn state_message(state: &str) -> Option<&'static str> {
    match state {
        "missingFiles" => Some(
            "missingFiles : qBittorrent ne trouve pas les fichiers de ce torrent sur le disque",
        ),
        "error" => Some(
            "I/O error : qBittorrent signale une erreur de lecture ou d'écriture sur ce torrent",
        ),
        _ => None,
    }
}

/// États pour lesquels un tracker injoignable ou qui rejette le torrent mérite d'être signalé.
fn wants_tracker_message(state: &str) -> bool {
    matches!(
        state,
        "downloading"
            | "stalledDL"
            | "metaDL"
            | "forcedDL"
            | "uploading"
            | "stalledUP"
            | "forcedUP"
    )
}

/// Parmi les trackers d'un torrent (réponse de `torrents/trackers`), le message du premier qui ne fonctionne pas.
/// Les pseudo-trackers (DHT, PeX, LSD) sont ignorés ; si un tracker fonctionne, il n'y a rien à signaler.
pub fn failing_tracker_message(trackers: &serde_json::Value) -> Option<String> {
    let list: Vec<&serde_json::Value> = trackers
        .as_array()?
        .iter()
        .filter(|t| t["url"].as_str().is_some_and(|u| u.contains("://")))
        .collect();
    if list.iter().any(|t| t["status"].as_i64() == Some(2)) {
        return None; // « Working »
    }
    list.iter()
        .filter(|t| t["status"].as_i64() == Some(4)) // « Not working »
        .filter_map(|t| t["msg"].as_str())
        .find(|m| !m.trim().is_empty())
        .map(crate::client::clean_message)
}

const TRACKER_MSG_TTL: std::time::Duration = std::time::Duration::from_secs(300);
/// Au plus ce nombre d'appels `torrents/trackers` par interrogation : un parc entier ne doit pas les déclencher d'un coup.
const TRACKER_MSG_BUDGET: usize = 8;

#[derive(Deserialize)]
struct Raw {
    hash: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    save_path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    dlspeed: u64,
    #[serde(default)]
    upspeed: u64,
    /// Pairs connectés (seeds / leechers) et essaim annoncé par le tracker (complets / incomplets).
    #[serde(default)]
    num_seeds: u32,
    #[serde(default)]
    num_leechs: u32,
    #[serde(default)]
    num_complete: i64,
    #[serde(default)]
    num_incomplete: i64,
    /// URL du tracker courant (contient le passkey d'un tracker privé : seul l'hôte sort du nœud).
    #[serde(default)]
    tracker: String,
}

#[derive(Deserialize)]
struct MainData {
    server_state: ServerState,
}
#[derive(Deserialize)]
struct ServerState {
    free_space_on_disk: u64,
}

/// qBittorrent répond 403 (anciennes versions) ou 401 (récentes) quand la session manque ou expire.
fn is_auth_failure(s: StatusCode) -> bool {
    matches!(s, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED)
}

fn http_err(e: reqwest::Error) -> ClientError {
    // `without_url` : l'URL ne doit pas fuiter dans les messages.
    ClientError::Http(e.without_url().to_string())
}

impl QbitClient {
    /// `base_url` doit pointer sur la boucle locale : l'API du client n'est jamais exposée.
    pub fn new(
        base_url: &str,
        user: &str,
        password: &str,
        root: PathBuf,
    ) -> Result<Self, ClientError> {
        let base = crate::client::parse_local_url(base_url)?;
        let http = Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_err)?;
        Ok(Self {
            http,
            base,
            user: user.to_owned(),
            password: password.to_owned(),
            root,
            auth_gen: tokio::sync::Mutex::new(0),
            tracker_msgs: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn url(&self, path: &str) -> Result<Url, ClientError> {
        self.base
            .join(path)
            .map_err(|e| ClientError::Protocol(e.to_string()))
    }

    async fn login(&self) -> Result<(), ClientError> {
        let r = self
            .http
            .post(self.url("api/v2/auth/login")?)
            .form(&[("username", &self.user), ("password", &self.password)])
            .send()
            .await
            .map_err(http_err)?;
        // Anciennes versions : 200 + corps « Ok. » ; récentes : 204 sans corps. « Fails. » = refus.
        let status = r.status();
        let body = r.text().await.map_err(http_err)?;
        let ok = status == StatusCode::NO_CONTENT || (status.is_success() && body.trim() == "Ok.");
        if ok {
            Ok(())
        } else {
            Err(ClientError::Auth)
        }
    }

    /// Envoie la requête ; sur 403 (session expirée / première requête) se ré-authentifie une fois.
    async fn send(
        &self,
        build: impl Fn() -> Result<RequestBuilder, ClientError>,
    ) -> Result<Response, ClientError> {
        let generation = *self.auth_gen.lock().await;
        let r = build()?.send().await.map_err(http_err)?;
        if !is_auth_failure(r.status()) {
            return Ok(r);
        }
        {
            // Si une autre requête s'est déjà ré-authentifiée depuis, on réutilise sa session.
            let mut g = self.auth_gen.lock().await;
            if *g == generation {
                self.login().await?;
                *g += 1;
            }
        }
        let r = build()?.send().await.map_err(http_err)?;
        if is_auth_failure(r.status()) {
            return Err(ClientError::Auth);
        }
        Ok(r)
    }

    async fn post_form(
        &self,
        path: &str,
        form: &[(&str, String)],
    ) -> Result<Response, ClientError> {
        self.send(|| Ok(self.http.post(self.url(path)?).form(form)))
            .await
    }

    async fn ok(r: Response) -> Result<(), ClientError> {
        if r.status().is_success() {
            Ok(())
        } else {
            Err(ClientError::Protocol(format!("statut HTTP {}", r.status())))
        }
    }

    async fn fetch_tracker_message(&self, hash: &str) -> Result<String, ClientError> {
        let v: serde_json::Value = self
            .send(|| {
                let mut u = self.url("api/v2/torrents/trackers")?;
                u.query_pairs_mut().append_pair("hash", hash);
                Ok(self.http.get(u))
            })
            .await?
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        Ok(failing_tracker_message(&v).unwrap_or_default())
    }

    /// qBittorrent v5 a renommé pause/resume en stop/start : on essaie le nom v4 puis v5.
    async fn post_either(&self, v4: &str, v5: &str, hash: &str) -> Result<(), ClientError> {
        let form = [("hashes", hash.to_owned())];
        let r = self.post_form(v4, &form).await?;
        if r.status() == StatusCode::NOT_FOUND {
            return Self::ok(self.post_form(v5, &form).await?).await;
        }
        Self::ok(r).await
    }
}

#[async_trait::async_trait]
impl TorrentClient for QbitClient {
    fn kind(&self) -> &'static str {
        "qbittorrent"
    }

    async fn list(&self) -> Result<Vec<TorrentInfo>, ClientError> {
        let r = self
            .send(|| Ok(self.http.get(self.url("api/v2/torrents/info")?)))
            .await?;
        if !r.status().is_success() {
            return Err(ClientError::Protocol(format!("statut HTTP {}", r.status())));
        }
        let raw: Vec<Raw> = r
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        // Messages de tracker, uniquement pour les torrents actifs sans tracker qui fonctionne (borné + mis en cache).
        let mut tmsg: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut budget = TRACKER_MSG_BUDGET;
        for t in &raw {
            if !(wants_tracker_message(&t.state) && t.tracker.is_empty()) {
                continue;
            }
            let h = t.hash.to_ascii_lowercase();
            let cached = self
                .tracker_msgs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&h)
                .filter(|(at, _)| at.elapsed() < TRACKER_MSG_TTL)
                .map(|(_, m)| m.clone());
            let msg = match cached {
                Some(m) => m,
                None if budget == 0 => continue,
                None => {
                    budget -= 1;
                    let m = self
                        .fetch_tracker_message(&t.hash)
                        .await
                        .unwrap_or_default();
                    self.tracker_msgs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(h.clone(), (std::time::Instant::now(), m.clone()));
                    m
                }
            };
            if !msg.is_empty() {
                tmsg.insert(h, msg);
            }
        }
        // Les entrées de cache des torrents disparus ne s'accumulent pas.
        self.tracker_msgs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, (at, _)| at.elapsed() < TRACKER_MSG_TTL * 2);
        Ok(raw
            .into_iter()
            .map(|t| TorrentInfo {
                message: state_message(&t.state).map_or_else(
                    || {
                        tmsg.get(&t.hash.to_ascii_lowercase())
                            .cloned()
                            .unwrap_or_default()
                    },
                    str::to_owned,
                ),
                infohash: t.hash.to_ascii_lowercase(),
                name: t.name,
                status: map_state(&t.state) as i32,
                progress: t.progress as f32,
                save_subdir: relative_to(&self.root, &t.save_path)
                    .unwrap_or_else(|| crate::paths::OUTSIDE_ROOT.to_owned()),
                size_bytes: t.size,
                dl_rate: t.dlspeed,
                ul_rate: t.upspeed,
                peers_connected: t.num_seeds + t.num_leechs,
                // qBittorrent ne distingue pas « connectés » et « actifs » : on déduit l'activité des débits.
                peers_active: if t.dlspeed > 0 || t.upspeed > 0 {
                    t.num_seeds + t.num_leechs
                } else {
                    0
                },
                swarm_seeds: u32::try_from(t.num_complete.max(0)).unwrap_or(0),
                swarm_leechers: u32::try_from(t.num_incomplete.max(0)).unwrap_or(0),
                tracker: crate::client::tracker_host(&t.tracker),
            })
            .collect())
    }

    async fn add(&self, req: AddRequest) -> Result<(), ClientError> {
        let save = req.save_path.to_string_lossy().into_owned();
        let paused = req.paused.to_string();
        let r = self
            .send(|| {
                let form = match &req.source {
                    AddSource::Magnet(m) => multipart::Form::new().text("urls", m.clone()),
                    AddSource::File(data) => multipart::Form::new().part(
                        "torrents",
                        multipart::Part::bytes(data.clone())
                            .file_name("nxt.torrent")
                            .mime_str("application/x-bittorrent")
                            .map_err(|e| ClientError::Protocol(e.to_string()))?,
                    ),
                };
                let form = form
                    .text("savepath", save.clone())
                    .text("paused", paused.clone()) // v4
                    .text("stopped", paused.clone()); // v5
                Ok(self
                    .http
                    .post(self.url("api/v2/torrents/add")?)
                    .multipart(form))
            })
            .await?;
        let status = r.status();
        let body = r.text().await.map_err(http_err)?;
        if status.is_success() && body.trim() != "Fails." {
            Ok(())
        } else {
            Err(ClientError::Protocol("ajout refusé par le client".into()))
        }
    }

    async fn pause(&self, h: &str) -> Result<(), ClientError> {
        self.post_either("api/v2/torrents/pause", "api/v2/torrents/stop", h)
            .await
    }

    async fn resume(&self, h: &str) -> Result<(), ClientError> {
        self.post_either("api/v2/torrents/resume", "api/v2/torrents/start", h)
            .await
    }

    async fn recheck(&self, h: &str) -> Result<(), ClientError> {
        Self::ok(
            self.post_form("api/v2/torrents/recheck", &[("hashes", h.to_owned())])
                .await?,
        )
        .await
    }

    async fn set_torrent_limits(&self, h: &str, dl: u64, ul: u64) -> Result<(), ClientError> {
        // 0 = illimité pour qBittorrent également.
        let cap = |v: u64| i64::try_from(v).unwrap_or(i64::MAX).to_string();
        Self::ok(
            self.post_form(
                "api/v2/torrents/setDownloadLimit",
                &[("hashes", h.to_owned()), ("limit", cap(dl))],
            )
            .await?,
        )
        .await?;
        Self::ok(
            self.post_form(
                "api/v2/torrents/setUploadLimit",
                &[("hashes", h.to_owned()), ("limit", cap(ul))],
            )
            .await?,
        )
        .await
    }

    async fn export_torrent(&self, h: &str) -> Result<Vec<u8>, ClientError> {
        // qBittorrent >= 4.5.
        let r = self
            .send(|| {
                let mut u = self.url("api/v2/torrents/export")?;
                u.query_pairs_mut().append_pair("hash", h);
                Ok(self.http.get(u))
            })
            .await?;
        if !r.status().is_success() {
            return Err(ClientError::Protocol(format!(
                "export impossible (statut HTTP {})",
                r.status()
            )));
        }
        r.bytes().await.map(|b| b.to_vec()).map_err(http_err)
    }

    async fn add_tracker(&self, h: &str, url: &str) -> Result<(), ClientError> {
        // qBittorrent ignore les doublons : idempotent d'office.
        let form = [("hash", h.to_owned()), ("urls", url.to_owned())];
        Self::ok(self.post_form("api/v2/torrents/addTrackers", &form).await?).await?;
        // Annonce immédiate au nouveau tracker (au mieux : absent des versions anciennes).
        let _ = self
            .post_form("api/v2/torrents/reannounce", &[("hashes", h.to_owned())])
            .await;
        Ok(())
    }

    async fn remove(&self, h: &str, delete_files: bool) -> Result<(), ClientError> {
        let form = [
            ("hashes", h.to_owned()),
            ("deleteFiles", delete_files.to_string()),
        ];
        Self::ok(self.post_form("api/v2/torrents/delete", &form).await?).await
    }

    async fn set_limits(&self, dl: u64, ul: u64) -> Result<(), ClientError> {
        let cap = |v: u64| i64::try_from(v).unwrap_or(i64::MAX).to_string();
        Self::ok(
            self.post_form("api/v2/transfer/setDownloadLimit", &[("limit", cap(dl))])
                .await?,
        )
        .await?;
        Self::ok(
            self.post_form("api/v2/transfer/setUploadLimit", &[("limit", cap(ul))])
                .await?,
        )
        .await
    }

    async fn disk_free(&self) -> Result<u64, ClientError> {
        let r = self
            .send(|| Ok(self.http.get(self.url("api/v2/sync/maindata")?)))
            .await?;
        let d: MainData = r
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        Ok(d.server_state.free_space_on_disk)
    }

    async fn version(&self) -> Result<String, ClientError> {
        let r = self
            .send(|| Ok(self.http.get(self.url("api/v2/app/version")?)))
            .await?;
        Ok(r.text().await.map_err(http_err)?.trim().to_owned())
    }

    async fn health(&self) -> Result<proto::pb::NodeHealth, ClientError> {
        use proto::pb::Reachability;
        let info: serde_json::Value = self
            .send(|| Ok(self.http.get(self.url("api/v2/transfer/info")?)))
            .await?
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        let prefs: serde_json::Value = self
            .send(|| Ok(self.http.get(self.url("api/v2/app/preferences")?)))
            .await?
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        // « connected » : des connexions entrantes aboutissent ; « firewalled » : aucune (NAT/pare-feu) ;
        // « disconnected » : aucun pair du tout.
        let incoming = match info["connection_status"].as_str() {
            Some("connected") => Reachability::Open,
            Some("firewalled" | "disconnected") => Reachability::Closed,
            _ => Reachability::Unknown,
        };
        Ok(proto::pb::NodeHealth {
            known: true,
            incoming: incoming as i32,
            listen_port: u32::try_from(prefs["listen_port"].as_u64().unwrap_or(0)).unwrap_or(0),
            dht_nodes: u32::try_from(info["dht_nodes"].as_u64().unwrap_or(0)).unwrap_or(0),
            connections: 0, // sans total fiable côté client ; la somme par torrent est calculée par le master
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping() {
        use TorrentStatus::*;
        for (s, want) in [
            ("downloading", Downloading),
            ("metaDL", Downloading),
            ("stalledUP", Seeding),
            ("uploading", Seeding),
            ("pausedUP", Paused),
            ("stoppedDL", Paused),
            ("checkingUP", Checking),
            ("error", Error),
            ("missingFiles", Error),
            ("stalledDL", Stalled),
            ("queuedDL", Queued),
            ("???", Unspecified),
        ] {
            assert_eq!(map_state(s), want, "{s}");
        }
    }

    #[test]
    fn refuses_non_loopback() {
        let r = PathBuf::from(".");
        assert!(QbitClient::new("http://192.168.1.10:8080", "a", "b", r.clone()).is_err());
        assert!(QbitClient::new("http://example.com", "a", "b", r.clone()).is_err());
        assert!(QbitClient::new("http://127.0.0.1:8080", "a", "b", r.clone()).is_ok());
        assert!(QbitClient::new("http://[::1]:8080", "a", "b", r.clone()).is_ok());
        assert!(QbitClient::new("http://localhost:8080", "a", "b", r).is_ok());
    }
}
