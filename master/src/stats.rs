//! Statistiques servies par l'API admin : échantillonnage des volumes (appelé chaque minute avec les métriques) et
//! agrégats (résumé, volumes, trackers, torrents inactifs, disques, gain de Multi-trackers).
//!
//! Le calcul pur (différences de compteurs, ratio, jours locaux, prévision) est dans `traffic.rs`.

use crate::store::{unix_now, StoreError, TrafficRow};
use crate::traffic::{self, Trend};
use crate::Core;
use proto::pb::{TorrentInfo, TorrentStatus};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Durée de conservation des volumes et des relevés d'espace disque (400 jours : un an plus de la marge).
pub const RETENTION_HOURS: i64 = 400 * 24;
/// Au-delà de ce délai sans envoi observé, un torrent en partage est considéré comme inactif.
pub const IDLE_DAYS_DEFAULT: u32 = 30;

fn is_seeding(t: &TorrentInfo) -> bool {
    t.status == TorrentStatus::Seeding as i32
}

/// Période demandée à l'API : `(heures, par_jour)`. `None` si inconnue.
pub fn parse_range(r: &str) -> Option<(i64, bool)> {
    Some(match r {
        "24h" => (24, false),
        "7d" => (7 * 24, true),
        "30d" => (30 * 24, true),
        "90d" => (90 * 24, true),
        "365d" => (365 * 24, true),
        _ => return None,
    })
}

#[derive(Debug, Serialize, Default, PartialEq)]
pub struct Volume {
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Serialize)]
pub struct Point {
    /// Début de la tranche (secondes Unix).
    pub ts: i64,
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Serialize)]
pub struct Named {
    pub id: String,
    pub name: String,
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Serialize)]
pub struct TrafficStats {
    pub range: String,
    /// "hour" ou "day".
    pub bucket: &'static str,
    pub series: Vec<Point>,
    pub totals: Volume,
    pub by_node: Vec<Named>,
    /// Par tracker (hôte ; vide = tracker inconnu).
    pub by_tracker: Vec<Named>,
    /// Début de l'enregistrement des volumes : avant cette date, rien n'a été mesuré (et non « rien n'a été envoyé »).
    pub tracking_since: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    /// Cumul tenu par les clients sur les torrents présents (depuis leur ajout).
    pub uploaded_total: u64,
    pub downloaded_total: u64,
    pub ratio: Option<f64>,
    pub up_24h: u64,
    pub down_24h: u64,
    pub up_7d: u64,
    pub up_30d: u64,
    pub down_30d: u64,
    /// Torrents en partage dont aucun envoi n'a été observé depuis `IDLE_DAYS_DEFAULT` jours.
    pub idle_count: usize,
    pub idle_days: u32,
    pub tracking_since: Option<i64>,
}

#[derive(Debug, Serialize, Default)]
pub struct TrackerStat {
    /// Hôte du tracker ; vide = inconnu.
    pub tracker: String,
    pub torrents: u64,
    pub seeding: u64,
    pub downloading: u64,
    pub errors: u64,
    pub size: u64,
    pub uploaded: u64,
    pub downloaded: u64,
    pub ratio: Option<f64>,
    pub nodes: u64,
    pub up_7d: u64,
    pub up_30d: u64,
    pub down_30d: u64,
    /// Premier message d'erreur d'un torrent de ce tracker (ex. « Unregistered torrent »), vide sinon.
    pub last_error: String,
}

#[derive(Debug, Serialize)]
pub struct IdleTorrent {
    pub infohash: String,
    pub name: String,
    pub node_id: String,
    pub node: String,
    pub tracker: String,
    pub size: u64,
    pub uploaded: u64,
    pub ratio: Option<f64>,
    /// Jours sans envoi observé.
    pub idle_days: f64,
    /// Depuis combien de jours nxtTorrent suit ce torrent (une valeur faible rend « inactif » peu parlant).
    pub observed_days: f64,
}

