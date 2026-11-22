//! Statistiques de trafic : calcul PUR (sans base ni réseau), donc testable à fond.
//!
//! Les clients torrent ne donnent que des compteurs CUMULÉS par torrent (octets envoyés / téléchargés depuis l'ajout).
//! Le master en déduit des VOLUMES : à chaque échantillon (une minute), la différence avec le relevé précédent est
//! ajoutée au bucket de l'heure courante, par nœud et par tracker. Quatre règles qui rendent le résultat juste :
//!
//! * **Premier relevé = ligne de base** : l'historique d'avant la surveillance n'est jamais attribué à « aujourd'hui ».
//! * **Compteur qui recule** (torrent retiré puis rajouté, statistiques remises à zéro) : le client repart de zéro, la
//!   valeur courante est donc comptée en entier, jamais une différence négative.
//! * **Torrent disparu** : sa ligne de base est supprimée ; un nouvel ajout repart d'une base neuve.
//! * **Nœud sans état complet reçu** (redémarrage du master, première connexion) : on n'y touche pas, sinon une liste
//!   vide transitoire effacerait toutes ses lignes de base et ferait perdre les volumes de l'intervalle.

use proto::pb::TorrentInfo;
use std::collections::{BTreeMap, HashMap};

/// Relevé de référence d'un torrent sur un nœud.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub up: u64,
    pub down: u64,
    /// Dernière fois (secondes Unix) où le compteur d'envoi a augmenté ; vaut la date du premier relevé tant qu'aucun
    /// envoi n'a été observé. Sert à repérer les torrents qui ne partagent plus rien.
    pub last_up_ts: i64,
    pub first_seen_ts: i64,
}

/// nœud -> (infohash -> relevé).
pub type Book = HashMap<String, HashMap<String, Totals>>;

/// Volume à ajouter au bucket horaire d'un (nœud, tracker).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    pub node_id: String,
    pub tracker: String,
    /// Torrent ajouté par Multi-trackers (cross-seed) : permet de chiffrer ce que cette fonction rapporte.
    pub xseed: bool,
    pub up: u64,
    pub down: u64,
}

/// Ce qu'un passage a changé : de quoi mettre à jour la base sans réécrire toutes les lignes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub deltas: Vec<Delta>,
    /// Lignes de base nouvelles ou modifiées : (nœud, infohash, relevé).
    pub upsert: Vec<(String, String, Totals)>,
    /// Lignes de base à supprimer : (nœud, infohash).
    pub removed: Vec<(String, String)>,
}

/// Différence entre deux compteurs cumulés ; un compteur qui recule signifie « repart de zéro » (voir plus haut).
fn step(prev: u64, now: u64) -> u64 {
    if now >= prev {
        now - prev
    } else {
        now
    }
}

