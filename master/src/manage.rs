//! Orchestration : état désiré (torrents, assignations), scheduler, réplication, réconciliation.
//!
//! Toutes les mutations passent par `reconcile_lock` : une seule décision à la fois, sans quoi deux
//! déclencheurs simultanés (événement + balayage) pourraient assigner deux fois le même torrent.
use crate::reconcile::{self, Desired, Phase};
use crate::scheduler::{self, Candidate};
use crate::state::{self, NodeState};
use crate::store::{unix_now, Role, StoreError, TorrentDef, TorrentFlag};
use crate::Core;
use proto::infohash::{magnet_infohash, normalize_infohash};
use proto::pb::{command::Kind, master_msg::M as MasterM, DesiredState};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::task::JoinHandle;

const METRIC_RETENTION_SECS: i64 = 30 * 24 * 3600;

#[derive(Clone, Debug)]
pub enum Source {
    Magnet(String),
    Infohash(String),
    /// Contenu d'un fichier .torrent (validé et analysé avant stockage ; indispensable aux trackers privés).
    File(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct NewTorrent {
    pub source: Source,
    /// Relatif à la racine locale de chaque agent.
    pub save_subdir: String,
    /// Labels que le nœud doit TOUS posséder.
    pub requires: Vec<String>,
    /// Label du groupe cible : à la complétion, les nœuds portant ce label reçoivent une copie.
    pub replicate_group: Option<String>,
    /// Taille estimée en octets (0 = inconnue) pour le contrôle d'espace disque.
    pub size_hint: u64,
    /// Nom d'affichage (vide = déduit du fichier .torrent ou du magnet).
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    #[error("requête invalide: {0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Découpe les commandes en messages d'au plus `DESIRED_CHUNK_BYTES` (les ajouts par fichier sont lourds).
/// Toujours au moins un message, même sans commande : il porte la révision que l'agent doit acquitter.
fn chunk_commands(commands: Vec<proto::pb::Command>) -> Vec<Vec<proto::pb::Command>> {
    use prost::Message;
    let mut out = vec![Vec::new()];
    let mut size = 0usize;
    for c in commands {
        let n = c.encoded_len();
        if size + n > proto::DESIRED_CHUNK_BYTES && !out.last().is_some_and(Vec::is_empty) {
            out.push(Vec::new());
            size = 0;
        }
        size += n;
        if let Some(last) = out.last_mut() {
            last.push(c);
        }
    }
    out
}

pub fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 64
        && l.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl Core {
    // ---------- API de gestion (future API admin) ----------

    pub async fn add_torrent(self: &Arc<Self>, t: NewTorrent) -> Result<String, ManageError> {
        self.add_torrent_as("admin", t).await
    }

    pub async fn add_torrent_as(
        self: &Arc<Self>,
        actor: &str,
        t: NewTorrent,
    ) -> Result<String, ManageError> {
        let mut file: Option<&[u8]> = None;
        let mut name = t.name.trim().chars().take(200).collect::<String>();
        let mut size_hint = t.size_hint;
        let (magnet, ih) = match &t.source {
            Source::File(bytes) => {
                let m = proto::metainfo::parse(bytes)
                    .map_err(|e| ManageError::Invalid(format!("fichier .torrent: {e}")))?;
                file = Some(bytes);
                if name.is_empty() {
                    name = m.name.chars().take(200).collect();
                }
                if size_hint == 0 {
                    size_hint = m.size;
                }
                (format!("magnet:?xt=urn:btih:{}", m.infohash), m.infohash)
            }
            Source::Magnet(m) => (
                m.clone(),
                magnet_infohash(m).ok_or_else(|| ManageError::Invalid("magnet invalide".into()))?,
            ),
            Source::Infohash(h) => {
                let ih = normalize_infohash(h)
                    .ok_or_else(|| ManageError::Invalid("infohash invalide".into()))?;
                (format!("magnet:?xt=urn:btih:{ih}"), ih)
            }
        };
        proto::relpath::split_relative(&t.save_subdir)
            .map_err(|e| ManageError::Invalid(format!("save_subdir: {e}")))?;
        if !t.requires.iter().all(|l| valid_label(l))
            || t.replicate_group
                .as_deref()
                .is_some_and(|g| !valid_label(g))
        {
            return Err(ManageError::Invalid("label invalide".into()));
        }
        let def = TorrentDef {
            infohash: ih.clone(),
            magnet,
            save_subdir: t.save_subdir,
            requires: t.requires,
            replicate_group: t.replicate_group,
            size_hint,
            paused: false,
            completed: false,
            deleted: false,
            name,
            has_file: file.is_some(),
        };
        if !self.store.insert_torrent(&def, file)? {
            // Déjà connu : idempotent ; on le ressuscite s'il avait été supprimé.
            self.store
                .set_torrent_flag(&ih, TorrentFlag::Deleted, false)?;
        }
        self.store.audit(actor, "torrent_added", &ih);
        self.schedule_pending().await;
        Ok(ih)
    }

    pub async fn remove_torrent(
        self: &Arc<Self>,
        ih: &str,
        delete_files: bool,
    ) -> Result<(), ManageError> {
        self.remove_torrent_as("admin", ih, delete_files).await
    }

    pub async fn remove_torrent_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
        delete_files: bool,
    ) -> Result<(), ManageError> {
        let ih = normalize_infohash(ih)
            .ok_or_else(|| ManageError::Invalid("infohash invalide".into()))?;
        let _g = self.reconcile_lock.lock().await;
        self.store
            .get_torrent(&ih)?
            .ok_or_else(|| ManageError::Invalid("torrent inconnu".into()))?;
        self.store
            .set_torrent_flag(&ih, TorrentFlag::Deleted, true)?;
        let mut touched = Vec::new();
        for a in self
            .store
            .assignments_for_torrent(&ih)?
            .into_iter()
            .filter(|a| !a.removed)
        {
            self.store.mark_removed(&ih, &a.node_id, delete_files)?;
            self.store.bump_rev(&a.node_id)?;
            touched.push(a.node_id);
        }
        self.store.audit(
            actor,
            "torrent_removed",
            &if delete_files {
                format!("{ih} delete_files")
            } else {
                ih.clone()
            },
        );
        for n in touched {
            self.reconcile_locked(&n).await;
        }
        Ok(())
    }

    pub async fn set_torrent_paused(
        self: &Arc<Self>,
        ih: &str,
        paused: bool,
    ) -> Result<(), ManageError> {
        self.set_torrent_paused_as("admin", ih, paused).await
    }

    /// Modifie les labels requis et le groupe de réplication d'un torrent existant. Effets :
    /// - `requires` : conditionne les PROCHAINES assignations (un torrent « en attente » peut enfin partir) ;
    ///   le torrent déjà placé n'est pas déplacé ;
    /// - `replicate_group` : si le torrent est déjà terminé, les copies sont placées au prochain balayage
    ///   (déclenché immédiatement ici).
    pub async fn set_torrent_placement_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
        requires: Vec<String>,
        group: Option<String>,
    ) -> Result<(), ManageError> {
        let ih = normalize_infohash(ih)
            .ok_or_else(|| ManageError::Invalid("infohash invalide".into()))?;
        let mut clean: Vec<String> = Vec::new();
        for l in requires.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
            if !valid_label(l) {
                return Err(ManageError::Invalid("label invalide".into()));
            }
            if !clean.iter().any(|x| x == l) {
                clean.push(l.to_owned());
            }
        }
        if clean.len() > 20 {
            return Err(ManageError::Invalid("20 labels au maximum".into()));
        }
        let group = group.map(|g| g.trim().to_owned()).filter(|g| !g.is_empty());
        if group.as_deref().is_some_and(|g| !valid_label(g)) {
            return Err(ManageError::Invalid(
                "groupe de réplication invalide".into(),
            ));
        }
        {
            let _g = self.reconcile_lock.lock().await;
            let old = self
                .store
                .get_torrent(&ih)?
                .ok_or_else(|| ManageError::Invalid("torrent inconnu".into()))?;
            self.store
                .set_torrent_placement(&ih, &clean, group.as_deref())?;
            self.store.audit(
                actor,
                "torrent_placement_changed",
                &format!(
                    "{ih}|{}|{}|{}|{}",
                    old.requires.join(","),
                    clean.join(","),
                    old.replicate_group.unwrap_or_default(),
                    group.unwrap_or_default()
                ),
            );
        }
        self.schedule_pending().await;
        Ok(())
    }