#[derive(Debug, Serialize)]
pub struct DiskPoint {
    pub ts: i64,
    pub free: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskStat {
    pub node_id: String,
    pub name: String,
    pub online: bool,
    pub free: u64,
    pub series: Vec<DiskPoint>,
    /// "unknown" | "falling" | "stable" | "rising"
    pub trend: &'static str,
    /// Variation de l'espace libre par jour (négative : le disque se remplit).
    pub per_day: f64,
    /// Jours avant saturation, annoncés seulement si le remplissage est assez régulier.
    pub days_left: Option<f64>,
    /// Qualité de l'estimation (0 à 1).
    pub confidence: f64,
}

#[derive(Debug, Serialize)]
pub struct XseedGain {
    /// Torrents ajoutés par Multi-trackers actuellement présents sur les nœuds.
    pub members: usize,
    pub seeding: usize,
    /// Espace disque qui aurait été occupé sans le partage des fichiers (liens physiques ou fichiers communs).
    pub bytes_saved: u64,
    pub uploaded_total: u64,
    pub up_7d: u64,
    pub up_30d: u64,
    /// Trackers supplémentaires qui reçoivent des envois grâce à la fonction.
    pub trackers: Vec<String>,
}

fn range_sum(rows: &[TrafficRow], from_hour: i64, keep: impl Fn(&TrafficRow) -> bool) -> Volume {
    rows.iter()
        .filter(|r| r.hour >= from_hour && keep(r))
        .fold(Volume::default(), |mut v, r| {
            v.up = v.up.saturating_add(r.up);
            v.down = v.down.saturating_add(r.down);
            v
        })
}

impl Core {
    /// Accès au registre des compteurs (tests : simule un redémarrage du master en le vidant).
    #[doc(hidden)]
    pub fn traffic_book_for_tests(&self) -> std::sync::MutexGuard<'_, Option<traffic::Book>> {
        self.traffic_book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Un nom lisible par nœud : le nom choisi par l'admin, sinon le nom d'hôte.
    fn node_names(&self) -> Result<(HashMap<String, String>, HashSet<String>), StoreError> {
        let mut names = HashMap::new();
        let mut online = HashSet::new();
        for n in self.store.node_details()? {
            if n.state == "Online" {
                online.insert(n.id.clone());
            }
            names.insert(n.id, n.name.filter(|x| !x.is_empty()).unwrap_or(n.hostname));
        }
        Ok((names, online))
    }

    /// Un passage de comptabilité : volumes de l'heure, derniers envois, espace disque. Appelé chaque minute.
    pub fn sample_traffic(&self) -> Result<(), StoreError> {
        self.sample_traffic_at(unix_now())
    }

    /// Variante à horloge injectée (tests).
    pub fn sample_traffic_at(&self, now: i64) -> Result<(), StoreError> {
        let hour = traffic::hour_of(now);
        let children = self.store.xseed_children()?;
        let is_x = |h: &str| children.contains_key(h);
        let (_, online) = self.node_names()?;

        let mut outcome = traffic::Outcome::default();
        let mut disks: Vec<(String, u64)> = Vec::new();
        {
            let mut guard = self
                .traffic_book
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(self.store.traffic_load_book()?);
            }
            let Some(book) = guard.as_mut() else {
                return Ok(());
            };
            self.each_telemetry(|id, t| {
                // Un nœud dont l'état complet n'a pas encore été reçu a une liste vide : y toucher effacerait ses
                // relevés de référence (voir traffic.rs).
                if t.full_states == 0 {
                    return;
                }
                let o = traffic::accumulate(book, id, &t.torrents, now, &is_x);
                outcome.deltas.extend(o.deltas);
                outcome.upsert.extend(o.upsert);
                outcome.removed.extend(o.removed);
                if online.contains(id) && t.disk_free > 0 {
                    disks.push((id.to_owned(), t.disk_free));
                }
            });
        }
        self.store.traffic_apply(hour, &outcome)?;
        for (id, free) in disks {
            self.store.disk_record(hour, &id, free)?;
        }
        Ok(())
    }

    /// Torrents de tous les nœuds en ligne ou non (dernier état connu), avec leur nœud.
    fn all_torrents(&self) -> Vec<(String, TorrentInfo)> {
        let mut v = Vec::new();
        self.each_telemetry(|id, t| {
            if t.full_states > 0 {
                v.extend(t.torrents.iter().map(|x| (id.to_owned(), x.clone())));
            }
        });
        v
    }

    pub fn stats_summary(&self) -> Result<Summary, StoreError> {
        self.stats_summary_at(unix_now())
    }

