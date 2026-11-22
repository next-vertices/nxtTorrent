//! Driver ruTorrent / rTorrent : XML-RPC transporté par le plugin `httprpc` de ruTorrent
//! (`plugins/httprpc/action.php`). Espace disque via le plugin `diskspace` (JSON).
use crate::client::{parse_local_url, AddRequest, AddSource, ClientError, TorrentClient};
use crate::paths::{relative_to, OUTSIDE_ROOT};
use proto::pb::{TorrentInfo, TorrentStatus};
use reqwest::{Client, StatusCode, Url};
use std::path::PathBuf;
use std::time::Duration;
use xmlrpc_lite::Value;

/// Champ personnalisé rTorrent où l'on mémorise le dossier demandé à l'ajout : `d.directory` de
/// rTorrent inclut le nom du torrent pour les torrents multi-fichiers, ce qui fausserait `save_subdir`.
const SAVE_KEY: &str = "nxt_save";

pub struct RutorrentClient {
    http: Client,
    base: Url,
    user: String,
    password: String,
    root: PathBuf,
}

impl std::fmt::Debug for RutorrentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RutorrentClient")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

fn http_err(e: reqwest::Error) -> ClientError {
    ClientError::Http(e.without_url().to_string())
}

fn s(x: &str) -> Value {
    Value::Str(x.to_owned())
}

/// rTorrent : en cours de hachage -> Checking ; arrêté -> Paused ; actif complet -> Seeding.
/// Message d'un torrent rTorrent (`d.message`) : (bloquant ?, texte nettoyé). rTorrent préfixe les erreurs de
/// tracker par « Tracker: » : elles n'empêchent pas le transfert ; tout le reste (fichiers, disque) est bloquant.
pub fn classify_message(raw: &str) -> (bool, String) {
    let m = crate::client::clean_message(raw);
    let blocking = !m.is_empty() && !m.to_ascii_lowercase().starts_with("tracker:");
    (blocking, m)
}

pub fn map_state(hashing: i64, active: i64, complete: i64) -> TorrentStatus {
    if hashing != 0 {
        TorrentStatus::Checking
    } else if active == 0 {
        TorrentStatus::Paused
    } else if complete != 0 {
        TorrentStatus::Seeding
    } else {
        TorrentStatus::Downloading
    }
}

/// Les arguments sont passés à rTorrent sous forme de commandes : on refuse ce qui pourrait en
/// altérer l'analyse (virgule, guillemet, accolade, caractère de contrôle) plutôt que d'échapper.
fn safe_arg(p: &str) -> Result<&str, ClientError> {
    if p.chars()
        .any(|c| c.is_control() || matches!(c, ',' | '"' | '\'' | '{' | '}' | '$' | '\\' | ';'))
    {
        return Err(ClientError::Protocol(
            "chemin non supporté par rTorrent (caractère spécial)".into(),
        ));
    }
    Ok(p)
}

impl RutorrentClient {
    /// `base_url` : racine de ruTorrent (ex. `http://127.0.0.1:8080/`), obligatoirement en boucle locale.
    pub fn new(
        base_url: &str,
        user: &str,
        password: &str,
        root: PathBuf,
    ) -> Result<Self, ClientError> {
        let mut base = parse_local_url(base_url)?;
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_err)?;
        Ok(Self {
            http,
            base,
            user: user.to_owned(),
            password: password.to_owned(),
            root,
        })
    }

    fn url(&self, path: &str) -> Result<Url, ClientError> {
        self.base
            .join(path)
            .map_err(|e| ClientError::Protocol(e.to_string()))
    }

    fn with_auth(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.user.is_empty() {
            r
        } else {
            r.basic_auth(&self.user, Some(&self.password))
        }
    }

    async fn call(&self, method: &str, params: &[Value]) -> Result<Value, ClientError> {
        let r = self
            .with_auth(self.http.post(self.url("plugins/httprpc/action.php")?))
            .header("Content-Type", "text/xml")
            .body(xmlrpc_lite::encode_call(method, params))
            .send()
            .await
            .map_err(http_err)?;
        match r.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(ClientError::Auth),
            st if !st.is_success() => {
                return Err(ClientError::Protocol(format!("statut HTTP {st}")))
            }
            _ => {}
        }
        let body = r.text().await.map_err(http_err)?;
        xmlrpc_lite::parse_response(&body).map_err(|e| match e {
            xmlrpc_lite::Error::Fault { message, .. } => {
                ClientError::Protocol(format!("rtorrent: {message}"))
            }
            other => ClientError::Protocol(other.to_string()),
        })
    }

    async fn on_hash(&self, method: &str, hash: &str) -> Result<(), ClientError> {
        // rTorrent est sensible à la casse : les infohash y sont en majuscules.
        self.call(method, &[s(&hash.to_ascii_uppercase())])
            .await
            .map(|_| ())
    }
}

