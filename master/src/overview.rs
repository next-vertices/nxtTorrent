//! Vues agrégées pour l'interface : liste de torrents filtrable/triable/paginée côté serveur (des
//! centaines, voire des milliers de torrents), KPIs du cluster et échantillons d'historique.
//!
//! Tout est calculé à partir de l'état désiré (SQLite) et de l'état observé (télémétrie en mémoire).
//! Les torrents présents sur un nœud mais inconnus du master apparaissent aussi (`managed = false`) :
//! le master ne les modifie jamais, mais un utilisateur de seedbox veut les voir.
use crate::state::NodeState;
use crate::store::{MetricPoint, StoreError};
use crate::Core;
use proto::pb::TorrentStatus;
use serde::Serialize;
use std::collections::HashMap;

/// Libellés de statut exposés à l'UI (ordre = priorité d'affichage quand un torrent est sur plusieurs nœuds).
pub const STATUSES: [&str; 8] = [
    "error",
    "downloading",
    "checking",
    "queued",
    "seeding",
    "paused",
    "waiting",
    "pending",
];

fn status_name(s: i32) -> &'static str {
    match TorrentStatus::try_from(s).unwrap_or(TorrentStatus::Unspecified) {
        TorrentStatus::Error => "error",
        TorrentStatus::Downloading | TorrentStatus::Stalled => "downloading",
        TorrentStatus::Checking => "checking",
        TorrentStatus::Queued => "queued",
        TorrentStatus::Seeding => "seeding",
        TorrentStatus::Paused => "paused",
        TorrentStatus::Unspecified => "waiting",
    }
}

fn rank(status: &str) -> usize {
    STATUSES
        .iter()
        .position(|s| *s == status)
        .unwrap_or(STATUSES.len())
}

