//! Abstraction du client torrent piloté par l'agent (qBittorrent d'abord).
use proto::pb::{NodeHealth, TorrentInfo};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("authentification refusée par le client torrent")]
    Auth,
    #[error("client torrent injoignable: {0}")]
    Http(String),
    #[error("réponse inattendue du client torrent: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone)]
pub enum AddSource {
    Magnet(String),
    /// Contenu d'un fichier .torrent (déjà validé par `proto::metainfo`).
    File(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct AddRequest {
    pub source: AddSource,
    /// Chemin déjà validé (sous la racine locale).
    pub save_path: PathBuf,
    pub paused: bool,
}

/// Les infohash passés ici sont déjà normalisés (40 hex minuscules).
#[async_trait::async_trait]
pub trait TorrentClient: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> &'static str;
    async fn list(&self) -> Result<Vec<TorrentInfo>, ClientError>;
    async fn add(&self, req: AddRequest) -> Result<(), ClientError>;
    async fn pause(&self, infohash: &str) -> Result<(), ClientError>;
    async fn resume(&self, infohash: &str) -> Result<(), ClientError>;
    async fn recheck(&self, infohash: &str) -> Result<(), ClientError>;
    async fn remove(&self, infohash: &str, delete_files: bool) -> Result<(), ClientError>;
    /// Limites globales en octets/s ; 0 = illimité.
    async fn set_limits(&self, dl: u64, ul: u64) -> Result<(), ClientError>;
    async fn disk_free(&self) -> Result<u64, ClientError>;
    async fn version(&self) -> Result<String, ClientError>;
    /// Plafonds de débit d'UN torrent, en octets/s (0 = illimité). Par défaut : non supporté.
    async fn set_torrent_limits(
        &self,
        _infohash: &str,
        _dl: u64,
        _ul: u64,
    ) -> Result<(), ClientError> {
        Err(ClientError::Protocol(
            "ce client torrent ne permet pas de limiter un torrent".into(),
        ))
    }
    /// Contenu du `.torrent` d'un torrent présent (ajouté par magnet : métadonnées obtenues du swarm).
    /// Par défaut : non supporté.
    async fn export_torrent(&self, _infohash: &str) -> Result<Vec<u8>, ClientError> {
        Err(ClientError::Protocol(
            "ce client torrent ne permet pas d'exporter le .torrent".into(),
        ))
    }
    /// Ajoute une URL d'annonce à un torrent existant (idempotent : déjà présente = succès).
    /// Par défaut : non supporté (le nœud ne peut alors pas servir de source à une réplication).
    async fn add_tracker(&self, _infohash: &str, _url: &str) -> Result<(), ClientError> {
        Err(ClientError::Protocol(
            "ce client torrent ne permet pas d'ajouter un tracker".into(),
        ))
    }
    /// Santé réseau du client (port joignable, DHT, connexions). Par défaut : inconnue (`known = false`).
    async fn health(&self) -> Result<NodeHealth, ClientError> {
        Ok(NodeHealth::default())
    }
}

/// Rend sûr un message venu d'un client torrent avant qu'il quitte le nœud : toute URL est remplacée par son
/// seul hôte (une URL de tracker privé contient le passkey), caractères de contrôle retirés, longueur bornée.
pub fn clean_message(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        // Début du schéma : on remonte sur les lettres qui précèdent « :// ».
        let start = rest[..i]
            .rfind(|c: char| !c.is_ascii_alphabetic())
            .map_or(0, |p| p + 1);
        let after = &rest[i + 3..];
        let end = after
            .find(|c: char| {
                c.is_whitespace() || matches!(c, ')' | '"' | '\'' | '>' | ',' | ';' | ']')
            })
            .unwrap_or(after.len());
        out.push_str(&rest[..start]);
        let host = tracker_host(&after[..end]);
        out.push_str(&format!(
            "[{}]",
            if host.is_empty() { "url" } else { &host }
        ));
        rest = &after[end..];
    }
    out.push_str(rest);
    out.chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

/// Joignabilité du port d'écoute, déduite d'observations (pas d'un test externe) :
/// - des connexions ENTRANTES ont été vues -> ouvert, sans ambiguïté ;
/// - aucune, alors qu'il y a de l'activité et assez de pairs connectés -> probablement fermé (NAT/pare-feu) ;
/// - sinon (nœud au repos, peu de pairs) -> on ne conclut pas.
pub fn reachability(active: bool, connections: u32, incoming: u32) -> proto::pb::Reachability {
    use proto::pb::Reachability::{Closed, Open, Unknown};
    if incoming > 0 {
        Open
    } else if active && connections >= MIN_PEERS_TO_JUDGE {
        Closed
    } else {
        Unknown
    }
}

const MIN_PEERS_TO_JUDGE: u32 = 8;

/// Hôte d'une URL de tracker, SANS port, chemin ni requête : l'URL d'un tracker privé contient le passkey,
/// qui ne doit jamais quitter le nœud. Vide si l'URL n'est pas exploitable.
pub fn tracker_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split(']')
            .next()
            .map_or(host, |h| h.trim_start_matches('['))
    } else {
        host.split(':').next().unwrap_or(host)
    };
    host.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        .take(100)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Analyse l'URL d'un client torrent : http(s) et **boucle locale uniquement**
