//! Exécution du jeu de commandes FERMÉ. Chaque commande est validée localement :
//! le master n'est pas cru sur parole (nœud compromis / master compromis = impact minimal).
use crate::client::{
    magnet_infohash, normalize_infohash, AddRequest, AddSource, ClientError, TorrentClient,
};
use crate::paths::resolve_under;
use proto::pb::command::Kind;
use proto::pb::{add_cmd::Source, Command};
use std::path::PathBuf;
use std::sync::Arc;

pub struct Executor {
    client: Arc<dyn TorrentClient>,
    root: PathBuf,
    /// Hôte du master auquel cet agent est appairé : seule adresse que `AddTracker` peut désigner.
    tracker_host: Option<String>,
}

/// Une URL d'annonce acceptable : `http://<hôte>[:port]/announce/<nœud>/<jeton>`, ASCII imprimable, sans identifiants,
/// requête ni fragment, bornée.
pub fn announce_host(url: &str) -> Option<&str> {
    if url.len() > 300 || url.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
        return None;
    }
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest.split_once('/')?;
    if authority.is_empty()
        || authority.contains(['@', '?', '#', '\\'])
        || !path.starts_with("announce/")
        || path.contains(['?', '#'])
        || path.split('/').any(|s| s == ".." || s == ".")
    {
        return None;
    }
    // L'hôte, sans le port (IPv6 entre crochets conservé).
    let host = match authority.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty() => h,
        _ => authority,
    };
    Some(host.trim_matches(['[', ']']))
}

fn client_err(e: &ClientError) -> String {
    // Message court, sans chemin ni URL.
    match e {
        ClientError::Auth => "auth client torrent".to_owned(),
        ClientError::Http(_) => "client torrent injoignable".to_owned(),
        ClientError::Protocol(m) => format!("client torrent: {m}"),
    }
}

fn hash(s: &str) -> Result<String, String> {
    normalize_infohash(s).ok_or_else(|| "infohash invalide".to_owned())
}

impl Executor {
    pub fn new(client: Arc<dyn TorrentClient>, root: PathBuf) -> Self {
        Self {
            client,
            root,
            tracker_host: None,
        }
    }

    /// Autorise `AddTracker` vers ce seul hôte (celui du master).
    #[must_use]
    pub fn with_tracker_host(mut self, host: &str) -> Self {
        self.tracker_host = Some(host.to_ascii_lowercase());
        self
    }

    /// `.torrent` d'un torrent présent, revalidé ICI (taille, structure, infohash identique à celui demandé).
    pub async fn export(&self, infohash: &str) -> Result<Vec<u8>, String> {
        let ih = hash(infohash)?;
        let bytes = self
            .client
            .export_torrent(&ih)
            .await
            .map_err(|e| client_err(&e))?;
        let meta = proto::metainfo::parse(&bytes).map_err(|e| e.to_string())?;
        if meta.infohash != ih {
            return Err("le .torrent exporté ne correspond pas à l'infohash demandé".to_owned());
        }
        Ok(bytes)
    }

    fn checked_announce(&self, url: &str) -> Result<String, String> {
        let host = announce_host(url).ok_or("URL d'annonce refusée")?;
        match &self.tracker_host {
            Some(h) if h.eq_ignore_ascii_case(host) => Ok(url.to_owned()),
            _ => Err("URL d'annonce refusée : hôte différent de celui du master".to_owned()),
        }
    }

    /// Exécute une commande. Idempotent : rejouer une commande déjà appliquée est sans effet.
    pub async fn execute(&self, cmd: &Command) -> Result<(), String> {
        let c = &self.client;
        let r = match cmd.kind.as_ref().ok_or("commande vide")? {
            Kind::Add(a) => return self.add(a).await,
            Kind::Pause(t) => c.pause(&hash(&t.infohash)?).await,
            Kind::Resume(t) => c.resume(&hash(&t.infohash)?).await,
            Kind::Recheck(t) => c.recheck(&hash(&t.infohash)?).await,
            Kind::Remove(r) => c.remove(&hash(&r.infohash)?, r.delete_files).await,
            Kind::SetLimits(l) => c.set_limits(l.dl_limit, l.ul_limit).await,
            // Traitée par le tunnel (la réponse est un message à part) ; jamais exécutée ici.
            Kind::FetchMetainfo(_)
            | Kind::ListDir(_)
            | Kind::CreateTorrent(_)
            | Kind::CancelJob(_) => return Err("commande traitée par le tunnel".to_owned()),
            Kind::LinkTree(l) => {
                // Opérations de fichiers (potentiellement nombreuses) : hors du fil asynchrone.
                let (root, cmd) = (self.root.clone(), l.clone());
                return tokio::task::spawn_blocking(move || crate::links::link_tree(&root, &cmd))
                    .await
                    .map_err(|_| "liens interrompus".to_owned())?
                    .map(|_| ());
            }
            Kind::TorrentLimits(l) => {
                c.set_torrent_limits(&hash(&l.infohash)?, l.dl_limit, l.ul_limit)
                    .await
            }
            Kind::AddTracker(t) => {
                let url = self.checked_announce(&t.announce_url)?;
                c.add_tracker(&hash(&t.infohash)?, &url).await
            }
        };
        r.map_err(|e| client_err(&e))
    }

    async fn add(&self, a: &proto::pb::AddCmd) -> Result<(), String> {
        let (source, ih) = match a.source.as_ref().ok_or("source absente")? {
            Source::Magnet(m) => (
                AddSource::Magnet(m.clone()),
                magnet_infohash(m).ok_or("magnet invalide")?,
            ),
            Source::Infohash(h) => {
                let h = hash(h)?;
                (AddSource::Magnet(format!("magnet:?xt=urn:btih:{h}")), h)
            }
            // Le fichier est revalidé ICI (taille, bencode, infohash) : le master n'est pas cru sur parole.
            Source::TorrentFile(bytes) => {
                let meta = proto::metainfo::parse(bytes).map_err(|e| e.to_string())?;
                (AddSource::File(bytes.clone()), meta.infohash)
            }
        };
        // Chemin : relatif, sans `..`, symlinks canonicalisés sous la racine.
        let save_path = resolve_under(&self.root, &a.save_subdir)
            .map_err(|e| format!("save_subdir refusé: {e}"))?;
        // Idempotence (clé = infohash).
        let existing = self.client.list().await.map_err(|e| client_err(&e))?;
        if existing.iter().any(|t| t.infohash == ih) {
            return Ok(());
        }
        self.client
            .add(AddRequest {
                source,
                save_path,
                paused: a.start_paused,
            })
            .await
            .map_err(|e| client_err(&e))
    }
}
