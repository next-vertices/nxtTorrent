//! Diagnostic réseau d'un nœud, déduit de ce que remonte son client torrent : port joignable, pairs
//! connectés, torrents en partage qui n'envoient rien alors que des pairs attendent.
//!
//! Aucune sonde externe : on interprète des observations (voir `agent::client::reachability`). Chaque
//! constat est une phrase destinée à un humain, avec la cause probable et l'action à tenter.
use proto::pb::{NodeHealth, Reachability, TorrentInfo, TorrentStatus};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    /// `ok` | `info` | `warn` | `bad`
    pub tone: &'static str,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    /// `open` | `closed` | `unknown`
    pub incoming: &'static str,
    /// Le client fournit des informations réseau (sinon tout est « inconnu »).
    pub known: bool,
    pub listen_port: u32,
    pub dht_nodes: u32,
    /// Pairs connectés, additionnés sur tous les torrents.
    pub peers: u32,
    pub seeding: usize,
    /// Torrents en partage dont l'essaim a des pairs en attente mais qui n'envoient rien.
    pub seeding_idle: usize,
    pub findings: Vec<Finding>,
}

fn f(tone: &'static str, title: &str, text: String) -> Finding {
    Finding {
        tone,
        title: title.to_owned(),
        text,
    }
}

/// `online` : le nœud est connecté en ce moment (sinon les valeurs sont périmées : pas de diagnostic).
pub fn assess(online: bool, health: NodeHealth, torrents: &[TorrentInfo]) -> Assessment {
    let seeding_status = TorrentStatus::Seeding as i32;
    let down_status = TorrentStatus::Downloading as i32;
    let peers: u32 = torrents.iter().map(|t| t.peers_connected).sum();
    let seeding: Vec<&TorrentInfo> = torrents
        .iter()
        .filter(|t| t.status == seeding_status)
        .collect();
    let idle = seeding
        .iter()
        .filter(|t| t.swarm_leechers > 0 && t.ul_rate == 0 && t.peers_connected == 0)
        .count();
    let starved_downloads = torrents
        .iter()
        .filter(|t| t.status == down_status && t.peers_connected == 0 && t.swarm_seeds > 0)
        .count();

    let incoming = if !online || !health.known {
        "unknown"
    } else {
        match Reachability::try_from(health.incoming).unwrap_or(Reachability::Unknown) {
            Reachability::Open => "open",
            Reachability::Closed => "closed",
            Reachability::Unknown => "unknown",
        }
    };

    let mut findings = Vec::new();
    if online {
        let port = if health.listen_port > 0 {
            format!(" (port {})", health.listen_port)
        } else {
            String::new()
        };
        match incoming {
            "open" => findings.push(f(
                "ok",
                "Port d'écoute joignable",
                format!("Des pairs se connectent à ce nœud{port} : la redirection de port fonctionne."),
            )),
            "closed" => findings.push(f(
                "bad",
                "Port d'écoute probablement fermé",
                format!(
                    "Aucune connexion entrante n'a été observée alors que {peers} pairs sont connectés{port}. \
                     Le port n'est sans doute pas redirigé (NAT de la box, pare-feu du VPS ou du fournisseur). \
                     Sans port ouvert, le nœud ne partage qu'avec les pairs qu'il contacte lui-même : \
                     c'est nettement moins efficace pour le partage."
                ),
            )),
            _ if !health.known => findings.push(f(
                "info",
                "Santé réseau non disponible",
                "Ce client torrent ne fournit pas d'informations sur la joignabilité du port.".to_owned(),
            )),
            _ => findings.push(f(
                "info",
                "Joignabilité non déterminée",
                "Pas assez d'activité pour conclure (il faut des pairs connectés). Réessayez quand des torrents sont actifs."
                    .to_owned(),
            )),
        }
        if seeding.len() >= 3 && idle >= 3 && idle * 2 >= seeding.len() {
            findings.push(f(
                "warn",
                "Partage au ralenti",
                format!(
                    "{idle} torrents sur {} en partage ont des pairs en attente dans leur essaim mais n'envoient rien. \
                     Causes fréquentes : port fermé, limite d'envoi atteinte, ou trop peu de connexions autorisées.",
                    seeding.len()
                ),
            ));
        }
        if starved_downloads > 0 {
            findings.push(f(
                "warn",
                "Téléchargements sans aucun pair",
                format!(
                    "{starved_downloads} téléchargement{} n'a aucun pair connecté alors que l'essaim en compte. \
                     Vérifiez la connexion du nœud (réseau, VPN) et le port d'écoute.",
                    if starved_downloads > 1 { "s" } else { "" }
                ),
            ));
        }
    }

    Assessment {
        incoming,
        known: health.known,
        listen_port: health.listen_port,
        dht_nodes: health.dht_nodes,
        peers,
        seeding: seeding.len(),
        seeding_idle: idle,
        findings,
    }
}