    /// Envoie au nœud ses limites de débit voulues (si l'admin en a défini : sinon les réglages locaux du client sont
    /// laissés tels quels). Appelée à la première connexion de chaque session et à chaque modification.
    pub async fn apply_limits(&self, node_id: &str) -> bool {
        let Ok(Some(n)) = self
            .store
            .node_details()
            .map(|v| v.into_iter().find(|n| n.id == node_id))
        else {
            return false;
        };
        if !n.limits_managed {
            return false;
        }
        let cmd = proto::pb::Command {
            cmd_id: format!("lim-{}", unix_now()),
            kind: Some(Kind::SetLimits(proto::pb::SetLimitsCmd {
                dl_limit: n.dl_limit,
                ul_limit: n.ul_limit,
            })),
        };
        self.sessions.send(node_id, MasterM::Cmd(cmd)).await
    }

    /// Demande aux nœuds qui portent le torrent de revérifier ses fichiers. Renvoie le nombre de nœuds joints.
    pub async fn recheck_torrent_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
    ) -> Result<usize, ManageError> {
        let ih = normalize_infohash(ih)
            .ok_or_else(|| ManageError::Invalid("infohash invalide".into()))?;
        self.store
            .get_torrent(&ih)?
            .ok_or_else(|| ManageError::Invalid("torrent inconnu".into()))?;
        let mut sent = 0;
        for a in self
            .store
            .assignments_for_torrent(&ih)?
            .into_iter()
            .filter(|a| !a.removed)
        {
            let cmd = proto::pb::Command {
                cmd_id: format!("chk-adm-{}-{}", &ih[..8], unix_now()),
                kind: Some(Kind::Recheck(proto::pb::TargetCmd {
                    infohash: ih.clone(),
                })),
            };
            if self.sessions.send(&a.node_id, MasterM::Cmd(cmd)).await {
                sent += 1;
            }
        }
        if sent == 0 {
            return Err(ManageError::Invalid(
                "aucun nœud connecté ne porte ce torrent pour l'instant".into(),
            ));
        }
        self.store.audit(actor, "torrent_rechecked", &ih);
        Ok(sent)
    }

    pub async fn set_torrent_paused_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
        paused: bool,
    ) -> Result<(), ManageError> {
        let ih = normalize_infohash(ih)
            .ok_or_else(|| ManageError::Invalid("infohash invalide".into()))?;
        let _g = self.reconcile_lock.lock().await;
        self.store
            .get_torrent(&ih)?
            .ok_or_else(|| ManageError::Invalid("torrent inconnu".into()))?;
        self.store
            .set_torrent_flag(&ih, TorrentFlag::Paused, paused)?;
        self.store.audit(
            actor,
            if paused {
                "torrent_paused"
            } else {
                "torrent_resumed"
            },
            &ih,
        );
        let mut touched = Vec::new();
        for a in self
            .store
            .assignments_for_torrent(&ih)?
            .into_iter()
            .filter(|a| !a.removed)
        {
            self.store.bump_rev(&a.node_id)?;
            touched.push(a.node_id);
        }
        for n in touched {
            self.reconcile_locked(&n).await;
        }
        Ok(())
    }

    /// Nœud actuellement responsable du torrent (None = `Pending`).
    pub fn torrent_primary(&self, ih: &str) -> Option<String> {
        self.store
            .assignments_for_torrent(ih)
            .ok()?
            .into_iter()
            .find(|a| a.role == Role::Primary.as_str() && !a.removed)
            .map(|a| a.node_id)
    }

    // ---------- Déclencheurs ----------

    /// Un nœud a signalé la fin d'un téléchargement : déclenche la règle de réplication.
    pub async fn on_completed(self: &Arc<Self>, node_id: &str, ih: &str) {
        let Ok(Some(t)) = self.store.get_torrent(ih) else {
            return;
        };
        let holds = self
            .store
            .assignments_for_torrent(ih)
            .is_ok_and(|v| v.iter().any(|a| a.node_id == node_id && !a.removed));
        if !holds {
            return;
        }
        if t.completed {
            return; // déjà connu : évite un balayage par torrent complet à chaque FullState
        }
        let _ = self
            .store
            .set_torrent_flag(ih, TorrentFlag::Completed, true);
        self.store.audit(node_id, "torrent_completed", ih);
        self.schedule_pending().await;
    }

    /// Réponse à une demande de .torrent : acceptée seulement si le master l'a demandée à CE nœud, si le fichier est
    /// valide et porte l'infohash attendu, et si le torrent n'en avait pas encore. Les copies peuvent alors démarrer.
    pub async fn on_metainfo(self: &Arc<Self>, node_id: &str, ih: &str, bytes: &[u8]) {
        let ih = ih.to_ascii_lowercase();
        let asked = self
            .metainfo_asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(ih.clone(), node_id.to_owned()));
        let valid = asked && proto::metainfo::parse(bytes).is_ok_and(|m| m.infohash == ih);
        if !valid {
            self.store.audit(node_id, "metainfo_refused", &ih);
            return;
        }
        if self
            .store
            .set_torrent_file_if_missing(&ih, bytes)
            .unwrap_or(false)
        {
            self.store.audit(node_id, "metainfo_received", &ih);
            self.schedule_pending().await;
        }
    }

    /// Réconcilie un nœud (après FullState, mutation de l'état désiré…).
    pub async fn reconcile(self: &Arc<Self>, node_id: &str) {
        let _g = self.reconcile_lock.lock().await;
        self.reconcile_locked(node_id).await;
    }

    /// Assigne les torrents `Pending`, réassigne ceux dont le nœud est perdu, complète les réplicas.
    pub async fn schedule_pending(self: &Arc<Self>) {
        let _g = self.reconcile_lock.lock().await;
        if let Err(e) = self.schedule_locked().await {
            tracing::error!(error = %e, "ordonnancement impossible");
        }
    }

    /// Balayage périodique (délai de grâce, nœuds revenus en ligne, réplicas manquants).
    pub fn start_background(self: &Arc<Self>) -> JoinHandle<()> {
        let core = self.clone();
        // Historique du cluster : un point par minute (réglable), conservé 30 jours. Tâche à part du balayage.
        let sampler = self.clone();
        tokio::spawn(async move {
            // Premier point peu après le démarrage : l'historique n'est jamais vide plus que quelques secondes.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            loop {
                if let Err(e) = sampler.sample_metrics() {
                    tracing::warn!(error = %e, "échantillon de métriques impossible");
                }
                let _ = sampler
                    .store
                    .prune_metrics(unix_now() - METRIC_RETENTION_SECS);
                let _ = sampler.store.traffic_prune(
                    crate::traffic::hour_of(unix_now()) - crate::stats::RETENTION_HOURS,
                );
                let _ = sampler.store.torrent_traffic_prune(
                    crate::traffic::hour_of(unix_now()) - crate::stats::TORRENT_RETENTION_HOURS,
                );
                tokio::time::sleep(sampler.metrics_interval()).await;
            }
        });
        // Alertes : évaluation toutes les 15 s (les délais de confirmation se comptent en minutes).
        let alerter = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                alerter.alert_tick().await;
            }
        });
        // Cross-seed : un passage toutes les 5 minutes (sans effet tant qu'aucun indexeur n'est configuré).
        let xs = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                xs.xseed_sweep().await;
            }
        });
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(core.settings.sweep_interval).await;
                core.schedule_pending().await;
            }
        })
    }

    // ---------- Internes (verrou déjà pris) ----------

    fn candidates(&self) -> Result<Vec<Candidate>, StoreError> {
        Ok(self
            .store
            .list_nodes()?
            .into_iter()
            .filter_map(|n| {
                Some(Candidate {
                    state: NodeState::parse(&n.state)?,
                    disk_free: self.telemetry(&n.id).disk_free,
                    load: self.store.active_load(&n.id).unwrap_or(0),
                    labels: n.labels,
                    id: n.id,
                })
            })
            .collect())
    }

    async fn schedule_locked(self: &Arc<Self>) -> Result<(), StoreError> {
        let now = unix_now();
        let grace = i64::try_from(self.settings.offline_grace.as_secs()).unwrap_or(i64::MAX);
        let reserve = self.settings.disk_reserve;
        let nodes = self.store.list_nodes()?;
        let mut touched: HashSet<String> = HashSet::new();
        // Les torrents du cross-seed vivent sur le nœud qui a déjà les fichiers : jamais placés ni déplacés ailleurs.
        let xseed = self.store.xseed_children()?;

        for t in self.store.list_torrents()? {
            if xseed.contains_key(&t.infohash) {
                continue;
            }
            let asg = self.store.assignments_for_torrent(&t.infohash)?;
            // Jamais de nœud qui détient déjà une ligne (y compris un tombstone) pour ce torrent.
            let exclude: HashSet<String> = asg.iter().map(|a| a.node_id.clone()).collect();
            let primary = asg
                .iter()
                .find(|a| a.role == Role::Primary.as_str() && !a.removed);

            if let Some(p) = primary {
                let lost = nodes.iter().find(|n| n.id == p.node_id).is_some_and(|n| {
                    NodeState::parse(&n.state).is_some_and(|st| {
                        st == NodeState::Revoked
                            || state::is_reassignable(st, n.offline_since, now, grace)
                    })
                });
                if !lost {
                    continue;
                }
                // On ne retire l'ancien nœud qu'une fois un remplaçant conforme trouvé.
                let Some(new) = scheduler::pick_node(
                    &self.candidates()?,
                    &t.requires,
                    t.size_hint,
                    reserve,
                    &exclude,
                ) else {
                    continue;
                };
                self.store.mark_removed(&t.infohash, &p.node_id, false)?;
                self.store.bump_rev(&p.node_id)?;
                self.store.insert_assignment(
                    &t.infohash,
                    &new,
                    Role::Primary,
                    Phase::Active.as_str(),
                )?;
                self.store.bump_rev(&new)?;
                self.store.audit(
                    "system",
                    "torrent_reassigned",
                    &format!("{} {} -> {new}", t.infohash, p.node_id),
                );
                touched.insert(new);
                continue;
            }

            if let Some(n) = scheduler::pick_node(
                &self.candidates()?,
                &t.requires,
                t.size_hint,
                reserve,
                &exclude,
            ) {
                self.store.insert_assignment(
                    &t.infohash,
                    &n,
                    Role::Primary,
                    Phase::Active.as_str(),
                )?;
                self.store.bump_rev(&n)?;
                self.store
                    .audit("system", "torrent_assigned", &format!("{} {n}", t.infohash));
                touched.insert(n);
            }
            // Sinon : reste Pending (aucun nœud conforme), jamais assigné à un nœud non conforme.
        }

        // Réplication : torrents terminés avec un groupe cible.
        for t in self
            .store
            .list_torrents()?
            .into_iter()
            .filter(|t| t.completed)
        {
            let Some(group) = &t.replicate_group else {
                continue;
            };
            // Les `requires` conditionnent l'ACQUISITION (ex. VPN) ; un réplica ne fait que copier
            // et seeder : il suffit d'appartenir au groupe cible et d'avoir la place.
            let req = vec![group.clone()];
            let have: HashSet<String> = self
                .store
                .assignments_for_torrent(&t.infohash)?
                .into_iter()
                .map(|a| a.node_id)
                .collect();
            // Copie par swarm sur un torrent ajouté par magnet : on attend que le master ait le .torrent (demandé à
            // un nœud qui l'a complet). Si le client ne sait pas l'exporter, repli sur l'ancien cycle (fichiers présents).
            if self.swarm().is_some() && !t.has_file && !self.metainfo_failed(&t.infohash) {
                touched.extend(have.iter().cloned());
                continue;
            }
            for c in self.candidates()? {
                if have.contains(&c.id) || !scheduler::is_eligible(&c, &req, t.size_hint, reserve) {
                    continue;
                }
                // Avec le tracker privé et le .torrent : copie par le protocole BitTorrent (Transfer), sans stockage
                // partagé. Sinon : ajout en pause + Recheck, les données étant supposées déjà présentes (cycle de
                // `reconcile::plan`).
                let phase = if self.swarm().is_some() && t.has_file {
                    Phase::Transfer
                } else {
                    Phase::Adding
                };
                self.store
                    .insert_assignment(&t.infohash, &c.id, Role::Replica, phase.as_str())?;
                // Les nœuds qui ont déjà le contenu doivent annoncer au tracker : on les réconcilie aussi.
                touched.extend(have.iter().cloned());
                self.store.bump_rev(&c.id)?;
                self.store.audit(
                    "system",
                    if phase == Phase::Transfer {
                        "replica_transfer"
                    } else {
                        "replica_assigned"
                    },
                    &format!("{} {}", t.infohash, c.id),
                );
                touched.insert(c.id);
            }
        }

        // Réconcilie les nœuds touchés + ceux dont l'état observé a pu dériver.
        for n in &touched {
            self.reconcile_locked(n).await;
        }
        Ok(())
    }

    pub(crate) async fn reconcile_locked(&self, node_id: &str) {
        let tel = self.telemetry(node_id);
        // Pas d'état observé fiable (aucun FullState) ou pas de tunnel : rien à calculer.
        if tel.full_states == 0 || !self.sessions.is_connected(node_id) {
            return;
        }
        let rows = match self.store.desired_for_node(node_id) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "état désiré illisible");
                return;
            }
        };
        let with_file: HashSet<String> = rows
            .iter()
            .filter(|(_, t)| t.has_file)
            .map(|(a, _)| a.infohash.clone())
            .collect();
        let desired: Vec<Desired> = rows
            .into_iter()
            .map(|(a, t)| {
                let phase = Phase::parse(&a.phase).unwrap_or(Phase::Active);
                // Une source n'annonce au tracker privé que tant qu'une copie est en cours de transfert.
                let source_tracker = self
                    .swarm()
                    .filter(|_| {
                        t.has_file
                            && !a.removed
                            && phase == Phase::Active
                            && !self
                                .store
                                .tracker_added(&a.infohash, node_id)
                                .unwrap_or(true)
                            && self.store.has_transfer(&a.infohash).unwrap_or(false)
                    })
                    .and_then(|sw| swarm_url(sw, &a.infohash, node_id));
                let fetch_metainfo = self.swarm().is_some()
                    && !t.has_file
                    && t.replicate_group.is_some()
                    && !a.removed
                    && phase == Phase::Active
                    && !self
                        .metainfo_asked
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .contains(&(a.infohash.clone(), node_id.to_owned()));
                // Plafond de débit de la copie : appliqué (ou mis à jour) une fois qu'elle existe sur le nœud.
                let copy_dl_limit = (self.swarm().is_some() && phase == Phase::Transfer)
                    .then(|| {
                        let want = self.copy_dl_limit();
                        let have = self
                            .store
                            .copy_limit_applied(&a.infohash, node_id)
                            .unwrap_or(None);
                        (have != Some(want) && !(want == 0 && have.is_none())).then_some(want)
                    })
                    .flatten();
                Desired {
                    copy_dl_limit,
                    fetch_metainfo,
                    infohash: a.infohash,
                    magnet: t.magnet,
                    save_subdir: t.save_subdir,
                    paused: t.paused,
                    phase,
                    removed: a.removed,
                    delete_files: a.delete_files,
                    source_tracker,
                }
            })
            .collect();
        let plan = reconcile::plan(&desired, &tel.torrents);
        let changed = !plan.phase_updates.is_empty() || !plan.finished_removals.is_empty();
        let cur = self.store.node_rev(node_id).unwrap_or(0);
        let rev = cur + u64::from(changed);
        if plan.commands.is_empty() && !changed && tel.applied_rev == rev {
            return; // convergé
        }
        let mut link_failures: Vec<(String, &'static str)> = Vec::new();
        let commands = plan
            .commands
            .iter()
            .cloned()
            .filter_map(|mut c| {
                c.cmd_id = format!("r{rev}-{}", c.cmd_id);
                // Liens physiques du cross-seed : la commande dépend des deux .torrent, elle est construite ici.
                if let Some(Kind::LinkTree(l)) = c.kind.as_mut() {
                    match self.build_link_cmd(&l.dest_dir.clone()) {
                        Ok(full) => *l = full,
                        Err(why) => {
                            link_failures.push((l.dest_dir.clone(), why));
                            return None;
                        }
                    }
                }
                // Les torrents ajoutés par fichier sont envoyés avec leur contenu (le magnet ne suffit pas
                // pour un tracker privé). Chargé à la demande : seuls les `Add` en ont besoin.
                if let Some(Kind::Add(a)) = c.kind.as_mut() {
                    if let Some(proto::pb::add_cmd::Source::Magnet(m)) = &a.source {
                        if let Some(ih) = magnet_infohash(m).filter(|h| with_file.contains(h)) {
                            if let Ok(Some(bytes)) = self.store.torrent_file(&ih) {
                                // Copie par swarm : même torrent (même infohash) mais n'annonçant QU'AU tracker privé
                                // du master, avec un jeton propre à ce nœud. Les autres ajouts portent l'original.
                                let bytes = if plan.swarm_adds.contains(&ih) {
                                    self.swarm()
                                        .and_then(|sw| {
                                            let url = swarm_url(sw, &ih, node_id)?;
                                            proto::metainfo::rewrite_announce(&bytes, &url).ok()
                                        })
                                        .unwrap_or(bytes)
                                } else {
                                    bytes
                                };
                                a.source = Some(proto::pb::add_cmd::Source::TorrentFile(bytes));
                            }
                        }
                    }
                }
                Some(c)
            })
            .collect();
        // Liens impossibles à construire (fichiers de torrent manquants…) : on renonce au candidat.
        for (ih, why) in link_failures {
            self.xseed_discard_state(&ih, node_id, &format!("liens impossibles : {why}"));
        }
        let mut sent = true;
        for chunk in chunk_commands(commands) {
            sent &= self
                .sessions
                .send(
                    node_id,
                    MasterM::Desired(DesiredState {
                        rev,
                        commands: chunk,
                    }),
                )
                .await;
            if !sent {
                break;
            }
        }
        // Les transitions de phase ne sont persistées que si la commande est partie : sinon
        // un réplica resterait bloqué en `Rechecking` sans que le Recheck ait été envoyé.
        if sent {
            for ih in &plan.metainfo_asked {
                self.metainfo_asked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert((ih.clone(), node_id.to_owned()));
            }
            for (ih, v) in &plan.limits_applied {
                let _ = self.store.mark_copy_limit(ih, node_id, *v);
            }
            for ih in &plan.tracker_added {
                let _ = self.store.mark_tracker_added(ih, node_id);
                self.store
                    .audit("system", "tracker_added", &format!("{ih} {node_id}"));
            }
        }
        if sent && changed {
            for (ih, ph) in &plan.phase_updates {
                let _ = self.store.set_phase(ih, node_id, ph.as_str());
                if matches!(ph, Phase::Retarget | Phase::Active) {
                    self.store.audit(
                        "system",
                        "replica_phase",
                        &format!("{ih} {node_id} {}", ph.as_str()),
                    );
                }
            }
            for ih in &plan.finished_removals {
                let _ = self.store.delete_assignment(ih, node_id);
            }
            let _ = self.store.bump_rev(node_id);
        }
    }
}

