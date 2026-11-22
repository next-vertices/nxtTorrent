//! Scheduler V1 : éligibilité par état/labels/espace disque, puis le moins chargé.
//! Fonction pure : aucun accès base ni réseau.
use crate::state::NodeState;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: String,
    pub state: NodeState,
    pub labels: Vec<String>,
    pub disk_free: u64,
    /// Nombre de torrents actifs déjà assignés au nœud.
    pub load: usize,
}

/// Éligible = Online, possède TOUS les labels requis, assez d'espace (taille + réserve).
pub fn is_eligible(c: &Candidate, requires: &[String], needed: u64, reserve: u64) -> bool {
    c.state == NodeState::Online
        && requires.iter().all(|r| c.labels.contains(r))
        && c.disk_free >= needed.saturating_add(reserve)
}

/// Le moins chargé parmi les éligibles (égalité : plus petit id, pour un résultat déterministe).
/// `None` si aucun éligible : le torrent reste `Pending`, jamais assigné à un nœud non conforme.
pub fn pick_node(
    candidates: &[Candidate],
    requires: &[String],
    needed: u64,
    reserve: u64,
    exclude: &HashSet<String>,
) -> Option<String> {
    candidates
        .iter()
        .filter(|c| !exclude.contains(&c.id) && is_eligible(c, requires, needed, reserve))
        .min_by(|a, b| a.load.cmp(&b.load).then_with(|| a.id.cmp(&b.id)))
        .map(|c| c.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, state: NodeState, labels: &[&str], disk: u64, load: usize) -> Candidate {
        Candidate {
            id: id.into(),
            state,
            labels: labels.iter().map(|s| (*s).to_owned()).collect(),
            disk_free: disk,
            load,
        }
    }
    fn req(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| (*s).to_owned()).collect()
    }
    const NONE: fn() -> HashSet<String> = HashSet::new;

    #[test]
    fn picks_least_loaded_eligible() {
        let cs = [
            c("a", NodeState::Online, &["ssd"], 1000, 5),
            c("b", NodeState::Online, &["ssd"], 1000, 2),
            c("c", NodeState::Online, &["ssd"], 1000, 9),
        ];
        assert_eq!(
            pick_node(&cs, &req(&["ssd"]), 0, 0, &NONE()).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn ties_are_deterministic() {
        let cs = [
            c("z", NodeState::Online, &[], 10, 1),
            c("m", NodeState::Online, &[], 10, 1),
        ];
        assert_eq!(pick_node(&cs, &[], 0, 0, &NONE()).as_deref(), Some("m"));
    }

    #[test]
    fn requires_all_labels() {
        let cs = [
            c("a", NodeState::Online, &["ssd"], 100, 0),
            c("b", NodeState::Online, &["ssd", "vpn-ch"], 100, 7),
        ];
        assert_eq!(
            pick_node(&cs, &req(&["ssd", "vpn-ch"]), 0, 0, &NONE()).as_deref(),
            Some("b")
        );
        assert_eq!(pick_node(&cs, &req(&["ssd", "tor"]), 0, 0, &NONE()), None);
    }

    #[test]
    fn only_online_nodes() {
        for st in [NodeState::Offline, NodeState::Pending, NodeState::Revoked] {
            assert_eq!(
                pick_node(&[c("a", st, &[], 100, 0)], &[], 0, 0, &NONE()),
                None
            );
        }
    }

    #[test]
    fn needs_enough_disk_with_reserve() {
        let cs = [
            c("a", NodeState::Online, &[], 1000, 0),
            c("b", NodeState::Online, &[], 5000, 3),
        ];
        assert_eq!(
            pick_node(&cs, &[], 1500, 100, &NONE()).as_deref(),
            Some("b")
        );
        assert_eq!(pick_node(&cs, &[], 4900, 200, &NONE()), None);
        assert_eq!(pick_node(&cs, &[], 900, 100, &NONE()).as_deref(), Some("a"));
    }

    #[test]
    fn exclusion_list_is_honoured() {
        let cs = [
            c("a", NodeState::Online, &[], 10, 0),
            c("b", NodeState::Online, &[], 10, 4),
        ];
        let ex: HashSet<String> = ["a".to_owned()].into();
        assert_eq!(pick_node(&cs, &[], 0, 0, &ex).as_deref(), Some("b"));
    }
}
