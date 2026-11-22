//! Calcul du diff entre l'état DÉSIRÉ d'un nœud (source de vérité : le master) et l'état OBSERVÉ
//! (`FullState`). Fonction pure : ne produit que les commandes manquantes, toutes idempotentes.
//!
//! Choix de conception : un torrent présent sur le nœud mais inconnu du master n'est JAMAIS touché
//! (le master ne supprime pas ce qu'il ne gère pas). Seul un « tombstone » explicite déclenche `Remove`.
use proto::pb::command::Kind;
use proto::pb::{
    add_cmd::Source, AddCmd, AddTrackerCmd, Command, LinkTreeCmd, RemoveCmd, TargetCmd,
    TorrentInfo, TorrentStatus,
};
use std::collections::HashSet;

const DONE: f32 = 0.9999;

/// Phase d'un torrent sur un nœud (cycle de réplication : Adding -> Rechecking -> Active).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Copie par le protocole BitTorrent : le torrent est ajouté avec l'annonce du tracker PRIVÉ du master
    /// (même infohash) et se télécharge depuis les autres nœuds ; aucun stockage partagé n'est nécessaire.
    Transfer,
    /// Copie terminée : le torrent « privé » est retiré SANS ses fichiers ; il sera ré-ajouté à l'identique de
    /// l'original (voir `Adding`), à la manière d'un cross-seed.
    Retarget,
    /// Cross-seed à disposition différente : l'agent crée d'abord des liens physiques vers les fichiers de l'original ;
    /// le torrent n'est ajouté au client qu'une fois la création confirmée.
    Linking,
    /// Ajout en pause demandé, pas encore observé.
    Adding,
    /// `Recheck` envoyé ; on attend la fin de la vérification avant de seeder.
    Rechecking,
    Active,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transfer => "Transfer",
            Self::Retarget => "Retarget",
            Self::Linking => "Linking",
            Self::Adding => "Adding",
            Self::Rechecking => "Rechecking",
            Self::Active => "Active",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Transfer" => Self::Transfer,
            "Retarget" => Self::Retarget,
            "Linking" => Self::Linking,
            "Adding" => Self::Adding,
            "Rechecking" => Self::Rechecking,
            "Active" => Self::Active,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Desired {
    pub infohash: String,
    pub magnet: String,
    pub save_subdir: String,
    /// Le torrent doit être en pause (décision admin).
    pub paused: bool,
    pub phase: Phase,
    /// Tombstone : le torrent doit disparaître du nœud.
    pub removed: bool,
    pub delete_files: bool,
    /// URL d'annonce (tracker privé du master, propre à ce nœud) à ajouter à ce torrent quand il est complet ici :
    /// ce nœud sert alors de source à une copie en cours. `None` = rien à faire.
    pub source_tracker: Option<String>,
    /// Torrent ajouté par magnet (le master n'a pas son .torrent) à copier par swarm : demander le .torrent à ce nœud
    /// dès qu'il l'a complet.
    pub fetch_metainfo: bool,
    /// Plafond de débit (octets/s, 0 = illimité) à appliquer à cette copie pendant son transfert ; `None` = déjà appliqué.
    pub copy_dl_limit: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Torrents dont l'`Add` doit porter le .torrent RÉÉCRIT (annonce du tracker du master) et non l'original.
    pub swarm_adds: HashSet<String>,
    /// Sources dont la commande `AddTracker` est émise (à mémoriser une fois envoyée).
    pub tracker_added: Vec<String>,
    /// Torrents dont le .torrent est demandé à ce nœud (à mémoriser une fois envoyé).
    pub metainfo_asked: Vec<String>,
    /// Plafonds de copie émis (à mémoriser une fois envoyés).
    pub limits_applied: Vec<(String, u64)>,
    pub commands: Vec<Command>,
    pub phase_updates: Vec<(String, Phase)>,
    /// Tombstones satisfaits (torrent absent du nœud) : l'assignation peut être supprimée.
    pub finished_removals: Vec<String>,
}

fn cmd(kind: &str, ih: &str, k: Kind) -> Command {
    Command {
        cmd_id: format!("{kind}-{}", &ih[..8.min(ih.len())]),
        kind: Some(k),
    }
}