/// Met à jour la comptabilité d'UN nœud à partir de sa liste de torrents actuelle.
pub fn accumulate(
    book: &mut Book,
    node_id: &str,
    torrents: &[TorrentInfo],
    now: i64,
    is_xseed: &dyn Fn(&str) -> bool,
) -> Outcome {
    let mut out = Outcome::default();
    let node = book.entry(node_id.to_owned()).or_default();
    // Les volumes d'un même (tracker, xseed) du passage sont additionnés : une seule ligne par bucket.
    let mut sum: BTreeMap<(String, bool), (u64, u64)> = BTreeMap::new();
    let mut present: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(torrents.len());
    for t in torrents {
        present.insert(t.infohash.as_str());
        match node.get_mut(&t.infohash) {
            None => {
                let base = Totals {
                    up: t.uploaded,
                    down: t.downloaded,
                    last_up_ts: now,
                    first_seen_ts: now,
                };
                node.insert(t.infohash.clone(), base.clone());
                out.upsert
                    .push((node_id.to_owned(), t.infohash.clone(), base));
            }
            Some(p) => {
                let (du, dd) = (step(p.up, t.uploaded), step(p.down, t.downloaded));
                let next = Totals {
                    up: t.uploaded,
                    down: t.downloaded,
                    last_up_ts: if du > 0 { now } else { p.last_up_ts },
                    first_seen_ts: p.first_seen_ts,
                };
                if du > 0 || dd > 0 {
                    let e = sum
                        .entry((t.tracker.clone(), is_xseed(&t.infohash)))
                        .or_default();
                    e.0 = e.0.saturating_add(du);
                    e.1 = e.1.saturating_add(dd);
                }
                if *p != next {
                    *p = next.clone();
                    out.upsert
                        .push((node_id.to_owned(), t.infohash.clone(), next));
                }
            }
        }
    }
    let gone: Vec<String> = node
        .keys()
        .filter(|h| !present.contains(h.as_str()))
        .cloned()
        .collect();
    for h in gone {
        node.remove(&h);
        out.removed.push((node_id.to_owned(), h));
    }
    out.deltas = sum
        .into_iter()
        .map(|((tracker, xseed), (up, down))| Delta {
            node_id: node_id.to_owned(),
            tracker,
            xseed,
            up,
            down,
        })
        .collect();
    out
}

/// Ratio d'un torrent : envoyé / téléchargé ; si rien n'a été téléchargé (torrent ajouté déjà complet), envoyé / taille.
/// `None` quand le ratio n'a pas de sens (taille inconnue et rien téléchargé).
pub fn ratio(up: u64, down: u64, size: u64) -> Option<f64> {
    let denom = if down > 0 { down } else { size };
    if denom == 0 {
        return None;
    }
    Some(up as f64 / denom as f64)
}

/// Heure (compteur d'heures depuis l'époque Unix, UTC) d'une date.
pub fn hour_of(ts: i64) -> i64 {
    ts.div_euclid(3600)
}

/// Regroupe des volumes horaires par JOUR LOCAL : `tz_secs` = décalage du fuseau de l'utilisateur par rapport à UTC
/// (positif à l'est). Renvoie, par jour, le début du jour (secondes Unix UTC) et les volumes.
pub fn by_day(rows: &[(i64, u64, u64)], tz_secs: i64) -> BTreeMap<i64, (u64, u64)> {
    let mut out: BTreeMap<i64, (u64, u64)> = BTreeMap::new();
    for &(hour, up, down) in rows {
        let local = hour * 3600 + tz_secs;
        let day_start = local.div_euclid(86_400) * 86_400 - tz_secs;
        let e = out.entry(day_start).or_default();
        e.0 = e.0.saturating_add(up);
        e.1 = e.1.saturating_add(down);
    }
    out
}

/// Tendance de l'espace libre d'un nœud.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trend {
    /// Pas assez de relevés (moins de 6 points ou moins de 24 h) pour estimer quoi que ce soit.
    Unknown,
    Falling,
    Stable,
    Rising,
}

/// Résultat de la prévision de saturation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forecast {
    pub trend: Trend,
    /// Variation de l'espace libre par jour (négative = le disque se remplit).
    pub per_day: f64,
    /// Jours avant saturation, si le disque se remplit de façon assez régulière pour l'estimer.
    pub days_left: Option<f64>,
    /// Qualité de l'ajustement (0 à 1) : bas = occupation en dents de scie, estimation peu fiable.
    pub r2: f64,
}

/// Variation quotidienne en deçà de laquelle l'espace est considéré stable (100 Mo/jour).
const STABLE_PER_DAY: f64 = 100e6;
/// En deçà, l'occupation est trop irrégulière pour annoncer une date.
const MIN_R2: f64 = 0.5;

