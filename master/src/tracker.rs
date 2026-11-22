//! Mini-tracker BitTorrent PRIVÉ, réservé à la réplication entre nœuds.
//!
//! Il ne sert qu'à mettre en relation les nœuds qui portent un même torrent (« qui d'autre a ce torrent ? ») : les
//! données circulent ensuite directement entre les nœuds par le protocole BitTorrent habituel, jamais par le master.
//!
//! Sécurité : une URL d'annonce n'est valable que pour UN torrent et UN nœud (`/announce/<nœud>/<jeton>`, jeton =
//! HMAC-SHA-256 du couple avec une clé du master). Sans jeton valide : refus. Les pairs renvoyés sont uniquement les
//! autres nœuds du même torrent ; l'adresse d'un pair est celle vue par le master (ou enregistrée), jamais celle que
//! le client prétend avoir. Le trafic est en clair (les clients ne feraient pas confiance à notre CA privée) : un jeton
//! intercepté ne permet que d'annoncer ce torrent-là pour ce nœud-là.
use axum::extract::{ConnectInfo, Path, RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use ring::hmac;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Intervalle d'annonce demandé aux clients (court : les copies démarrent vite, la charge est négligeable).
pub const INTERVAL_SECS: u64 = 30;
/// Un pair silencieux depuis plus de 3 intervalles n'est plus proposé.
const PEER_TTL: Duration = Duration::from_secs(INTERVAL_SECS * 3);
const MAX_PEERS_PER_TORRENT: usize = 200;
const MAX_TORRENTS: usize = 10_000;

#[derive(Clone)]
struct PeerEntry {
    ip: IpAddr,
    port: u16,
    left: u64,
    seen: Instant,
}

pub struct Tracker {
    key: hmac::Key,
    /// infohash -> (nœud -> dernier état annoncé)
    swarms: Mutex<HashMap<[u8; 20], HashMap<String, PeerEntry>>>,
    /// Adresse à proposer pour un nœud (celle vue par le master) ; à défaut, l'adresse source de l'annonce.
    ips: Mutex<HashMap<String, IpAddr>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    BadRequest(&'static str),
    Forbidden,
}

impl Tracker {
    pub fn new(secret: &[u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, secret),
            swarms: Mutex::new(HashMap::new()),
            ips: Mutex::new(HashMap::new()),
        })
    }

    /// Jeton d'annonce pour (torrent, nœud).
    pub fn token(&self, infohash: &[u8; 20], node_id: &str) -> String {
        let tag = hmac::sign(&self.key, &material(infohash, node_id));
        data_encoding::HEXLOWER.encode(&tag.as_ref()[..16])
    }

    /// Chemin d'annonce complet, à placer derrière l'adresse publique du tracker.
    pub fn announce_path(&self, infohash: &[u8; 20], node_id: &str) -> String {
        format!("/announce/{node_id}/{}", self.token(infohash, node_id))
    }

    /// Adresse à proposer aux autres nœuds pour ce nœud (celle sous laquelle le master le voit).
    pub fn set_node_ip(&self, node_id: &str, ip: IpAddr) {
        self.ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(node_id.to_owned(), ip);
    }

    fn verify(&self, infohash: &[u8; 20], node_id: &str, token: &str) -> bool {
        let Ok(given) = data_encoding::HEXLOWER.decode(token.as_bytes()) else {
            return false;
        };
        // Étiquette tronquée à 16 octets : on recalcule et on compare en temps constant via `hmac::verify` sur
        // l'étiquette complète impossible (tronquée) ; comparaison XOR cumulée, sans sortie anticipée.
        let full = hmac::sign(&self.key, &material(infohash, node_id));
        given.len() == 16
            && given
                .iter()
                .zip(&full.as_ref()[..16])
                .fold(0u8, |d, (a, b)| d | (a ^ b))
                == 0
    }

    /// Traite une annonce et renvoie la réponse bencodée.
    pub fn announce(
        &self,
        node_id: &str,
        token: &str,
        raw_query: &str,
        remote: IpAddr,
    ) -> Result<Vec<u8>, Refusal> {
        let q = Query::parse(raw_query).ok_or(Refusal::BadRequest("requête invalide"))?;
        let ih = q
            .info_hash
            .ok_or(Refusal::BadRequest("info_hash absent ou invalide"))?;
        if node_id.len() > 64 || !self.verify(&ih, node_id, token) {
            return Err(Refusal::Forbidden);
        }
        let port = q
            .port
            .filter(|p| *p > 0)
            .ok_or(Refusal::BadRequest("port invalide"))?;
        let ip = self
            .ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(node_id)
            .copied()
            .unwrap_or(remote);
        let now = Instant::now();
        let mut sw = self
            .swarms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Borne mémoire : on oublie les pairs périmés, puis les torrents vides.
        sw.values_mut()
            .for_each(|m| m.retain(|_, p| now.duration_since(p.seen) < PEER_TTL));
        sw.retain(|_, m| !m.is_empty());
        if q.event.as_deref() == Some("stopped") {
            if let Some(m) = sw.get_mut(&ih) {
                m.remove(node_id);
            }
            return Ok(response(&[], 0, 0));
        }
        if sw.len() >= MAX_TORRENTS && !sw.contains_key(&ih) {
            return Err(Refusal::BadRequest("trop de torrents suivis"));
        }
        let swarm = sw.entry(ih).or_default();
        if swarm.len() >= MAX_PEERS_PER_TORRENT && !swarm.contains_key(node_id) {
            return Err(Refusal::BadRequest("trop de pairs"));
        }
        swarm.insert(
            node_id.to_owned(),
            PeerEntry {
                ip,
                port,
                left: q.left,
                seen: now,
            },
        );
        let complete =
            u32::try_from(swarm.values().filter(|p| p.left == 0).count()).unwrap_or(u32::MAX);
        let incomplete = u32::try_from(swarm.len())
            .unwrap_or(u32::MAX)
            .saturating_sub(complete);
        // Uniquement les AUTRES nœuds de ce torrent (IPv4 : format compact).
        let peers: Vec<(Ipv4Addr, u16)> = swarm
            .iter()
            .filter(|(n, _)| n.as_str() != node_id)
            .filter_map(|(_, p)| match p.ip {
                IpAddr::V4(v4) => Some((v4, p.port)),
                IpAddr::V6(_) => None,
            })
            .take(q.numwant.unwrap_or(50).min(100))
            .collect();
        Ok(response(&peers, complete, incomplete))
    }

    /// Nombre de nœuds actuellement connus pour ce torrent (diagnostic et tests).
    pub fn swarm_size(&self, infohash: &[u8; 20]) -> usize {
        let now = Instant::now();
        self.swarms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(infohash)
            .map_or(0, |m| {
                m.values()
                    .filter(|p| now.duration_since(p.seen) < PEER_TTL)
                    .count()
            })
    }
}