/// Torrent ajouté par le cross-seed, regroupé sous la ligne de son contenu (jamais une ligne à part).
#[derive(Clone, Debug, Serialize)]
pub struct RowMember {
    pub infohash: String,
    pub name: String,
    /// Hôte du tracker (jamais l'URL : elle porte un passkey).
    pub tracker: String,
    pub status: &'static str,
    pub progress: f32,
    pub size: u64,
    pub dl_rate: u64,
    pub ul_rate: u64,
    pub peers_connected: u32,
    /// Vit sur des liens physiques vers les fichiers de l'original (aucun espace disque en plus).
    pub linked: bool,
    /// Nœud qui le porte (nom d'affichage) et message éventuel du client.
    pub node: String,
    pub node_online: bool,
    pub message: String,
    pub uploaded: u64,
    pub ratio: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RowNode {
    pub node_id: String,
    /// Nom d'affichage du nœud (jamais vide : repli sur le nom d'hôte).
    pub node: String,
    pub role: &'static str,
    pub phase: String,
    pub status: &'static str,
    pub progress: f32,
    /// Le nœud est connecté en ce moment.
    pub node_online: bool,
    /// Message du client torrent sur ce nœud (erreur ou avertissement), nettoyé ; vide si rien.
    pub message: String,
    pub peers_connected: u32,
    pub peers_active: u32,
    /// Débits et compteurs de CE nœud (la ligne du torrent, elle, additionne tous les nœuds).
    pub dl_rate: u64,
    pub ul_rate: u64,
    pub uploaded: u64,
    pub downloaded: u64,
    /// Ratio de ce nœud (voir `traffic::ratio`) ; `None` si sans objet.
    pub ratio: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TorrentRow {
    pub infohash: String,
    pub name: String,
    pub status: &'static str,
    pub progress: f32,
    pub size: u64,
    pub dl_rate: u64,
    pub ul_rate: u64,
    pub save_subdir: String,
    pub managed: bool,
    pub paused: bool,
    pub requires: Vec<String>,
    pub replicate_group: Option<String>,
    /// Mode de copie : "" (aucune), "swarm" (BitTorrent entre nœuds), "waiting_file" (magnet : .torrent demandé à un
    /// nœud), "shared" (fichiers supposés déjà présents sur les nœuds cibles).
    pub replication: &'static str,
    /// Hôte du tracker principal (vide si inconnu). Jamais l'URL : celle d'un tracker privé porte le passkey.
    pub tracker: String,
    /// Pairs connectés / actifs, additionnés sur les nœuds.
    pub peers_connected: u32,
    pub peers_active: u32,
    /// Essaim annoncé par le tracker (valeur la plus haute vue sur les nœuds).
    pub swarm_seeds: u32,
    pub swarm_leechers: u32,
    pub nodes: Vec<RowNode>,
    /// Torrents du même contenu sur d'autres trackers (cross-seed). Leurs débits et pairs sont DÉJÀ additionnés
    /// dans cette ligne.
    pub members: Vec<RowMember>,
    /// Un des torrents regroupés est en erreur (le statut de la ligne reste celui du torrent d'origine).
    pub members_issue: bool,
    /// Octets envoyés / téléchargés depuis l'ajout (compteurs des clients, additionnés sur les nœuds ; pour une ligne
    /// groupée, l'envoi inclut celui des torrents des autres trackers).
    pub uploaded: u64,
    pub downloaded: u64,
    /// Plus ancienne date d'ajout connue (secondes Unix), 0 si inconnue.
    pub added_on: u64,
    /// Ratio (voir `traffic::ratio`) ; `None` si sans objet.
    pub ratio: Option<f64>,
    /// Dénominateur du ratio (téléchargé, ou taille quand rien n'a été téléchargé), additionné sur les nœuds.
    #[serde(skip)]
    ratio_denom: u64,
}

#[derive(Clone, Debug, Default)]
pub struct TorrentQuery {
    /// Sous-chaîne du nom, de l'infohash ou du dossier (insensible à la casse).
    pub q: String,
    pub status: Option<String>,
    pub node: Option<String>,
    pub managed: Option<bool>,
    /// Hôte de tracker exact ; `-` = torrents sans tracker connu.
    pub tracker: Option<String>,
    pub sort: String,
    pub desc: bool,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct TorrentPage {
    /// Nombre de lignes après filtrage.
    pub total: usize,
    pub items: Vec<TorrentRow>,
    /// Effectif par statut sur l'ensemble filtré par texte/nœud (alimente les puces de filtre).
    pub counts: HashMap<&'static str, usize>,
    /// Trackers présents (après filtres texte/nœud/statut), du plus fréquent au moins fréquent.
    pub trackers: Vec<TrackerCount>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TrackerCount {
    pub host: String,
    pub count: usize,
}

impl Core {
    /// Comment un torrent à copier sera répliqué (voir `TorrentRow::replication`).
    fn replication_mode(&self, group: Option<&str>, has_file: bool, ih: &str) -> &'static str {
        if group.is_none() {
            ""
        } else if self.swarm().is_none() {
            "shared"
        } else if has_file {
            "swarm"
        } else if self.metainfo_failed(ih) {
            "shared"
        } else {
            "waiting_file"
        }
    }

    /// Toutes les lignes (une par infohash), nœuds agrégés.
    pub fn torrent_rows(&self) -> Result<Vec<TorrentRow>, StoreError> {
        let defs = self.store.list_torrents()?;
        let mut asg: HashMap<String, Vec<crate::store::AssignRow>> = HashMap::new();
        for a in self.store.all_assignments()? {
            asg.entry(a.infohash.clone()).or_default().push(a);
        }
        let details = self.store.node_details()?;
        let online: std::collections::HashSet<String> = details
            .iter()
            .filter(|n| n.state == "Online")
            .map(|n| n.id.clone())
            .collect();
        let names: HashMap<String, String> = details
            .into_iter()
            .map(|n| {
                let label = n.name.filter(|x| !x.is_empty()).unwrap_or(n.hostname);
                (n.id, label)
            })
            .collect();

        let mut rows: HashMap<String, TorrentRow> = HashMap::new();
        for d in defs {
            rows.insert(
                d.infohash.clone(),
                TorrentRow {
                    name: if d.name.is_empty() {
                        d.infohash.clone()
                    } else {
                        d.name.clone()
                    },
                    infohash: d.infohash.clone(),
                    status: "pending",
                    progress: 0.0,
                    size: d.size_hint,
                    dl_rate: 0,
                    ul_rate: 0,
                    save_subdir: d.save_subdir,
                    managed: true,
                    paused: d.paused,
                    requires: d.requires,
                    replication: self.replication_mode(
                        d.replicate_group.as_deref(),
                        d.has_file,
                        &d.infohash,
                    ),
                    replicate_group: d.replicate_group,
                    tracker: String::new(),
                    peers_connected: 0,
                    peers_active: 0,
                    swarm_seeds: 0,
                    swarm_leechers: 0,
                    nodes: Vec::new(),
                    members: Vec::new(),
                    members_issue: false,
                    uploaded: 0,
                    downloaded: 0,
                    added_on: 0,
                    ratio: None,
                    ratio_denom: 0,
                },
            );
        }
        // Assignations actives : le torrent n'est plus « pending » mais « waiting » tant qu'aucun nœud ne le remonte.
        for (ih, list) in &asg {
            if let Some(r) = rows.get_mut(ih) {
                for a in list.iter().filter(|a| !a.removed) {
                    r.status = "waiting";
                    r.nodes.push(RowNode {
                        node_id: a.node_id.clone(),
                        node: names.get(&a.node_id).cloned().unwrap_or_default(),
                        role: match a.role.as_str() {
                            "primary" => "primary",
                            "xseed" => "xseed",
                            _ => "replica",
                        },
                        phase: a.phase.clone(),
                        status: "waiting",
                        progress: 0.0,
                        node_online: online.contains(&a.node_id),
                        message: String::new(),
                        peers_connected: 0,
                        peers_active: 0,
                        dl_rate: 0,
                        ul_rate: 0,
                        uploaded: 0,
                        downloaded: 0,
                        ratio: None,
                    });
                }
            }
        }

        // État observé : un seul passage, verrou tenu le temps de copier ce qui est utile.
        self.each_telemetry(|node_id, tel| {
            for o in &tel.torrents {
                let st = status_name(o.status);
                let row = rows
                    .entry(o.infohash.clone())
                    .or_insert_with(|| TorrentRow {
                        infohash: o.infohash.clone(),
                        name: String::new(),
                        status: "waiting",
                        progress: 0.0,
                        size: 0,
                        dl_rate: 0,
                        ul_rate: 0,
                        save_subdir: o.save_subdir.clone(),
                        managed: false,
                        paused: false,
                        requires: Vec::new(),
                        replicate_group: None,
                        replication: "",
                        tracker: String::new(),
                        peers_connected: 0,
                        peers_active: 0,
                        swarm_seeds: 0,
                        swarm_leechers: 0,
                        nodes: Vec::new(),
                        members: Vec::new(),
                        members_issue: false,
                        uploaded: 0,
                        downloaded: 0,
                        added_on: 0,
                        ratio: None,
                        ratio_denom: 0,
                    });
                if row.name.is_empty() || row.name == row.infohash {
                    if !o.name.is_empty() {
                        row.name.clone_from(&o.name);
                    } else if row.name.is_empty() {
                        row.name.clone_from(&row.infohash);
                    }
                }
                row.size = row.size.max(o.size_bytes);
                row.dl_rate += o.dl_rate;
                row.ul_rate += o.ul_rate;
                row.peers_connected += o.peers_connected;
                row.peers_active += o.peers_active;
                row.swarm_seeds = row.swarm_seeds.max(o.swarm_seeds);
                row.swarm_leechers = row.swarm_leechers.max(o.swarm_leechers);
                row.uploaded = row.uploaded.saturating_add(o.uploaded);
                row.downloaded = row.downloaded.saturating_add(o.downloaded);
                row.ratio_denom = row.ratio_denom.saturating_add(if o.downloaded > 0 {
                    o.downloaded
                } else {
                    o.size_bytes
                });
                if o.added_on > 0 {
                    row.added_on = if row.added_on == 0 {
                        o.added_on
                    } else {
                        row.added_on.min(o.added_on)
                    };
                }
                if row.tracker.is_empty() {
                    // Texte venu d'un nœud : caractères sûrs uniquement, longueur bornée.
                    row.tracker = o
                        .tracker
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
                        .take(100)
                        .collect();
                }
                if row.nodes.iter().all(|n| n.node_id != node_id) {
                    row.nodes.push(RowNode {
                        node_id: node_id.to_owned(),
                        node: names.get(node_id).cloned().unwrap_or_default(),
                        role: "replica",
                        phase: String::new(),
                        status: st,
                        progress: o.progress,
                        node_online: online.contains(node_id),
                        message: String::new(),
                        peers_connected: 0,
                        peers_active: 0,
                        dl_rate: 0,
                        ul_rate: 0,
                        uploaded: 0,
                        downloaded: 0,
                        ratio: None,
                    });
                }
                if let Some(n) = row.nodes.iter_mut().find(|n| n.node_id == node_id) {
                    n.status = st;
                    n.progress = o.progress;
                    n.peers_connected = o.peers_connected;
                    n.peers_active = o.peers_active;
                    n.dl_rate = o.dl_rate;
                    n.ul_rate = o.ul_rate;
                    n.uploaded = o.uploaded;
                    n.downloaded = o.downloaded;
                    n.ratio = crate::traffic::ratio(o.uploaded, o.downloaded, o.size_bytes);
                    // Texte venu d'un nœud : jamais de contrôle, longueur bornée.
                    n.message = o
                        .message
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(300)
                        .collect();
                }
            }
        });

        let mut out: Vec<TorrentRow> = rows.into_values().collect();
        for r in &mut out {
            r.ratio = (r.ratio_denom > 0).then(|| r.uploaded as f64 / r.ratio_denom as f64);
            // Statut global = le plus « urgent » parmi les nœuds ; progression = la plus basse.
            if let Some(best) = r.nodes.iter().map(|n| n.status).min_by_key(|s| rank(s)) {
                r.status = best;
            }
            if !r.nodes.is_empty() {
                // Une copie de réplication en cours de vérification (ou en attente de ses données) ne doit pas
                // tirer vers le bas la progression d'un torrent que la source a terminé.
                let settled = |n: &&RowNode| n.phase.is_empty() || n.phase == "Active";
                let pool: Vec<&RowNode> = if r.nodes.iter().any(|n| settled(&n)) {
                    r.nodes.iter().filter(settled).collect()
                } else {
                    r.nodes.iter().collect()
                };
                r.progress = pool
                    .iter()
                    .map(|n| n.progress)
                    .fold(f32::INFINITY, f32::min);
            }
            r.nodes.sort_by(|a, b| a.node.cmp(&b.node));
        }
        Ok(group_cross_seeded(
            out,
            &self.store.xseed_children()?,
            &self.store.xseed_linked_set()?,
        ))
    }

    pub fn query_torrents(&self, q: &TorrentQuery) -> Result<TorrentPage, StoreError> {
        Ok(filter_page(self.torrent_rows()?, q))
    }

    /// KPIs du cluster (instantané).
    pub fn cluster_stats(&self) -> Result<ClusterStats, StoreError> {
        let nodes = self.store.node_details()?;
        let mut s = ClusterStats::default();
        for n in &nodes {
            match NodeState::parse(&n.state) {
                Some(NodeState::Revoked) => {
                    s.nodes_revoked += 1;
                    continue;
                }
                Some(NodeState::Online) => s.nodes_online += 1,
                Some(NodeState::Offline) => s.nodes_offline += 1,
                _ => s.nodes_pending += 1,
            }
            if matches!(NodeState::parse(&n.state), Some(NodeState::Online)) {
                let t = self.telemetry_summary(&n.id);
                s.dl_rate += t.0;
                s.ul_rate += t.1;
                s.disk_free += t.2;
            }
        }
        for r in self.torrent_rows()? {
            s.torrents_total += 1;
            s.bytes_total += r.size;
            match r.status {
                "seeding" => {
                    s.torrents_seeding += 1;
                    s.bytes_seeding += r.size;
                }
                "downloading" => s.torrents_downloading += 1,
                "paused" => s.torrents_paused += 1,
                "error" => s.torrents_error += 1,
                "pending" => s.torrents_pending += 1,
                _ => {}
            }
        }
        Ok(s)
    }

    /// Un échantillon par nœud : permet de comparer leur travail dans le temps.
    fn sample_node_metrics(&self) -> Result<(), StoreError> {
        let ts = crate::store::unix_now();
        let mut peers: HashMap<String, i64> = HashMap::new();
        self.each_telemetry(|id, t| {
            peers.insert(
                id.to_owned(),
                t.torrents
                    .iter()
                    .map(|x| i64::from(x.peers_connected))
                    .sum(),
            );
        });
        let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        let rows: Vec<crate::store::NodeMetric> = self
            .store
            .node_details()?
            .into_iter()
            .filter(|n| n.state != "Revoked")
            .map(|n| {
                let online = n.state == "Online";
                let (dl, ul, _) = if online {
                    self.telemetry_summary(&n.id)
                } else {
                    (0, 0, 0)
                };
                crate::store::NodeMetric {
                    ts,
                    online,
                    dl_rate: i(dl),
                    ul_rate: i(ul),
                    torrents: if online {
                        i64::try_from(self.telemetry_torrent_count(&n.id)).unwrap_or(0)
                    } else {
                        0
                    },
                    peers: if online {
                        peers.get(&n.id).copied().unwrap_or(0)
                    } else {
                        0
                    },
                    node_id: n.id,
                }
            })
            .collect();
        self.store.insert_node_metrics(&rows)
    }

    /// Enregistre un point d'historique.
    pub fn sample_metrics(&self) -> Result<(), StoreError> {
        let s = self.cluster_stats()?;
        let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        self.sample_node_metrics()?;
        // Volumes, derniers envois et espace disque : une panne ici ne doit pas empêcher l'historique de base.
        if let Err(e) = self.sample_traffic() {
            tracing::warn!(error = %e, "statistiques de trafic impossibles");
        }
        self.store.insert_metric(&MetricPoint {
            ts: crate::store::unix_now(),
            nodes_online: i64::from(s.nodes_online),
            nodes_total: i64::from(s.nodes_online + s.nodes_offline + s.nodes_pending),
            torrents_total: i64::try_from(s.torrents_total).unwrap_or(i64::MAX),
            torrents_seeding: i64::try_from(s.torrents_seeding).unwrap_or(i64::MAX),
            torrents_downloading: i64::try_from(s.torrents_downloading).unwrap_or(i64::MAX),
            dl_rate: i(s.dl_rate),
            ul_rate: i(s.ul_rate),
            disk_free: i(s.disk_free),
        })
    }
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct ClusterStats {
    pub nodes_online: u32,
    pub nodes_offline: u32,
    pub nodes_pending: u32,
    pub nodes_revoked: u32,
    pub torrents_total: u64,
    pub torrents_seeding: u64,
    pub torrents_downloading: u64,
    pub torrents_paused: u64,
    pub torrents_error: u64,
    pub torrents_pending: u64,
    pub dl_rate: u64,
    pub ul_rate: u64,
    pub disk_free: u64,
    pub bytes_total: u64,
    pub bytes_seeding: u64,
}

fn sort_key_cmp(a: &TorrentRow, b: &TorrentRow, sort: &str) -> std::cmp::Ordering {
    match sort {
        "size" => a.size.cmp(&b.size),
        "progress" => a.progress.total_cmp(&b.progress),
        "dl" => a.dl_rate.cmp(&b.dl_rate),
        "ul" => a.ul_rate.cmp(&b.ul_rate),
        "status" => rank(a.status).cmp(&rank(b.status)),
        "nodes" => a.nodes.len().cmp(&b.nodes.len()),
        "tracker" => a.tracker.cmp(&b.tracker),
        "peers" => a.peers_connected.cmp(&b.peers_connected),
        "uploaded" => a.uploaded.cmp(&b.uploaded),
        "ratio" => a.ratio.unwrap_or(-1.0).total_cmp(&b.ratio.unwrap_or(-1.0)),
        "added" => a.added_on.cmp(&b.added_on),
        "swarm" => (a.swarm_seeds + a.swarm_leechers).cmp(&(b.swarm_seeds + b.swarm_leechers)),
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
    .then_with(|| a.infohash.cmp(&b.infohash))
}

/// Filtrage, tri et pagination (fonction pure, testée sans base).
/// Regroupe les torrents du cross-seed sous la ligne du torrent d'origine : une ligne par CONTENU. Débits et pairs
/// sont additionnés ; si le parent est absent de la liste, l'enfant reste une ligne à part (rien ne disparaît).
pub fn group_cross_seeded(
    rows: Vec<TorrentRow>,
    children: &HashMap<String, String>,
    linked: &std::collections::HashSet<String>,
) -> Vec<TorrentRow> {
    if children.is_empty() {
        return rows;
    }
    let present: std::collections::HashSet<String> =
        rows.iter().map(|r| r.infohash.clone()).collect();
    let (kids, mut parents): (Vec<TorrentRow>, Vec<TorrentRow>) = rows.into_iter().partition(|r| {
        children
            .get(&r.infohash)
            .is_some_and(|p| present.contains(p))
    });
    let index: HashMap<String, usize> = parents
        .iter()
        .enumerate()
        .map(|(i, r)| (r.infohash.clone(), i))
        .collect();
    for c in kids {
        let Some(&i) = children.get(&c.infohash).and_then(|p| index.get(p)) else {
            parents.push(c);
            continue;
        };
        let p = &mut parents[i];
        p.dl_rate += c.dl_rate;
        p.ul_rate += c.ul_rate;
        p.peers_connected += c.peers_connected;
        p.peers_active += c.peers_active;
        p.members_issue |= c.status == "error";
        // L'envoi de la ligne couvre tous les trackers ; le dénominateur reste celui du torrent d'origine.
        p.uploaded = p.uploaded.saturating_add(c.uploaded);
        let node = c.nodes.first();
        p.members.push(RowMember {
            linked: linked.contains(&c.infohash),
            node: node.map(|n| n.node.clone()).unwrap_or_default(),
            node_online: node.is_some_and(|n| n.node_online),
            message: c
                .nodes
                .iter()
                .find(|n| !n.message.is_empty())
                .map(|n| n.message.clone())
                .unwrap_or_default(),
            infohash: c.infohash,
            name: c.name,
            tracker: c.tracker,
            status: c.status,
            progress: c.progress,
            size: c.size,
            dl_rate: c.dl_rate,
            ul_rate: c.ul_rate,
            peers_connected: c.peers_connected,
            uploaded: c.uploaded,
            ratio: c.ratio,
        });
    }
    for p in &mut parents {
        if !p.members.is_empty() {
            p.ratio = (p.ratio_denom > 0).then(|| p.uploaded as f64 / p.ratio_denom as f64);
        }
        p.members
            .sort_by(|a, b| a.tracker.cmp(&b.tracker).then(a.name.cmp(&b.name)));
    }
    parents
}

pub fn filter_page(rows: Vec<TorrentRow>, q: &TorrentQuery) -> TorrentPage {
    let needle = q.q.trim().to_lowercase();
    let text_ok = |r: &TorrentRow| {
        needle.is_empty()
            || r.name.to_lowercase().contains(&needle)
            || r.infohash.contains(&needle)
            || r.save_subdir.to_lowercase().contains(&needle)
            || r.members
                .iter()
                .any(|m| m.infohash.contains(&needle) || m.name.to_lowercase().contains(&needle))
    };
    let node_ok = |r: &TorrentRow| {
        q.node
            .as_deref()
            .is_none_or(|n| r.nodes.iter().any(|x| x.node_id == n))
    };
    let managed_ok = |r: &TorrentRow| q.managed.is_none_or(|m| r.managed == m);
    let tracker_ok = |r: &TorrentRow| {
        q.tracker.as_deref().is_none_or(|t| {
            if t == "-" {
                r.tracker.is_empty()
            } else {
                r.tracker == t || r.members.iter().any(|m| m.tracker == t)
            }
        })
    };

    let base: Vec<TorrentRow> = rows
        .into_iter()
        .filter(|r| text_ok(r) && node_ok(r) && managed_ok(r))
        .collect();
    // Facettes de trackers calculées AVANT le filtre de tracker : le sélecteur reste utilisable pour en changer.
    let mut by_tracker: HashMap<&str, usize> = HashMap::new();
    for r in &base {
        // Un contenu compte une fois par tracker distinct (le sien et ceux de ses torrents cross-seedés).
        let mut seen: Vec<&str> = vec![r.tracker.as_str()];
        for m in &r.members {
            if !seen.contains(&m.tracker.as_str()) {
                seen.push(m.tracker.as_str());
            }
        }
        for t in seen {
            *by_tracker.entry(t).or_default() += 1;
        }
    }
    let mut trackers: Vec<TrackerCount> = by_tracker
        .into_iter()
        .map(|(h, n)| TrackerCount {
            host: if h.is_empty() {
                "-".to_owned()
            } else {
                h.to_owned()
            },
            count: n,
        })
        .collect();
    trackers.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.host.cmp(&b.host)));
    let base: Vec<TorrentRow> = base.into_iter().filter(|r| tracker_ok(r)).collect();
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for r in &base {
        *counts.entry(r.status).or_default() += 1;
    }
    let mut items: Vec<TorrentRow> = base
        .into_iter()
        .filter(|r| q.status.as_deref().is_none_or(|s| r.status == s))
        .collect();
    items.sort_by(|a, b| {
        let o = sort_key_cmp(a, b, &q.sort);
        if q.desc {
            o.reverse()
        } else {
            o
        }
    });
    let total = items.len();
    let limit = if q.limit == 0 { 50 } else { q.limit.min(500) };
    let items = items.into_iter().skip(q.offset).take(limit).collect();
    TorrentPage {
        total,
        items,
        counts,
        trackers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, ih: &str, status: &'static str, size: u64, node: &str) -> TorrentRow {
        TorrentRow {
            infohash: ih.into(),
            name: name.into(),
            status,
            progress: 1.0,
            size,
            dl_rate: 0,
            ul_rate: 0,
            save_subdir: "tv".into(),
            managed: true,
            paused: false,
            requires: vec![],
            replicate_group: None,
            replication: "",
            tracker: String::new(),
            peers_connected: 0,
            peers_active: 0,
            swarm_seeds: 0,
            swarm_leechers: 0,
            nodes: vec![RowNode {
                node_id: node.into(),
                node: node.into(),
                role: "primary",
                phase: "Active".into(),
                status,
                progress: 1.0,
                node_online: true,
                message: String::new(),
                peers_connected: 0,
                peers_active: 0,
                dl_rate: 0,
                ul_rate: 0,
                uploaded: 0,
                downloaded: 0,
                ratio: None,
            }],
            members: Vec::new(),
            members_issue: false,
            uploaded: 0,
            downloaded: 0,
            added_on: 0,
            ratio: None,
            ratio_denom: 0,
        }
    }

    #[test]
    fn grouped_rows_add_every_trackers_upload_over_the_original_torrents_denominator() {
        let mut p = row("Film", "aa", "seeding", 1000, "n1");
        (p.uploaded, p.downloaded, p.ratio_denom, p.ratio) = (600, 1000, 1000, Some(0.6));
        let mut c = row("Film [b]", "bb", "seeding", 1000, "n1");
        (c.uploaded, c.ratio, c.tracker) = (900, Some(0.9), "b.example".into());
        let children = HashMap::from([("bb".to_owned(), "aa".to_owned())]);
        let g = group_cross_seeded(vec![p, c], &children, &std::collections::HashSet::new());
        assert_eq!(g.len(), 1);
        assert_eq!(
            g[0].uploaded, 1500,
            "l'envoi de la ligne couvre tous les trackers"
        );
        assert_eq!(
            g[0].ratio,
            Some(1.5),
            "rapporté au téléchargement du torrent d'origine, pas de sa copie"
        );
        assert_eq!(
            (g[0].members[0].uploaded, g[0].members[0].ratio),
            (900, Some(0.9))
        );
    }

    fn sample() -> Vec<TorrentRow> {
        vec![
            row("Ubuntu ISO", "aa", "seeding", 300, "n1"),
            row("debian", "bb", "downloading", 100, "n2"),
            row("Fedora", "cc", "seeding", 200, "n2"),
            row("arch", "dd", "paused", 50, "n1"),
        ]
    }

    fn q() -> TorrentQuery {
        TorrentQuery {
            sort: "name".into(),
            ..TorrentQuery::default()
        }
    }

    #[test]
    fn sorts_case_insensitively_and_paginates() {
        let p = filter_page(sample(), &q());
        let names: Vec<_> = p.items.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["arch", "debian", "Fedora", "Ubuntu ISO"]);
        let p = filter_page(
            sample(),
            &TorrentQuery {
                offset: 1,
                limit: 2,
                desc: true,
                ..q()
            },
        );
        assert_eq!(p.total, 4);
        let names: Vec<_> = p.items.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Fedora", "debian"]);
    }

    #[test]
    fn filters_by_text_status_and_node_and_counts_ignore_the_status_filter() {
        let p = filter_page(
            sample(),
            &TorrentQuery {
                status: Some("seeding".into()),
                ..q()
            },
        );
        assert_eq!(p.total, 2);
        // Les puces de filtre montrent toujours l'effectif de chaque statut.
        assert_eq!(p.counts["paused"], 1);
        assert_eq!(p.counts["seeding"], 2);

        let p = filter_page(
            sample(),
            &TorrentQuery {
                q: "UBU".into(),
                ..q()
            },
        );
        assert_eq!(p.total, 1);
        let p = filter_page(
            sample(),
            &TorrentQuery {
                node: Some("n2".into()),
                ..q()
            },
        );
        assert_eq!(p.total, 2);
        let p = filter_page(
            sample(),
            &TorrentQuery {
                q: "bb".into(),
                ..q()
            },
        );
        assert_eq!(p.items[0].name, "debian", "recherche par infohash");
    }

    #[test]
    fn tracker_filter_sort_and_facets() {
        let mut rows = sample();
        rows[0].tracker = "tracker.a.org".into();
        rows[1].tracker = "tracker.b.org".into();
        rows[2].tracker = "tracker.a.org".into();
        // rows[3] : tracker inconnu
        let p = filter_page(
            rows.clone(),
            &TorrentQuery {
                tracker: Some("tracker.a.org".into()),
                ..q()
            },
        );
        assert_eq!(p.total, 2);
        // Les facettes ignorent le filtre de tracker (on peut en changer) et comptent les « inconnus ».
        assert_eq!(
            p.trackers,
            vec![
                TrackerCount {
                    host: "tracker.a.org".into(),
                    count: 2
                },
                TrackerCount {
                    host: "-".into(),
                    count: 1
                },
                TrackerCount {
                    host: "tracker.b.org".into(),
                    count: 1
                },
            ]
        );
        let p = filter_page(
            rows.clone(),
            &TorrentQuery {
                tracker: Some("-".into()),
                ..q()
            },
        );
        assert_eq!(p.total, 1);
        assert_eq!(p.items[0].name, "arch");
        let p = filter_page(
            rows,
            &TorrentQuery {
                sort: "tracker".into(),
                desc: true,
                ..q()
            },
        );
        assert_eq!(p.items[0].tracker, "tracker.b.org");
    }

    #[test]
    fn numeric_sort_and_limit_is_capped() {
        let p = filter_page(
            sample(),
            &TorrentQuery {
                sort: "size".into(),
                desc: true,
                ..q()
            },
        );
        assert_eq!(p.items[0].size, 300);
        let many: Vec<_> = (0..900)
            .map(|i| row(&format!("t{i}"), &format!("{i:040}"), "seeding", 1, "n1"))
            .collect();
        let p = filter_page(
            many,
            &TorrentQuery {
                limit: 10_000,
                ..q()
            },
        );
        assert_eq!(p.total, 900);
        assert_eq!(p.items.len(), 500, "page bornée");
    }

    fn with(mut r: TorrentRow, tracker: &str, dl: u64, ul: u64, peers: u32) -> TorrentRow {
        r.tracker = tracker.into();
        r.dl_rate = dl;
        r.ul_rate = ul;
        r.peers_connected = peers;
        r
    }

    #[test]
    fn cross_seeded_torrents_collapse_into_one_row_per_content() {
        let rows = vec![
            with(row("Film", "aa", "seeding", 100, "n1"), "a.test", 0, 10, 2),
            with(
                row("Film (B)", "bb", "seeding", 100, "n1"),
                "b.test",
                0,
                30,
                5,
            ),
            with(row("Film (C)", "cc", "error", 100, "n1"), "c.test", 0, 0, 0),
            row("Autre", "dd", "seeding", 50, "n2"),
        ];
        let kids: HashMap<String, String> = [("bb", "aa"), ("cc", "aa")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let out = group_cross_seeded(rows, &kids, &Default::default());
        assert_eq!(out.len(), 2, "une ligne par contenu, pas par torrent");
        let p = out.iter().find(|r| r.infohash == "aa").unwrap();
        assert_eq!(
            (p.ul_rate, p.peers_connected, p.members.len()),
            (40, 7, 2),
            "débits et pairs additionnés"
        );
        assert!(p.members_issue, "un torrent regroupé en erreur est signalé");
        assert_eq!(p.status, "seeding", "le statut reste celui de l'original");
        assert_eq!(p.members[0].tracker, "b.test");
        assert!(out.iter().all(|r| r.infohash != "bb" && r.infohash != "cc"));
    }

    #[test]
    fn a_cross_seeded_torrent_without_its_parent_is_never_hidden() {
        let rows = vec![row("Orphelin", "bb", "seeding", 100, "n1")];
        let kids: HashMap<String, String> = [("bb".to_owned(), "zz".to_owned())].into();
        let out = group_cross_seeded(rows, &kids, &Default::default());
        assert_eq!(out.len(), 1);
        assert!(out[0].members.is_empty());
    }

    #[test]
    fn filters_and_tracker_facets_see_the_grouped_members_once() {
        let rows = vec![
            with(row("Film", "aa", "seeding", 100, "n1"), "a.test", 0, 0, 0),
            with(
                row("Film (B)", "bb", "seeding", 100, "n1"),
                "b.test",
                0,
                0,
                0,
            ),
            with(
                row("Film (B2)", "b2", "seeding", 100, "n1"),
                "b.test",
                0,
                0,
                0,
            ),
            row("Autre", "dd", "seeding", 50, "n2"),
        ];
        let kids: HashMap<String, String> = [("bb", "aa"), ("b2", "aa")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let grouped = group_cross_seeded(rows, &kids, &Default::default());
        let mut query = q();
        query.tracker = Some("b.test".into());
        let page = filter_page(grouped.clone(), &query);
        assert_eq!(page.total, 1, "filtrer sur un tracker retrouve le groupe");
        assert_eq!(page.items[0].infohash, "aa");
        // Le contenu compte UNE fois pour b.test même s'il y porte deux torrents.
        let all = filter_page(grouped.clone(), &q());
        assert_eq!(all.total, 2);
        let b = all.trackers.iter().find(|t| t.host == "b.test").unwrap();
        assert_eq!(b.count, 1);
        // La recherche par infohash d'un torrent regroupé retrouve la ligne du contenu.
        let mut query = q();
        query.q = "b2".into();
        assert_eq!(filter_page(grouped, &query).items[0].infohash, "aa");
    }
}