/// (l'API d'un client n'est jamais exposée ni jointe à distance par l'agent).
pub fn parse_local_url(s: &str) -> Result<url::Url, ClientError> {
    let u = url::Url::parse(s).map_err(|e| ClientError::Protocol(e.to_string()))?;
    let local = match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if !matches!(u.scheme(), "http" | "https") || !local {
        return Err(ClientError::Protocol(
            "l'API du client torrent doit être sur 127.0.0.1/localhost".into(),
        ));
    }
    Ok(u)
}

pub use proto::infohash::{magnet_infohash, normalize_infohash};

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn messages_never_carry_a_tracker_url_or_passkey() {
        for (m, w) in [
            (
                "Could not connect to http://t.example.org:80/PASSKEY123/announce?k=SECRET (timeout)",
                "Could not connect to [t.example.org] (timeout)",
            ),
            (
                "Error: \"https://tracker.test/ann/SECRETPASS\" returned 404",
                "Error: \"[tracker.test]\" returned 404",
            ),
            ("Tracker: [Failure reason \"Unregistered torrent\"]", "Tracker: [Failure reason \"Unregistered torrent\"]"),
            ("a\u{0}b\n  c", "ab c"),
            ("udp://open.demonii.com:1337/announce et http://x.org/y", "[open.demonii.com] et [x.org]"),
        ] {
            let c = clean_message(m);
            assert_eq!(c, w, "{m}");
            assert!(!c.contains("SECRET") && !c.contains("PASSKEY"), "{c}");
        }
        assert_eq!(
            clean_message(&"x".repeat(1000)).len(),
            300,
            "longueur bornée"
        );
    }

    #[test]
    fn reachability_only_concludes_with_evidence() {
        use proto::pb::Reachability::*;
        assert_eq!(
            reachability(true, 3, 1),
            Open,
            "une connexion entrante suffit"
        );
        assert_eq!(
            reachability(true, 30, 0),
            Closed,
            "beaucoup de pairs, aucun entrant"
        );
        assert_eq!(
            reachability(true, 2, 0),
            Unknown,
            "trop peu de pairs pour conclure"
        );
        assert_eq!(
            reachability(false, 50, 0),
            Unknown,
            "nœud au repos : pas de conclusion"
        );
    }

    #[test]
    fn tracker_host_never_keeps_the_passkey() {
        for (u, w) in [
            ("http://bt3.t-ru.org:80/ann?pk=SECRET", "bt3.t-ru.org"),
            (
                "https://tracker.example.org/SECRETPASSKEY/announce",
                "tracker.example.org",
            ),
            ("udp://Open.Demonii.com:1337/announce", "open.demonii.com"),
            ("http://user:pw@host.test:8080/x", "host.test"),
            ("http://[2001:db8::1]:80/announce", "2001:db8::1"),
            ("bt3.t-ru.org", "bt3.t-ru.org"),
            ("", ""),
        ] {
            let h = tracker_host(u);
            assert_eq!(h, w, "{u}");
            assert!(!h.contains("SECRET") && !h.contains("pw"), "{h}");
        }
    }

    #[test]
    fn infohash_forms() {
        assert_eq!(normalize_infohash(&H.to_uppercase()).as_deref(), Some(H));
        let b32 = data_encoding::BASE32_NOPAD
            .encode(&data_encoding::HEXLOWER.decode(H.as_bytes()).unwrap());
        assert_eq!(normalize_infohash(&b32).as_deref(), Some(H));
        for bad in ["", "zz", "../../etc", &H[..39], &format!("{H}0")] {
            assert!(normalize_infohash(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn magnets() {
        assert_eq!(
            magnet_infohash(&format!("magnet:?xt=urn:btih:{H}&dn=x")).as_deref(),
            Some(H)
        );
        assert!(magnet_infohash("http://evil/x.torrent").is_none());
        assert!(magnet_infohash("magnet:?dn=x").is_none());
        assert!(magnet_infohash(&format!("magnet:?xt=urn:btih:{H}\n--x")).is_none());
    }
}