fn material(infohash: &[u8; 20], node_id: &str) -> Vec<u8> {
    let mut m = b"nxt-tracker-v1|".to_vec();
    m.extend(infohash);
    m.push(b'|');
    m.extend(node_id.as_bytes());
    m
}

/// Réponse d'annonce : `d8:completei..e10:incompletei..e8:intervali..e12:min intervali..e5:peers<compact>e`.
fn response(peers: &[(Ipv4Addr, u16)], complete: u32, incomplete: u32) -> Vec<u8> {
    let mut compact = Vec::with_capacity(peers.len() * 6);
    for (ip, port) in peers {
        compact.extend(ip.octets());
        compact.extend(port.to_be_bytes());
    }
    let mut out = format!(
        "d8:completei{complete}e10:incompletei{incomplete}e8:intervali{INTERVAL_SECS}e12:min intervali{}e5:peers{}:",
        INTERVAL_SECS / 2,
        compact.len()
    )
    .into_bytes();
    out.extend(compact);
    out.push(b'e');
    out
}

fn failure(msg: &str) -> Vec<u8> {
    format!("d14:failure reason{}:{msg}e", msg.len()).into_bytes()
}

/// Paramètres d'annonce. `info_hash` est une chaîne binaire encodée en pour-cent (octets arbitraires) : on décode soi-même.
struct Query {
    info_hash: Option<[u8; 20]>,
    port: Option<u16>,
    left: u64,
    event: Option<String>,
    numwant: Option<usize>,
}