/// Un nœud qui envoie nettement moins que les autres (médiane des nœuds de la période), à nombre de torrents
/// comparable. Renvoie l'identifiant des nœuds concernés.
pub fn underperformers(rows: &[(String, f64, i64)]) -> Vec<String> {
    // (id, octets envoyés, torrents) ; au moins 3 nœuds pour qu'une médiane ait un sens.
    let active: Vec<&(String, f64, i64)> = rows.iter().filter(|r| r.2 > 0).collect();
    if active.len() < 3 {
        return Vec::new();
    }
    let mut v: Vec<f64> = active.iter().map(|r| r.1).collect();
    v.sort_by(f64::total_cmp);
    let median = v[v.len() / 2];
    if median <= 0.0 {
        return Vec::new();
    }
    active
        .iter()
        .filter(|r| r.1 < median * 0.25)
        .map(|r| r.0.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(status: TorrentStatus, peers: u32, leechers: u32, ul: u64) -> TorrentInfo {
        TorrentInfo {
            status: status as i32,
            peers_connected: peers,
            swarm_leechers: leechers,
            swarm_seeds: 10,
            ul_rate: ul,
            ..TorrentInfo::default()
        }
    }

    fn h(incoming: Reachability) -> NodeHealth {
        NodeHealth {
            known: true,
            incoming: incoming as i32,
            listen_port: 51413,
            ..NodeHealth::default()
        }
    }

    #[test]
    fn closed_port_is_explained_with_cause_and_consequence() {
        let a = assess(
            true,
            h(Reachability::Closed),
            &[t(TorrentStatus::Seeding, 30, 5, 100)],
        );
        assert_eq!(a.incoming, "closed");
        let bad = a.findings.iter().find(|x| x.tone == "bad").unwrap();
        assert!(
            bad.text.contains("51413") && bad.text.contains("redirigé"),
            "{}",
            bad.text
        );
    }

    #[test]
    fn open_port_and_offline_node() {
        let a = assess(true, h(Reachability::Open), &[]);
        assert_eq!(a.findings[0].tone, "ok");
        // Nœud hors ligne : valeurs périmées, aucun diagnostic.
        let a = assess(false, h(Reachability::Closed), &[]);
        assert_eq!(a.incoming, "unknown");
        assert!(a.findings.is_empty());
    }

    #[test]
    fn unknown_client_does_not_raise_false_alarms() {
        let a = assess(true, NodeHealth::default(), &[]);
        assert_eq!(a.incoming, "unknown");
        assert_eq!(a.findings.len(), 1);
        assert_eq!(a.findings[0].tone, "info");
    }

    #[test]
    fn idle_seeding_with_waiting_peers_is_flagged_only_when_widespread() {
        let idle: Vec<_> = (0..4).map(|_| t(TorrentStatus::Seeding, 0, 8, 0)).collect();
        let a = assess(true, h(Reachability::Unknown), &idle);
        assert_eq!(a.seeding_idle, 4);
        assert!(a.findings.iter().any(|x| x.title == "Partage au ralenti"));
        // Quelques torrents au repos parmi beaucoup d'actifs : normal.
        let mut mixed: Vec<_> = (0..10)
            .map(|_| t(TorrentStatus::Seeding, 5, 8, 100))
            .collect();
        mixed.push(t(TorrentStatus::Seeding, 0, 8, 0));
        let a = assess(true, h(Reachability::Open), &mixed);
        assert!(!a.findings.iter().any(|x| x.title == "Partage au ralenti"));
        // Téléchargement sans aucun pair alors que l'essaim existe.
        let a = assess(
            true,
            h(Reachability::Open),
            &[t(TorrentStatus::Downloading, 0, 3, 0)],
        );
        assert!(a
            .findings
            .iter()
            .any(|x| x.title.contains("sans aucun pair")));
    }

    #[test]
    fn underperformers_need_enough_nodes_and_a_clear_gap() {
        let r = |id: &str, up: f64, n: i64| (id.to_owned(), up, n);
        assert!(
            underperformers(&[r("a", 100.0, 5), r("b", 1.0, 5)]).is_empty(),
            "2 nœuds : pas de médiane"
        );
        let rows = [
            r("a", 1000.0, 50),
            r("b", 900.0, 40),
            r("c", 50.0, 45),
            r("d", 0.0, 0),
        ];
        assert_eq!(
            underperformers(&rows),
            vec!["c".to_owned()],
            "d n'a aucun torrent : exclu"
        );
    }
}