#[async_trait::async_trait]
impl TorrentClient for RutorrentClient {
    fn kind(&self) -> &'static str {
        "rutorrent"
    }

    async fn list(&self) -> Result<Vec<TorrentInfo>, ClientError> {
        let mut p = vec![s(""), s("main")];
        for f in [
            "d.hash=",
            "d.name=",
            "d.is_active=",
            "d.complete=",
            "d.size_bytes=",
            "d.bytes_done=",
            "d.directory=",
            "d.down.rate=",
            "d.up.rate=",
            "d.hashing=",
        ] {
            p.push(s(f));
        }
        p.push(s(&format!("d.custom={SAVE_KEY}")));
        p.push(s("d.peers_connected="));
        p.push(s("d.message="));
        let v = self.call("d.multicall2", &p).await?;
        let rows = v
            .as_array()
            .ok_or_else(|| ClientError::Protocol("liste invalide".into()))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let r = row.as_array()?;
                let int = |i: usize| r.get(i).and_then(Value::as_i64).unwrap_or(0);
                let text = |i: usize| {
                    r.get(i)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let (blocking, message) = classify_message(&text(12));
                let size = u64::try_from(int(4)).unwrap_or(0);
                let done = u64::try_from(int(5)).unwrap_or(0);
                let saved = text(10);
                let dir = if saved.is_empty() { text(6) } else { saved };
                Some(TorrentInfo {
                    infohash: text(0).to_ascii_lowercase(),
                    name: text(1),
                    status: if blocking {
                        TorrentStatus::Error
                    } else {
                        map_state(int(9), int(2), int(3))
                    } as i32,
                    progress: if size == 0 {
                        0.0
                    } else {
                        (done as f64 / size as f64).min(1.0) as f32
                    },
                    save_subdir: relative_to(&self.root, &dir)
                        .unwrap_or_else(|| OUTSIDE_ROOT.to_owned()),
                    size_bytes: size,
                    dl_rate: u64::try_from(int(7)).unwrap_or(0),
                    ul_rate: u64::try_from(int(8)).unwrap_or(0),
                    message,
                    peers_connected: u32::try_from(int(11)).unwrap_or(0),
                    // Essaim et tracker : non exposés simplement par rTorrent (0 / vide = inconnu).
                    peers_active: 0,
                    swarm_seeds: 0,
                    swarm_leechers: 0,
                    tracker: String::new(),
                })
            })
            .collect())
    }

    async fn add(&self, req: AddRequest) -> Result<(), ClientError> {
        let dir = req.save_path.to_string_lossy().into_owned();
        let dir = safe_arg(&dir)?;
        // `load.normal` / `load.start` : lien magnet ; `load.raw` / `load.raw_start` : contenu du .torrent.
        // « normal » / « raw » chargent sans démarrer (ajout en pause) ; « start » démarre.
        let (method, payload) = match &req.source {
            AddSource::Magnet(m) => (
                if req.paused {
                    "load.normal"
                } else {
                    "load.start"
                },
                s(m),
            ),
            AddSource::File(data) => (
                if req.paused {
                    "load.raw"
                } else {
                    "load.raw_start"
                },
                Value::Base64(data.clone()),
            ),
        };
        self.call(
            method,
            &[
                s(""),
                payload,
                s(&format!("d.directory.set={dir}")),
                s(&format!("d.custom.set={SAVE_KEY},{dir}")),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn pause(&self, h: &str) -> Result<(), ClientError> {
        self.on_hash("d.stop", h).await
    }

    async fn resume(&self, h: &str) -> Result<(), ClientError> {
        self.on_hash("d.start", h).await
    }

    async fn recheck(&self, h: &str) -> Result<(), ClientError> {
        self.on_hash("d.check_hash", h).await
    }

    async fn remove(&self, h: &str, delete_files: bool) -> Result<(), ClientError> {
        if delete_files {
            // rTorrent n'efface pas les données ; le faire passerait par une exécution de commande
            // côté client, ce que l'agent s'interdit. Refus explicite plutôt que fichiers conservés en silence.
            return Err(ClientError::Protocol(
                "ruTorrent: suppression des fichiers non supportée (utiliser delete_files=false)"
                    .into(),
            ));
        }
        match self.on_hash("d.erase", h).await {
            // Torrent déjà absent : suppression idempotente.
            Err(ClientError::Protocol(m))
                if m.contains("info-hash") || m.contains("Could not find") =>
            {
                Ok(())
            }
            other => other,
        }
    }

    async fn set_limits(&self, dl: u64, ul: u64) -> Result<(), ClientError> {
        // 0 = illimité chez rTorrent également.
        let kb = |b: u64| Value::Int(i64::try_from(b.div_ceil(1024)).unwrap_or(i64::MAX));
        self.call("throttle.global_down.max_rate.set_kb", &[s(""), kb(dl)])
            .await?;
        self.call("throttle.global_up.max_rate.set_kb", &[s(""), kb(ul)])
            .await
            .map(|_| ())
    }

    async fn disk_free(&self) -> Result<u64, ClientError> {
        let r = self
            .with_auth(self.http.get(self.url("plugins/diskspace/action.php")?))
            .send()
            .await
            .map_err(http_err)?;
        if !r.status().is_success() {
            return Err(ClientError::Protocol(
                "plugin diskspace indisponible".into(),
            ));
        }
        let v: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.without_url().to_string()))?;
        v["free"]
            .as_u64()
            .ok_or_else(|| ClientError::Protocol("espace libre absent".into()))
    }

    async fn version(&self) -> Result<String, ClientError> {
        let v = self.call("system.client_version", &[]).await?;
        Ok(format!("rTorrent {}", v.as_str().unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping() {
        use TorrentStatus::*;
        assert_eq!(map_state(1, 1, 0), Checking, "le hachage prime");
        assert_eq!(map_state(0, 0, 0), Paused);
        assert_eq!(map_state(0, 0, 1), Paused, "complet mais arrêté");
        assert_eq!(map_state(0, 1, 1), Seeding);
        assert_eq!(map_state(0, 1, 0), Downloading);
        // Messages : une erreur de tracker n'est pas bloquante, un problème de fichier l'est.
        assert_eq!(classify_message(""), (false, String::new()));
        let (b, m) = classify_message(
            "Tracker: [Failure reason \"Unregistered torrent\"] http://t.org/PASSKEYX/ann",
        );
        assert!(!b && !m.contains("PASSKEYX"), "{m}");
        assert!(classify_message("Could not create file: Permission denied").0);
    }

    #[test]
    fn rejects_paths_that_could_alter_rtorrent_command_parsing() {
        assert!(safe_arg("/downloads/movies/hd").is_ok());
        for bad in [
            "/d/a,b", "/d/\"x", "/d/{x}", "/d/$HOME", "/d/a;b", "/d/a\nb", "/d/it's",
        ] {
            assert!(safe_arg(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn base_url_gets_trailing_slash_and_loopback_only() {
        let c =
            RutorrentClient::new("http://127.0.0.1:8080/rutorrent", "", "", ".".into()).unwrap();
        assert_eq!(
            c.url("plugins/httprpc/action.php").unwrap().path(),
            "/rutorrent/plugins/httprpc/action.php"
        );
        assert!(RutorrentClient::new("http://nas.local/", "", "", ".".into()).is_err());
        let c = RutorrentClient::new("http://127.0.0.1:8080", "u", "secret", ".".into()).unwrap();
        assert!(!format!("{c:?}").contains("secret"));
    }
}