/// Prévision par régression linéaire sur les relevés (secondes Unix, octets libres), du plus ancien au plus récent.
pub fn forecast(points: &[(i64, u64)]) -> Forecast {
    let unknown = Forecast {
        trend: Trend::Unknown,
        per_day: 0.0,
        days_left: None,
        r2: 0.0,
    };
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return unknown;
    };
    if points.len() < 6 || last.0 - first.0 < 86_400 {
        return unknown;
    }
    let n = points.len() as f64;
    let t0 = first.0;
    let xs: Vec<f64> = points.iter().map(|p| (p.0 - t0) as f64).collect();
    let ys: Vec<f64> = points.iter().map(|p| p.1 as f64).collect();
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    let syy: f64 = ys.iter().map(|y| (y - my).powi(2)).sum();
    let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    if sxx == 0.0 {
        return unknown;
    }
    let slope = sxy / sxx; // octets par seconde
    let per_day = slope * 86_400.0;
    let r2 = if syy == 0.0 {
        1.0
    } else {
        (sxy * sxy) / (sxx * syy)
    };
    let now_free = last.1 as f64;
    let trend = if per_day.abs() < STABLE_PER_DAY {
        Trend::Stable
    } else if per_day < 0.0 {
        Trend::Falling
    } else {
        Trend::Rising
    };
    let days_left = (trend == Trend::Falling && r2 >= MIN_R2).then(|| now_free / -per_day);
    Forecast {
        trend,
        per_day,
        days_left,
        r2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: &str, tracker: &str, up: u64, down: u64) -> TorrentInfo {
        TorrentInfo {
            infohash: h.into(),
            tracker: tracker.into(),
            uploaded: up,
            downloaded: down,
            ..Default::default()
        }
    }
    const NO_XSEED: &dyn Fn(&str) -> bool = &|_| false;

    #[test]
    fn the_first_reading_is_a_baseline_and_never_counts_old_history() {
        let mut b = Book::new();
        let o = accumulate(
            &mut b,
            "n1",
            &[t("a", "tr", 9_000_000, 1_000)],
            100,
            NO_XSEED,
        );
        assert!(
            o.deltas.is_empty(),
            "l'historique d'avant la surveillance n'est pas du trafic d'aujourd'hui"
        );
        assert_eq!(o.upsert.len(), 1);
        assert_eq!(b["n1"]["a"].up, 9_000_000);
        assert_eq!(b["n1"]["a"].last_up_ts, 100);
    }

    #[test]
    fn later_readings_add_the_difference_per_tracker() {
        let mut b = Book::new();
        accumulate(
            &mut b,
            "n1",
            &[t("a", "tr1", 100, 0), t("b", "tr2", 0, 50)],
            100,
            NO_XSEED,
        );
        let o = accumulate(
            &mut b,
            "n1",
            &[t("a", "tr1", 160, 0), t("b", "tr2", 0, 90)],
            160,
            NO_XSEED,
        );
        let mut d = o.deltas.clone();
        d.sort_by(|x, y| x.tracker.cmp(&y.tracker));
        assert_eq!((d[0].tracker.as_str(), d[0].up, d[0].down), ("tr1", 60, 0));
        assert_eq!((d[1].tracker.as_str(), d[1].up, d[1].down), ("tr2", 0, 40));
        assert_eq!(b["n1"]["a"].last_up_ts, 160, "un envoi a été observé");
        assert_eq!(
            b["n1"]["b"].last_up_ts, 100,
            "rien envoyé : la date ne bouge pas"
        );
    }

    #[test]
    fn two_torrents_on_the_same_tracker_are_summed_into_one_delta() {
        let mut b = Book::new();
        accumulate(
            &mut b,
            "n1",
            &[t("a", "tr", 0, 0), t("b", "tr", 0, 0)],
            1,
            NO_XSEED,
        );
        let o = accumulate(
            &mut b,
            "n1",
            &[t("a", "tr", 10, 0), t("b", "tr", 5, 0)],
            2,
            NO_XSEED,
        );
        assert_eq!(o.deltas.len(), 1);
        assert_eq!(o.deltas[0].up, 15);
    }

    #[test]
    fn a_counter_that_goes_backwards_restarts_from_zero_and_is_never_negative() {
        let mut b = Book::new();
        accumulate(&mut b, "n1", &[t("a", "tr", 1_000, 500)], 1, NO_XSEED);
        // Torrent retiré puis rajouté entre deux relevés, ou statistiques remises à zéro : le client repart de 0.
        let o = accumulate(&mut b, "n1", &[t("a", "tr", 40, 10)], 2, NO_XSEED);
        assert_eq!((o.deltas[0].up, o.deltas[0].down), (40, 10));
        assert_eq!(
            b["n1"]["a"].up, 40,
            "la base est recalée sur la nouvelle valeur"
        );
    }

    #[test]
    fn an_unchanged_torrent_writes_nothing() {
        let mut b = Book::new();
        accumulate(&mut b, "n1", &[t("a", "tr", 5, 5)], 1, NO_XSEED);
        let o = accumulate(&mut b, "n1", &[t("a", "tr", 5, 5)], 99, NO_XSEED);
        assert_eq!(
            o,
            Outcome::default(),
            "ni volume, ni ligne à réécrire pour un torrent au repos"
        );
    }

    #[test]
    fn a_removed_torrent_loses_its_baseline_so_a_re_add_starts_fresh() {
        let mut b = Book::new();
        accumulate(&mut b, "n1", &[t("a", "tr", 100, 0)], 1, NO_XSEED);
        let o = accumulate(&mut b, "n1", &[], 2, NO_XSEED);
        assert_eq!(o.removed, vec![("n1".to_owned(), "a".to_owned())]);
        assert!(b["n1"].is_empty());
        // Rajouté plus tard avec un compteur plus bas : simple nouvelle base, aucun volume inventé.
        let o = accumulate(&mut b, "n1", &[t("a", "tr", 7, 0)], 3, NO_XSEED);
        assert!(o.deltas.is_empty());
    }

    #[test]
    fn nodes_are_accounted_separately_even_for_the_same_infohash() {
        let mut b = Book::new();
        accumulate(&mut b, "n1", &[t("a", "tr", 0, 0)], 1, NO_XSEED);
        accumulate(&mut b, "n2", &[t("a", "tr", 0, 0)], 1, NO_XSEED);
        let o1 = accumulate(&mut b, "n1", &[t("a", "tr", 10, 0)], 2, NO_XSEED);
        let o2 = accumulate(&mut b, "n2", &[t("a", "tr", 3, 0)], 2, NO_XSEED);
        assert_eq!((o1.deltas[0].up, o2.deltas[0].up), (10, 3));
    }

    #[test]
    fn multi_tracker_torrents_are_flagged_so_their_gain_can_be_measured() {
        let mut b = Book::new();
        let is_x: &dyn Fn(&str) -> bool = &|h| h == "x";
        accumulate(
            &mut b,
            "n1",
            &[t("p", "tr1", 0, 0), t("x", "tr2", 0, 0)],
            1,
            is_x,
        );
        let o = accumulate(
            &mut b,
            "n1",
            &[t("p", "tr1", 10, 0), t("x", "tr2", 4, 0)],
            2,
            is_x,
        );
        let x = o.deltas.iter().find(|d| d.xseed).unwrap();
        assert_eq!((x.tracker.as_str(), x.up), ("tr2", 4));
        assert_eq!(o.deltas.iter().find(|d| !d.xseed).unwrap().up, 10);
    }

    #[test]
    fn ratio_uses_downloaded_or_falls_back_to_size() {
        assert_eq!(ratio(300, 100, 1000), Some(3.0));
        assert_eq!(
            ratio(500, 0, 1000),
            Some(0.5),
            "ajouté déjà complet : rapporté à la taille"
        );
        assert_eq!(ratio(0, 0, 1000), Some(0.0));
        assert_eq!(
            ratio(10, 0, 0),
            None,
            "taille inconnue et rien téléchargé : pas de ratio"
        );
    }

    #[test]
    fn days_follow_the_users_timezone_not_utc() {
        // 2024-01-01 23:00 UTC et 2024-01-02 01:00 UTC.
        let h1 = 1_704_150_000 / 3600; // 23:00 UTC le 1er
        let h2 = h1 + 2; // 01:00 UTC le 2
        let rows = [(h1, 10, 1), (h2, 20, 2)];
        // UTC : deux jours distincts.
        assert_eq!(by_day(&rows, 0).len(), 2);
        // Paris (UTC+1) : 00:00 et 02:00 le 2 -> UN seul jour.
        let paris = by_day(&rows, 3600);
        assert_eq!(paris.len(), 1);
        assert_eq!(paris.values().next(), Some(&(30, 3)));
        // Le début du jour local est exprimé en secondes UTC (minuit à Paris = 23:00 UTC la veille).
        assert_eq!(*paris.keys().next().unwrap() % 86_400, 82_800);
    }

    #[test]
    fn forecast_needs_enough_history() {
        assert_eq!(forecast(&[]).trend, Trend::Unknown);
        let few: Vec<(i64, u64)> = (0..4).map(|i| (i * 7200, 1_000_000_000)).collect();
        assert_eq!(forecast(&few).trend, Trend::Unknown, "moins de 6 points");
        let short: Vec<(i64, u64)> = (0..8).map(|i| (i * 600, 1_000_000_000)).collect();
        assert_eq!(forecast(&short).trend, Trend::Unknown, "moins de 24 h");
    }

    #[test]
    fn a_disk_filling_steadily_gets_a_number_of_days() {
        // 10 Go libres, -1 Go par jour, un relevé toutes les 6 h pendant 3 jours.
        let pts: Vec<(i64, u64)> = (0..12)
            .map(|i| (i * 21_600, 13_000_000_000 - (i as u64) * 250_000_000))
            .collect();
        let f = forecast(&pts);
        assert_eq!(f.trend, Trend::Falling);
        assert!(
            (f.per_day + 1e9).abs() < 1e7,
            "≈ -1 Go/jour : {}",
            f.per_day
        );
        let d = f.days_left.expect("régulier : une date est annoncée");
        assert!((d - 10.25).abs() < 0.5, "≈ 10 jours avant saturation : {d}");
        assert!(f.r2 > 0.99);
    }

    #[test]
    fn a_stable_or_rising_disk_gets_no_date() {
        let flat: Vec<(i64, u64)> = (0..12).map(|i| (i * 21_600, 5_000_000_000)).collect();
        let f = forecast(&flat);
        assert_eq!(f.trend, Trend::Stable);
        assert!(f.days_left.is_none());
        let up: Vec<(i64, u64)> = (0..12)
            .map(|i| (i * 21_600, 5_000_000_000 + i as u64 * 2_000_000_000))
            .collect();
        let f = forecast(&up);
        assert_eq!(f.trend, Trend::Rising);
        assert!(f.days_left.is_none());
    }

    #[test]
    fn a_sawtooth_disk_gets_no_misleading_date() {
        // Remplissage puis grand ménage : la droite ne représente rien, on n'annonce donc pas de date.
        let pts: Vec<(i64, u64)> = (0..16)
            .map(|i| {
                let free = if (i / 4) % 2 == 0 {
                    20_000_000_000 - (i as u64 % 4) * 4_000_000_000
                } else {
                    8_000_000_000 + (i as u64 % 4) * 3_000_000_000
                };
                (i * 21_600, free)
            })
            .collect();
        let f = forecast(&pts);
        assert!(f.r2 < MIN_R2, "l'ajustement est mauvais : {f:?}");
        assert!(
            f.days_left.is_none(),
            "pas de date annoncée quand l'ajustement est mauvais : {f:?}"
        );
    }
}