/// URL d'annonce du tracker privé pour (torrent, nœud) ; `None` si l'infohash est illisible.
fn swarm_url(sw: &crate::Swarm, infohash: &str, node_id: &str) -> Option<String> {
    let ih: [u8; 20] = data_encoding::HEXLOWER
        .decode(infohash.to_ascii_lowercase().as_bytes())
        .ok()?
        .try_into()
        .ok()?;
    Some(format!(
        "{}{}",
        sw.public_base,
        sw.tracker.announce_path(&ih, node_id)
    ))
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use proto::pb::{add_cmd::Source as S, AddCmd, Command};

    fn big(i: usize, mb: usize) -> Command {
        Command {
            cmd_id: format!("add-{i}"),
            kind: Some(Kind::Add(AddCmd {
                source: Some(S::TorrentFile(vec![0u8; mb * 1024 * 1024])),
                save_subdir: String::new(),
                start_paused: false,
            })),
        }
    }

    #[test]
    fn desired_state_is_split_below_the_message_limit_and_never_empty() {
        assert_eq!(chunk_commands(vec![]).len(), 1, "la révision doit partir");
        // 5 × 3 Mio : 2 par message (6 Mio < 8), jamais au-dessus de la limite de l'agent.
        let chunks = chunk_commands((0..5).map(|i| big(i, 3)).collect());
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(
            chunks.iter().map(Vec::len).sum::<usize>(),
            5,
            "rien de perdu"
        );
        // Une commande isolée plus grosse que le seuil part seule au lieu de boucler.
        let one = chunk_commands(vec![big(0, 9)]);
        assert_eq!(one.len(), 1);
    }
}