fn target(ih: &str) -> TargetCmd {
    TargetCmd {
        infohash: ih.to_owned(),
    }
}

fn is_paused(o: &TorrentInfo) -> bool {
    o.status == TorrentStatus::Paused as i32
}

fn is_checking(o: &TorrentInfo) -> bool {
    o.status == TorrentStatus::Checking as i32 || o.status == TorrentStatus::Queued as i32
}

pub fn plan(desired: &[Desired], observed: &[TorrentInfo]) -> Plan {
    let mut p = Plan::default();
    for d in desired {
        let ih = d.infohash.as_str();
        let obs = observed.iter().find(|o| o.infohash == ih);

        if d.removed {
            match obs {
                Some(_) => p.commands.push(cmd(
                    "rm",
                    ih,
                    Kind::Remove(RemoveCmd {
                        infohash: ih.to_owned(),
                        delete_files: d.delete_files,
                    }),
                )),
                None => p.finished_removals.push(ih.to_owned()),
            }
            continue;
        }

        let add = |paused: bool| {
            cmd(
                "add",
                ih,
                Kind::Add(AddCmd {
                    source: Some(Source::Magnet(d.magnet.clone())),
                    save_subdir: d.save_subdir.clone(),
                    start_paused: paused,
                }),
            )
        };

        // Une source (nœud qui a déjà le contenu complet) annonce aussi au tracker privé, pour que les copies la trouvent.
        if let (Some(url), Some(o)) = (&d.source_tracker, obs) {
            if !is_checking(o) && o.progress >= DONE && d.phase == Phase::Active {
                p.commands.push(cmd(
                    "trk",
                    ih,
                    Kind::AddTracker(AddTrackerCmd {
                        infohash: ih.to_owned(),
                        announce_url: url.clone(),
                    }),
                ));
                p.tracker_added.push(ih.to_owned());
            }
        }

        if let (true, Some(o)) = (d.fetch_metainfo, obs) {
            if !is_checking(o) && o.progress >= DONE && d.phase == Phase::Active {
                p.commands
                    .push(cmd("fmi", ih, Kind::FetchMetainfo(target(ih))));
                p.metainfo_asked.push(ih.to_owned());
            }
        }

        match (d.phase, obs) {
            // Liens à créer AVANT l'ajout : la commande est complétée par `reconcile_locked` (elle dépend des deux
            // .torrent) et la phase n'avance que sur confirmation de l'agent. Idempotent : rejouable sans risque.
            (Phase::Linking, _) => p.commands.push(cmd(
                "lnk",
                ih,
                Kind::LinkTree(LinkTreeCmd {
                    dest_dir: ih.to_owned(),
                    ..LinkTreeCmd::default()
                }),
            )),
            // Copie par swarm : ajout (en marche) du torrent réécrit ; la progression vient des autres nœuds.
            (Phase::Transfer, None) => {
                p.swarm_adds.insert(ih.to_owned());
                p.commands.push(add(false));
            }
            (Phase::Transfer, Some(o)) => {
                if let Some(v) = d.copy_dl_limit {
                    p.commands.push(cmd(
                        "lim",
                        ih,
                        Kind::TorrentLimits(proto::pb::TorrentLimitsCmd {
                            infohash: ih.to_owned(),
                            dl_limit: v,
                            ul_limit: 0,
                        }),
                    ));
                    p.limits_applied.push((ih.to_owned(), v));
                }
                if !is_checking(o) && o.progress >= DONE {
                    // Données complètes : on quitte le torrent « privé » en gardant les fichiers.
                    p.commands.push(cmd(
                        "rtg",
                        ih,
                        Kind::Remove(RemoveCmd {
                            infohash: ih.to_owned(),
                            delete_files: false,
                        }),
                    ));
                    p.phase_updates.push((ih.to_owned(), Phase::Retarget));
                } else if d.paused && !is_paused(o) {
                    p.commands.push(cmd("pau", ih, Kind::Pause(target(ih))));
                } else if !d.paused && is_paused(o) {
                    p.commands.push(cmd("res", ih, Kind::Resume(target(ih))));
                }
            }
            // Retrait demandé : on le redit tant que le client n'a pas fini, puis on ré-ajoute l'original EN PAUSE.
            (Phase::Retarget, Some(_)) => p.commands.push(cmd(
                "rtg",
                ih,
                Kind::Remove(RemoveCmd {
                    infohash: ih.to_owned(),
                    delete_files: false,
                }),
            )),
            (Phase::Retarget, None) => {
                p.phase_updates.push((ih.to_owned(), Phase::Adding));
                p.commands.push(add(true));
            }
            // Absent : on (ré)ajoute. Un réplica repart toujours en pause pour être revérifié.
            (Phase::Adding, None) => p.commands.push(add(true)),
            (Phase::Rechecking, None) => {
                p.phase_updates.push((ih.to_owned(), Phase::Adding));
                p.commands.push(add(true));
            }
            (Phase::Active, None) => p.commands.push(add(d.paused)),

            (Phase::Adding, Some(_)) => {
                p.commands.push(cmd("chk", ih, Kind::Recheck(target(ih))));
                p.phase_updates.push((ih.to_owned(), Phase::Rechecking));
            }
            (Phase::Rechecking, Some(o)) => {
                // Vérification terminée ET données complètes : on passe en seed (sauf pause admin).
                if !is_checking(o) && o.progress >= DONE {
                    p.phase_updates.push((ih.to_owned(), Phase::Active));
                    if !d.paused && is_paused(o) {
                        p.commands.push(cmd("res", ih, Kind::Resume(target(ih))));
                    }
                }
                // Sinon : vérification en cours, ou données incomplètes -> le réplica reste en pause.
            }
            (Phase::Active, Some(o)) => {
                if d.paused && !is_paused(o) {
                    p.commands.push(cmd("pau", ih, Kind::Pause(target(ih))));
                } else if !d.paused && is_paused(o) {
                    p.commands.push(cmd("res", ih, Kind::Resume(target(ih))));
                }
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "1111111111111111111111111111111111111111";

    fn d(phase: Phase) -> Desired {
        Desired {
            infohash: H.into(),
            magnet: format!("magnet:?xt=urn:btih:{H}"),
            save_subdir: "tv".into(),
            paused: false,
            phase,
            removed: false,
            delete_files: false,
            source_tracker: None,
            fetch_metainfo: false,
            copy_dl_limit: None,
        }
    }
    fn o(status: TorrentStatus, progress: f32) -> TorrentInfo {
        TorrentInfo {
            infohash: H.into(),
            status: status as i32,
            progress,
            ..TorrentInfo::default()
        }
    }
    fn kinds(p: &Plan) -> Vec<&'static str> {
        p.commands
            .iter()
            .map(|c| match c.kind {
                Some(Kind::Add(_)) => "add",
                Some(Kind::Pause(_)) => "pause",
                Some(Kind::Resume(_)) => "resume",
                Some(Kind::Recheck(_)) => "recheck",
                Some(Kind::Remove(_)) => "remove",
                _ => "?",
            })
            .collect()
    }

    #[test]
    fn missing_torrent_is_added_and_converged_is_empty() {
        let p = plan(&[d(Phase::Active)], &[]);
        assert_eq!(kinds(&p), ["add"]);
        let p = plan(&[d(Phase::Active)], &[o(TorrentStatus::Downloading, 0.3)]);
        assert!(p.commands.is_empty() && p.phase_updates.is_empty());
    }

    #[test]
    fn add_respects_paused_flag() {
        let mut x = d(Phase::Active);
        x.paused = true;
        let p = plan(&[x], &[]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Add(a)) if a.start_paused));
    }

    #[test]
    fn pause_resume_follow_master() {
        let mut x = d(Phase::Active);
        x.paused = true;
        assert_eq!(
            kinds(&plan(&[x.clone()], &[o(TorrentStatus::Downloading, 0.1)])),
            ["pause"]
        );
        assert!(plan(&[x], &[o(TorrentStatus::Paused, 0.1)])
            .commands
            .is_empty());
        let y = d(Phase::Active);
        assert_eq!(
            kinds(&plan(&[y], &[o(TorrentStatus::Paused, 0.1)])),
            ["resume"]
        );
    }

    #[test]
    fn replica_lifecycle_add_recheck_seed() {
        // 1. absent -> ajout EN PAUSE
        let p = plan(&[d(Phase::Adding)], &[]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Add(a)) if a.start_paused));
        // 2. présent -> recheck, phase Rechecking
        let p = plan(&[d(Phase::Adding)], &[o(TorrentStatus::Paused, 0.0)]);
        assert_eq!(kinds(&p), ["recheck"]);
        assert_eq!(p.phase_updates, vec![(H.to_owned(), Phase::Rechecking)]);
        // 3. vérification en cours -> on attend
        let p = plan(&[d(Phase::Rechecking)], &[o(TorrentStatus::Checking, 0.5)]);
        assert!(p.commands.is_empty() && p.phase_updates.is_empty());
        // 3b. données incomplètes après vérification -> reste en pause, pas de boucle de recheck
        let p = plan(&[d(Phase::Rechecking)], &[o(TorrentStatus::Paused, 0.4)]);
        assert!(p.commands.is_empty() && p.phase_updates.is_empty());
        // 4. complet et en pause -> seed
        let p = plan(&[d(Phase::Rechecking)], &[o(TorrentStatus::Paused, 1.0)]);
        assert_eq!(kinds(&p), ["resume"]);
        assert_eq!(p.phase_updates, vec![(H.to_owned(), Phase::Active)]);
        // 5. régime établi
        assert!(plan(&[d(Phase::Active)], &[o(TorrentStatus::Seeding, 1.0)])
            .commands
            .is_empty());
    }

    #[test]
    fn swarm_replica_transfers_retargets_to_the_original_then_seeds() {
        // 1. absent : ajout du torrent RÉÉCRIT, en marche.
        let p = plan(&[d(Phase::Transfer)], &[]);
        assert_eq!(kinds(&p), ["add"]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Add(a)) if !a.start_paused));
        assert!(p.swarm_adds.contains(H));
        // 2. téléchargement en cours : on attend, sans rien envoyer.
        let p = plan(&[d(Phase::Transfer)], &[o(TorrentStatus::Downloading, 0.5)]);
        assert!(p.commands.is_empty() && p.phase_updates.is_empty());
        // 2b. vérification initiale d'un contenu complet : on attend la fin avant de conclure.
        let p = plan(&[d(Phase::Transfer)], &[o(TorrentStatus::Checking, 1.0)]);
        assert!(p.commands.is_empty());
        // 3. complet : retrait SANS les fichiers.
        let p = plan(&[d(Phase::Transfer)], &[o(TorrentStatus::Seeding, 1.0)]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Remove(r)) if !r.delete_files));
        assert_eq!(p.phase_updates, vec![(H.to_owned(), Phase::Retarget)]);
        // 4. le client n'a pas fini de retirer : on le redit, sans changer de phase.
        let p = plan(&[d(Phase::Retarget)], &[o(TorrentStatus::Seeding, 1.0)]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Remove(r)) if !r.delete_files));
        assert!(p.phase_updates.is_empty());
        // 5. retiré : l'ORIGINAL est ré-ajouté en pause (pas dans `swarm_adds`), puis le cycle habituel reprend.
        let p = plan(&[d(Phase::Retarget)], &[]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Add(a)) if a.start_paused));
        assert!(p.swarm_adds.is_empty());
        assert_eq!(p.phase_updates, vec![(H.to_owned(), Phase::Adding)]);
        let p = plan(&[d(Phase::Adding)], &[o(TorrentStatus::Paused, 1.0)]);
        assert_eq!(kinds(&p), ["recheck"]);
    }

    #[test]
    fn swarm_replica_removed_manually_restarts_the_transfer() {
        // Le torrent disparaît pendant la copie : on repart du torrent réécrit, pas de l'original.
        let p = plan(&[d(Phase::Transfer)], &[]);
        assert!(p.swarm_adds.contains(H));
    }

    #[test]
    fn a_complete_node_adds_the_private_tracker_once_asked() {
        let mut x = d(Phase::Active);
        x.source_tracker = Some("http://m:7444/announce/n/t".into());
        // Pas encore complet : on attend.
        assert!(plan(&[x.clone()], &[o(TorrentStatus::Downloading, 0.4)])
            .commands
            .is_empty());
        let p = plan(&[x.clone()], &[o(TorrentStatus::Seeding, 1.0)]);
        assert!(
            matches!(&p.commands[0].kind, Some(Kind::AddTracker(t)) if t.announce_url == "http://m:7444/announce/n/t")
        );
        assert_eq!(p.tracker_added, vec![H.to_owned()]);
        // Une fois mémorisé par le master (`source_tracker: None`), plus rien.
        x.source_tracker = None;
        assert!(plan(&[x], &[o(TorrentStatus::Seeding, 1.0)])
            .commands
            .is_empty());
    }

    #[test]
    fn a_complete_magnet_torrent_asks_its_node_for_the_torrent_file() {
        let mut x = d(Phase::Active);
        x.fetch_metainfo = true;
        assert!(plan(&[x.clone()], &[o(TorrentStatus::Downloading, 0.4)])
            .commands
            .is_empty());
        let p = plan(&[x.clone()], &[o(TorrentStatus::Seeding, 1.0)]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::FetchMetainfo(t)) if t.infohash == H));
        assert_eq!(p.metainfo_asked, vec![H.to_owned()]);
        x.fetch_metainfo = false;
        assert!(plan(&[x], &[o(TorrentStatus::Seeding, 1.0)])
            .commands
            .is_empty());
    }

    #[test]
    fn copy_speed_limit_is_sent_once_during_the_transfer() {
        let mut x = d(Phase::Transfer);
        x.copy_dl_limit = Some(5_000_000);
        let p = plan(&[x.clone()], &[o(TorrentStatus::Downloading, 0.2)]);
        assert!(matches!(&p.commands[0].kind,
            Some(Kind::TorrentLimits(l)) if l.dl_limit == 5_000_000 && l.ul_limit == 0));
        assert_eq!(p.limits_applied, vec![(H.to_owned(), 5_000_000)]);
        // Déjà appliqué (le master a mémorisé) : plus rien.
        x.copy_dl_limit = None;
        assert!(plan(&[x], &[o(TorrentStatus::Downloading, 0.5)])
            .commands
            .is_empty());
    }

    #[test]
    fn a_linked_cross_seed_asks_for_its_links_before_the_torrent_is_added() {
        // Tant que l'agent n'a pas confirmé, on (re)demande les liens, sans ajouter le torrent.
        let p = plan(&[d(Phase::Linking)], &[]);
        assert_eq!(p.commands.len(), 1);
        assert!(matches!(&p.commands[0].kind, Some(Kind::LinkTree(l)) if l.dest_dir == H));
        assert!(
            p.phase_updates.is_empty(),
            "la phase n'avance que sur confirmation de l'agent"
        );
        // Une fois les liens confirmés (phase Adding), le cycle habituel reprend : ajout en pause, revérification.
        let p = plan(&[d(Phase::Adding)], &[]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Add(a)) if a.start_paused));
    }

    #[test]
    fn replica_removed_manually_restarts_cycle() {
        let p = plan(&[d(Phase::Rechecking)], &[]);
        assert_eq!(kinds(&p), ["add"]);
        assert_eq!(p.phase_updates, vec![(H.to_owned(), Phase::Adding)]);
    }

    #[test]
    fn tombstone_removes_then_finishes() {
        let mut x = d(Phase::Active);
        x.removed = true;
        x.delete_files = true;
        let p = plan(&[x.clone()], &[o(TorrentStatus::Seeding, 1.0)]);
        assert!(matches!(&p.commands[0].kind, Some(Kind::Remove(r)) if r.delete_files));
        let p = plan(&[x], &[]);
        assert!(p.commands.is_empty());
        assert_eq!(p.finished_removals, vec![H.to_owned()]);
    }

    #[test]
    fn unmanaged_torrents_are_never_touched() {
        let stranger = TorrentInfo {
            infohash: "f".repeat(40),
            ..TorrentInfo::default()
        };
        let p = plan(&[], &[stranger]);
        assert!(p.commands.is_empty() && p.finished_removals.is_empty());
    }

    #[test]
    fn plan_is_idempotent_on_converged_state() {
        let desired = [d(Phase::Active)];
        let observed = [o(TorrentStatus::Seeding, 1.0)];
        assert!(plan(&desired, &observed).commands.is_empty());
        assert!(plan(&desired, &observed).commands.is_empty());
    }
}