fn pct_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

impl Query {
    fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 2048 {
            return None;
        }
        let mut q = Self {
            info_hash: None,
            port: None,
            left: u64::MAX,
            event: None,
            numwant: None,
        };
        for pair in raw.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let val = pct_decode(v)?;
            let txt = || String::from_utf8_lossy(&val).into_owned();
            match k {
                "info_hash" => q.info_hash = <[u8; 20]>::try_from(val.as_slice()).ok(),
                "port" => q.port = txt().parse().ok(),
                "left" => q.left = txt().parse().unwrap_or(u64::MAX),
                "event" => q.event = Some(txt()),
                "numwant" => q.numwant = txt().parse().ok(),
                _ => {}
            }
        }
        Some(q)
    }
}

async fn handler(
    State(t): State<Arc<Tracker>>,
    Path((node, token)): Path<(String, String)>,
    RawQuery(q): RawQuery,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let body = match t.announce(&node, &token, q.as_deref().unwrap_or(""), addr.ip()) {
        Ok(b) => b,
        // Les clients lisent la raison dans un dictionnaire bencodé, même en cas d'erreur.
        Err(Refusal::Forbidden) => failure("non autorisé"),
        Err(Refusal::BadRequest(m)) => failure(m),
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain")], body).into_response()
}

pub fn router(t: Arc<Tracker>) -> Router {
    Router::new()
        .route("/announce/{node}/{token}", get(handler))
        .with_state(t)
}

/// Sert le tracker sur un listener déjà ouvert.
pub async fn serve(listener: tokio::net::TcpListener, t: Arc<Tracker>) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(t).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: [u8; 20] = [0xAB; 20];

    fn q(ih: &[u8; 20], port: u16, left: u64, extra: &str) -> String {
        let ih: String = ih.iter().map(|b| format!("%{b:02X}")).collect();
        format!("info_hash={ih}&peer_id=-XX0001-123456789012&port={port}&uploaded=0&downloaded=0&left={left}&compact=1{extra}")
    }

    fn peers_of(resp: &[u8]) -> Vec<(Ipv4Addr, u16)> {
        let s = resp.windows(7).position(|w| w == b"5:peers").unwrap() + 7;
        let colon = resp[s..].iter().position(|b| *b == b':').unwrap() + s;
        let len: usize = std::str::from_utf8(&resp[s..colon])
            .unwrap()
            .parse()
            .unwrap();
        resp[colon + 1..colon + 1 + len]
            .chunks(6)
            .map(|c| {
                (
                    Ipv4Addr::new(c[0], c[1], c[2], c[3]),
                    u16::from_be_bytes([c[4], c[5]]),
                )
            })
            .collect()
    }

    #[test]
    fn nodes_discover_each_other_only_through_valid_tokens() {
        let t = Tracker::new(&[1u8; 32]);
        t.set_node_ip("nodeA", "10.0.0.1".parse().unwrap());
        t.set_node_ip("nodeB", "10.0.0.2".parse().unwrap());
        let (ta, tb) = (t.token(&H, "nodeA"), t.token(&H, "nodeB"));
        let from = |s: &str| s.parse::<IpAddr>().unwrap();

        // A annonce seule : personne d'autre.
        let r = t
            .announce(
                "nodeA",
                &ta,
                &q(&H, 51413, 0, "&event=started"),
                from("203.0.113.9"),
            )
            .unwrap();
        assert!(peers_of(&r).is_empty());
        // B annonce : reçoit A, avec l'adresse ENREGISTRÉE (pas l'adresse source de l'annonce).
        let r = t
            .announce("nodeB", &tb, &q(&H, 6881, 5000, ""), from("198.51.100.7"))
            .unwrap();
        assert_eq!(peers_of(&r), vec![(Ipv4Addr::new(10, 0, 0, 1), 51413)]);
        assert!(
            r.starts_with(b"d8:completei1e10:incompletei1e"),
            "{}",
            String::from_utf8_lossy(&r)
        );
        // A annonce à nouveau : reçoit B.
        let r = t
            .announce("nodeA", &ta, &q(&H, 51413, 0, ""), from("203.0.113.9"))
            .unwrap();
        assert_eq!(peers_of(&r), vec![(Ipv4Addr::new(10, 0, 0, 2), 6881)]);
        assert_eq!(t.swarm_size(&H), 2);
        // Départ de B : il n'est plus proposé.
        t.announce(
            "nodeB",
            &tb,
            &q(&H, 6881, 5000, "&event=stopped"),
            from("198.51.100.7"),
        )
        .unwrap();
        assert!(peers_of(
            &t.announce("nodeA", &ta, &q(&H, 51413, 0, ""), from("203.0.113.9"))
                .unwrap()
        )
        .is_empty());
    }

    #[test]
    fn tokens_are_bound_to_one_torrent_and_one_node() {
        let t = Tracker::new(&[1u8; 32]);
        let other = Tracker::new(&[2u8; 32]);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let good = t.token(&H, "nodeA");
        let h2 = [0xCD; 20];
        for (node, tok, ih, why) in [
            ("nodeA", "0".repeat(32), H, "jeton inventé"),
            ("nodeB", good.clone(), H, "jeton d'un autre nœud"),
            ("nodeA", good.clone(), h2, "jeton d'un autre torrent"),
            (
                "nodeA",
                other.token(&H, "nodeA"),
                H,
                "jeton d'un autre master",
            ),
            ("nodeA", String::new(), H, "jeton vide"),
            ("nodeA", "zz".into(), H, "jeton non hexadécimal"),
        ] {
            assert_eq!(
                t.announce(node, &tok, &q(&ih, 1000, 0, ""), ip),
                Err(Refusal::Forbidden),
                "{why}"
            );
        }
        assert_eq!(t.swarm_size(&H), 0, "rien n'est enregistré après un refus");
        assert!(t.announce("nodeA", &good, &q(&H, 1000, 0, ""), ip).is_ok());
    }

    #[test]
    fn malformed_requests_are_refused_without_panicking() {
        let t = Tracker::new(&[1u8; 32]);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let tok = t.token(&H, "n");
        for bad in [
            "".to_owned(),
            "info_hash=%ZZ&port=1".into(),
            "info_hash=abc&port=1".into(),
            q(&H, 0, 0, ""),
            format!("{}&x={}", q(&H, 1, 0, ""), "a".repeat(3000)),
        ] {
            let r = t.announce("n", &tok, &bad, ip);
            assert!(
                matches!(r, Err(Refusal::BadRequest(_) | Refusal::Forbidden)),
                "{bad:.40}"
            );
        }
        assert!(
            t.announce(&"n".repeat(100), &tok, &q(&H, 1, 0, ""), ip)
                .is_err(),
            "identifiant de nœud démesuré"
        );
    }

    #[tokio::test]
    async fn http_endpoint_answers_with_bencode_and_never_leaks_other_torrents() {
        let t = Tracker::new(&[3u8; 32]);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(serve(l, t.clone()));
        let h2 = [0xEF; 20];
        t.set_node_ip("a", "10.0.0.1".parse().unwrap());
        t.set_node_ip("b", "10.0.0.2".parse().unwrap());
        let get = |node: &'static str, ih: [u8; 20], port: u16| {
            let url = format!(
                "http://{addr}{}?{}",
                t.announce_path(&ih, node),
                q(&ih, port, 0, "")
            );
            async move {
                reqwest::get(url)
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
                    .to_vec()
            }
        };
        get("a", H, 1111).await;
        get("a", h2, 3333).await; // un AUTRE torrent
        let r = get("b", H, 2222).await;
        assert_eq!(
            peers_of(&r),
            vec![(Ipv4Addr::new(10, 0, 0, 1), 1111)],
            "seul un pair du MÊME torrent"
        );
        // Mauvais jeton : réponse d'échec bencodée.
        let bad = reqwest::get(format!(
            "http://{addr}/announce/a/{}?{}",
            "0".repeat(32),
            q(&H, 1, 0, "")
        ))
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
        assert!(bad.starts_with(b"d14:failure reason"));
        assert_eq!(
            reqwest::get(format!("http://{addr}/autre"))
                .await
                .unwrap()
                .status(),
            404
        );
    }
}