    pub fn stats_summary_at(&self, now: i64) -> Result<Summary, StoreError> {
        let hour = traffic::hour_of(now);
        let rows = self.store.traffic_rows(hour - 30 * 24)?;
        let (mut up, mut down, mut denom) = (0u64, 0u64, 0u64);
        let mut idle = 0usize;
        self.each_telemetry(|_, t| {
            if t.full_states == 0 {
                return;
            }
            for x in &t.torrents {
                up = up.saturating_add(x.uploaded);
                down = down.saturating_add(x.downloaded);
                denom = denom.saturating_add(if x.downloaded > 0 {
                    x.downloaded
                } else {
                    x.size_bytes
                });
            }
        });
        if let Some(book) = self
            .traffic_book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            self.each_telemetry(|id, t| {
                for x in t.torrents.iter().filter(|x| is_seeding(x)) {
                    if let Some(b) = book.get(id).and_then(|n| n.get(&x.infohash)) {
                        if now - b.last_up_ts >= i64::from(IDLE_DAYS_DEFAULT) * 86_400 {
                            idle += 1;
                        }
                    }
                }
            });
        }
        let v24 = range_sum(&rows, hour - 24, |_| true);
        let v7 = range_sum(&rows, hour - 7 * 24, |_| true);
        let v30 = range_sum(&rows, hour - 30 * 24, |_| true);
        Ok(Summary {
            uploaded_total: up,
            downloaded_total: down,
            ratio: (denom > 0).then(|| up as f64 / denom as f64),
            up_24h: v24.up,
            down_24h: v24.down,
            up_7d: v7.up,
            up_30d: v30.up,
            down_30d: v30.down,
            idle_count: idle,
            idle_days: IDLE_DAYS_DEFAULT,
            tracking_since: self.store.traffic_first_hour()?.map(|h| h * 3600),
        })
    }

    /// Volumes sur une période. `tz_secs` : décalage du fuseau de l'utilisateur (pour des jours à SON heure locale).
    pub fn stats_traffic(
        &self,
        range: &str,
        tz_secs: i64,
    ) -> Result<Option<TrafficStats>, StoreError> {
        self.stats_traffic_at(unix_now(), range, tz_secs)
    }

    pub fn stats_traffic_at(
        &self,
        now: i64,
        range: &str,
        tz_secs: i64,
    ) -> Result<Option<TrafficStats>, StoreError> {
        let Some((hours, daily)) = parse_range(range) else {
            return Ok(None);
        };
        let hour = traffic::hour_of(now);
        let from = hour - hours + 1;
        let rows = self.store.traffic_rows(from)?;
        let (names, _) = self.node_names()?;

        let mut totals = Volume::default();
        let mut by_node: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        let mut by_tracker: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for r in &rows {
            totals.up = totals.up.saturating_add(r.up);
            totals.down = totals.down.saturating_add(r.down);
            let n = by_node.entry(r.node_id.as_str()).or_default();
            n.0 = n.0.saturating_add(r.up);
            n.1 = n.1.saturating_add(r.down);
            let t = by_tracker.entry(r.tracker.as_str()).or_default();
            t.0 = t.0.saturating_add(r.up);
            t.1 = t.1.saturating_add(r.down);
        }

        let series = if daily {
            let h: Vec<(i64, u64, u64)> = {
                let mut per_hour: BTreeMap<i64, (u64, u64)> = BTreeMap::new();
                for r in &rows {
                    let e = per_hour.entry(r.hour).or_default();
                    e.0 = e.0.saturating_add(r.up);
                    e.1 = e.1.saturating_add(r.down);
                }
                per_hour.into_iter().map(|(h, (u, d))| (h, u, d)).collect()
            };
            let mut days = traffic::by_day(&h, tz_secs);
            // Jours sans trafic : un point à zéro, pour que le graphique ne « saute » pas les jours creux.
            let n_days = i64::from(u32::try_from(hours / 24).unwrap_or(0));
            let today = traffic::by_day(&[(hour, 0, 0)], tz_secs)
                .keys()
                .next()
                .copied()
                .unwrap_or(0);
            for i in 0..n_days {
                days.entry(today - i * 86_400).or_default();
            }
            days.into_iter()
                .filter(|(d, _)| *d >= today - (n_days - 1) * 86_400)
                .map(|(ts, (up, down))| Point { ts, up, down })
                .collect()
        } else {
            let mut per_hour: BTreeMap<i64, (u64, u64)> =
                (from..=hour).map(|h| (h, (0, 0))).collect();
            for r in &rows {
                let e = per_hour.entry(r.hour).or_default();
                e.0 = e.0.saturating_add(r.up);
                e.1 = e.1.saturating_add(r.down);
            }
            per_hour
                .into_iter()
                .map(|(h, (up, down))| Point {
                    ts: h * 3600,
                    up,
                    down,
                })
                .collect()
        };

        let name_of = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
        let mut by_node: Vec<Named> = by_node
            .into_iter()
            .map(|(id, (up, down))| Named {
                id: id.to_owned(),
                name: name_of(id),
                up,
                down,
            })
            .collect();
        by_node.sort_by(|a, b| b.up.cmp(&a.up).then_with(|| a.name.cmp(&b.name)));
        let mut by_tracker: Vec<Named> = by_tracker
            .into_iter()
            .map(|(t, (up, down))| Named {
                id: t.to_owned(),
                name: t.to_owned(),
                up,
                down,
            })
            .collect();
        by_tracker.sort_by(|a, b| b.up.cmp(&a.up).then_with(|| a.name.cmp(&b.name)));
        Ok(Some(TrafficStats {
            range: range.to_owned(),
            bucket: if daily { "day" } else { "hour" },
            series,
            totals,
            by_node,
            by_tracker,
            tracking_since: self.store.traffic_first_hour()?.map(|h| h * 3600),
        }))
    }

    /// Un résumé par tracker : état actuel (clients) et volumes (historique).
    pub fn stats_trackers(&self) -> Result<Vec<TrackerStat>, StoreError> {
        self.stats_trackers_at(unix_now())
    }

    pub fn stats_trackers_at(&self, now: i64) -> Result<Vec<TrackerStat>, StoreError> {
        let hour = traffic::hour_of(now);
        let rows = self.store.traffic_rows(hour - 30 * 24)?;
        let mut by: BTreeMap<String, TrackerStat> = BTreeMap::new();
        let mut denom: HashMap<String, u64> = HashMap::new();
        let mut nodes: HashMap<String, HashSet<String>> = HashMap::new();
        for (node, t) in self.all_torrents() {
            let s = by.entry(t.tracker.clone()).or_insert_with(|| TrackerStat {
                tracker: t.tracker.clone(),
                ..Default::default()
            });
            s.torrents += 1;
            s.size = s.size.saturating_add(t.size_bytes);
            s.uploaded = s.uploaded.saturating_add(t.uploaded);
            s.downloaded = s.downloaded.saturating_add(t.downloaded);
            let d = denom.entry(t.tracker.clone()).or_default();
            *d = d.saturating_add(if t.downloaded > 0 {
                t.downloaded
            } else {
                t.size_bytes
            });
            nodes.entry(t.tracker.clone()).or_default().insert(node);
            match TorrentStatus::try_from(t.status) {
                Ok(TorrentStatus::Seeding) => s.seeding += 1,
                Ok(TorrentStatus::Downloading) => s.downloading += 1,
                Ok(TorrentStatus::Error) => {
                    s.errors += 1;
                    if s.last_error.is_empty() {
                        s.last_error = t.message.clone();
                    }
                }
                _ => {}
            }
        }
        // Un tracker qui n'a plus de torrent mais un historique de volumes apparaît quand même.
        for r in &rows {
            by.entry(r.tracker.clone()).or_insert_with(|| TrackerStat {
                tracker: r.tracker.clone(),
                ..Default::default()
            });
        }
        for (name, s) in &mut by {
            s.nodes = nodes.get(name).map_or(0, |n| n.len() as u64);
            s.ratio = denom
                .get(name)
                .filter(|d| **d > 0)
                .map(|d| s.uploaded as f64 / *d as f64);
            let mine = |r: &TrafficRow| r.tracker == *name;
            s.up_7d = range_sum(&rows, hour - 7 * 24, mine).up;
            let v30 = range_sum(&rows, hour - 30 * 24, mine);
            s.up_30d = v30.up;
            s.down_30d = v30.down;
        }
        let mut v: Vec<TrackerStat> = by.into_values().collect();
        v.sort_by(|a, b| {
            b.up_30d
                .cmp(&a.up_30d)
                .then_with(|| b.uploaded.cmp(&a.uploaded))
                .then_with(|| a.tracker.cmp(&b.tracker))
        });
        Ok(v)
    }

    /// Torrents en partage dont aucun envoi n'a été observé depuis `days` jours, les plus anciens d'abord.
    pub fn stats_idle(&self, days: u32, limit: usize) -> Result<Vec<IdleTorrent>, StoreError> {
        self.stats_idle_at(unix_now(), days, limit)
    }

    pub fn stats_idle_at(
        &self,
        now: i64,
        days: u32,
        limit: usize,
    ) -> Result<Vec<IdleTorrent>, StoreError> {
        let (names, _) = self.node_names()?;
        let guard = self
            .traffic_book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(book) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let mut v: Vec<IdleTorrent> = self
            .all_torrents()
            .into_iter()
            .filter(|(_, t)| is_seeding(t))
            .filter_map(|(node, t)| {
                let b = book.get(&node)?.get(&t.infohash)?;
                let idle = now - b.last_up_ts;
                (idle >= i64::from(days) * 86_400).then(|| IdleTorrent {
                    ratio: traffic::ratio(t.uploaded, t.downloaded, t.size_bytes),
                    idle_days: idle as f64 / 86_400.0,
                    observed_days: (now - b.first_seen_ts) as f64 / 86_400.0,
                    node: names.get(&node).cloned().unwrap_or_else(|| node.clone()),
                    node_id: node,
                    infohash: t.infohash,
                    name: t.name,
                    tracker: t.tracker,
                    size: t.size_bytes,
                    uploaded: t.uploaded,
                })
            })
            .collect();
        v.sort_by(|a, b| {
            b.idle_days
                .total_cmp(&a.idle_days)
                .then_with(|| b.size.cmp(&a.size))
                .then_with(|| a.infohash.cmp(&b.infohash))
        });
        v.truncate(limit);
        Ok(v)
    }

    /// Espace libre par nœud : valeur actuelle, courbe des 14 derniers jours et prévision de saturation.
    pub fn stats_disks(&self) -> Result<Vec<DiskStat>, StoreError> {
        self.stats_disks_at(unix_now())
    }

    pub fn stats_disks_at(&self, now: i64) -> Result<Vec<DiskStat>, StoreError> {
        let hour = traffic::hour_of(now);
        let series = self.store.disk_series(hour - 14 * 24)?;
        let (names, online) = self.node_names()?;
        let mut by: BTreeMap<String, Vec<(i64, u64)>> = BTreeMap::new();
        for (n, h, f) in series {
            by.entry(n).or_default().push((h * 3600, f));
        }
        let mut out = Vec::new();
        for (id, name) in &names {
            let pts = by.remove(id).unwrap_or_default();
            let live = self.telemetry_summary(id).2;
            let free = if live > 0 {
                live
            } else {
                pts.last().map_or(0, |p| p.1)
            };
            if pts.is_empty() && free == 0 {
                continue;
            }
            let f = traffic::forecast(&pts);
            out.push(DiskStat {
                node_id: id.clone(),
                name: name.clone(),
                online: online.contains(id),
                free,
                series: pts
                    .iter()
                    .map(|&(ts, free)| DiskPoint { ts, free })
                    .collect(),
                trend: match f.trend {
                    Trend::Unknown => "unknown",
                    Trend::Falling => "falling",
                    Trend::Stable => "stable",
                    Trend::Rising => "rising",
                },
                per_day: f.per_day,
                days_left: f.days_left,
                confidence: f.r2,
            });
        }
        // Les plus urgents d'abord : saturation proche, puis disque qui se remplit, puis le reste par nom.
        out.sort_by(|a, b| {
            let key = |d: &DiskStat| d.days_left.unwrap_or(f64::MAX);
            key(a).total_cmp(&key(b)).then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// Ce que Multi-trackers rapporte : espace disque économisé et envois obtenus sur les trackers supplémentaires.
    pub fn stats_xseed_gain(&self) -> Result<XseedGain, StoreError> {
        self.stats_xseed_gain_at(unix_now())
    }

    pub fn stats_xseed_gain_at(&self, now: i64) -> Result<XseedGain, StoreError> {
        let hour = traffic::hour_of(now);
        let children = self.store.xseed_children()?;
        let rows = self.store.traffic_rows(hour - 30 * 24)?;
        let mut g = XseedGain {
            members: 0,
            seeding: 0,
            bytes_saved: 0,
            uploaded_total: 0,
            up_7d: range_sum(&rows, hour - 7 * 24, |r| r.xseed).up,
            up_30d: range_sum(&rows, hour - 30 * 24, |r| r.xseed).up,
            trackers: Vec::new(),
        };
        let mut trackers: BTreeMap<String, u64> = BTreeMap::new();
        for r in rows.iter().filter(|r| r.xseed && !r.tracker.is_empty()) {
            let e = trackers.entry(r.tracker.clone()).or_default();
            *e = e.saturating_add(r.up);
        }
        for (_, t) in self.all_torrents() {
            if !children.contains_key(&t.infohash) {
                continue;
            }
            g.members += 1;
            if is_seeding(&t) {
                g.seeding += 1;
            }
            g.bytes_saved = g.bytes_saved.saturating_add(t.size_bytes);
            g.uploaded_total = g.uploaded_total.saturating_add(t.uploaded);
            if !t.tracker.is_empty() {
                trackers.entry(t.tracker).or_default();
            }
        }
        g.trackers = trackers.into_keys().collect();
        Ok(g)
    }
}
