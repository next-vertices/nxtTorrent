//! Cross-seed intégré : pour un contenu complet sur un nœud, retrouve le MÊME contenu chez d'autres indexeurs et
//! l'ajoute sur ce nœud (autre torrent, autre tracker), pointé sur les fichiers existants. Voir l'ADR 0019.
//!
//! Garanties : appariement par liste de fichiers (jamais par le seul nom) ; aucun téléchargement de données (un candidat
//! incomplet après vérification est retiré, fichiers conservés) ; mode simulation par défaut ; indexeurs interrogés au
//! plus une fois par (torrent, nœud) et par période, avec un délai entre requêtes.
use crate::manage::ManageError;
use crate::reconcile::Phase;
use crate::store::{unix_now, IndexerRow, Role, TorrentDef, TorrentFlag};
use crate::torznab::{Item, Torznab, TorznabError};
use crate::Core;
use proto::metainfo::{self, Metainfo};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

const DONE: f32 = 0.9999;
/// Délai au-delà duquel des liens demandés mais jamais confirmés font renoncer au candidat.
const LINK_TIMEOUT_SECS: i64 = 300;
/// Attentes (secondes) après des échecs consécutifs (panne, délai dépassé, erreur serveur) : 1 min, 5 min, 15 min, 1 h,
/// puis 6 h. Un indexeur qui souffre n'est jamais harcelé.
const BACKOFF_SECS: [i64; 5] = [60, 300, 900, 3600, 21_600];
/// Attente imposée quand l'indexeur annonce son quota (limite de requêtes ou de téléchargements atteinte).
const QUOTA_PAUSE_SECS: i64 = 12 * 3600;
/// Attente par défaut après un 429 sans `Retry-After`, et bornes de celle qu'il demande.
const RATE_LIMIT_DEFAULT_SECS: u64 = 300;
const RATE_LIMIT_MIN_SECS: u64 = 60;
const RATE_LIMIT_MAX_SECS: u64 = 24 * 3600;
/// Un résultat déjà examiné (même titre et même taille chez cet indexeur) n'est pas retéléchargé avant ce délai.
const SEEN_TTL_SECS: i64 = 30 * 24 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XseedMode {
    /// Désactivé.
    Off,
    /// Simulation : cherche et journalise ce qui SERAIT ajouté, sans rien ajouter (défaut).
    Dry,
    /// Ajoute réellement les torrents trouvés.
    Active,
}

impl XseedMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Dry => "dry",
            Self::Active => "active",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "off" => Self::Off,
            "dry" => Self::Dry,
            "active" => Self::Active,
            _ => return None,
        })
    }
}

/// Pourquoi le garde n'émet pas la requête.
#[derive(Debug)]
enum Blocked {
    Disabled,
    Paused(i64),
    Budget(&'static str),
}

/// Pourquoi une requête à un indexeur n'a pas abouti.
#[derive(Debug)]
enum CallStop {
    /// Requête non émise (le garde l'a refusée) : voir `Blocked`.
    Blocked,
    Failed(TorznabError),
}

impl Blocked {
    fn describe(&self) -> String {
        match self {
            Self::Disabled => "indexeur désactivé".to_owned(),
            Self::Paused(until) => format!("indexeur en pause jusqu'à {until}"),
            Self::Budget(w) => format!("budget de requêtes ({w}) atteint"),
        }
    }
}

/// Issue de l'examen d'un résultat de recherche.
enum Tried {
    Matched,
    Skipped,
    /// Cesser de solliciter cet indexeur pour ce torrent (budget, pause ou erreur).
    Stop,
}

fn now_ms_precise() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Dossier ISOLÉ d'un candidat sur liens : `<dossier de l'original>/.nxt-xseed/<16 premiers caractères de son infohash>`.
fn link_dir(parent_subdir: &str, cand_ih: &str) -> String {
    let leaf = format!(".nxt-xseed/{}", &cand_ih[..16.min(cand_ih.len())]);
    if parent_subdir.is_empty() {
        leaf
    } else {
        format!("{}/{leaf}", parent_subdir.trim_end_matches('/'))
    }
}

/// Clé d'un résultat déjà examiné : indexeur + titre + taille (jamais le lien, qui porte la clé d'API).
fn seen_key(indexer_id: &str, item: &Item) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(indexer_id.as_bytes());
    h.update([0]);
    h.update(item.title.as_bytes());
    h.update([0]);
    h.update(item.size.to_be_bytes());
    data_encoding::HEXLOWER.encode(&h.finalize())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XseedSummary {
    /// Couples (torrent, nœud) pour lesquels les indexeurs ont été interrogés.
    pub searched: usize,
    /// Candidats identiques trouvés (simulation) ou ajoutés (mode actif).
    pub matched: usize,
}

/// Texte de recherche tiré du nom d'un torrent : séparateurs en espaces, groupe de release final retiré.
pub fn search_query(name: &str) -> String {
    let base = name.trim();
    // `Film.2024.1080p-GRP` -> `Film 2024 1080p` : le groupe change d'un tracker à l'autre.
    let base = match base.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty()
                && !tail.is_empty()
                && !tail.contains(['.', ' ', '_'])
                && tail.len() <= 16 =>
        {
            head
        }
        _ => base,
    };
    let q: String = base
        .chars()
        .map(|c| if matches!(c, '.' | '_') { ' ' } else { c })
        .collect();
    q.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tracker_host(m: &Metainfo) -> String {
    m.trackers
        .first()
        .and_then(|t| reqwest::Url::parse(t).ok())
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

impl Core {
    pub fn xseed_mode(&self) -> XseedMode {
        self.store
            .setting_get("xseed_mode")
            .ok()
            .flatten()
            .and_then(|v| XseedMode::parse(&v))
            .unwrap_or(XseedMode::Dry)
    }

    /// Réglages avancés (tests) : délai entre deux requêtes à un indexeur (ms), période de re-recherche (s).
    /// `settle_secs` : attente avant de juger incomplet un candidat dont la vérification est terminée (le client peut
    /// mettre un instant à passer en « vérification » après la commande ; 120 s par défaut).
    /// `delay_ms` remplace le délai de CHAQUE indexeur ; `u64::MAX` = utiliser leurs réglages (production).
    pub fn set_xseed_tuning(&self, delay_ms: u64, cooldown_secs: u64, settle_secs: u64) {
        self.xseed_settle_secs.store(settle_secs, Ordering::Relaxed);
        self.xseed_delay_ms.store(delay_ms, Ordering::Relaxed);
        self.xseed_cooldown_secs
            .store(cooldown_secs, Ordering::Relaxed);
    }

    /// Lance un passage en arrière-plan ; `false` si un passage est déjà en cours.
    pub fn xseed_run_in_background(self: &Arc<Self>) -> bool {
        if self
            .xseed_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let core = self.clone();
        tokio::spawn(async move {
            // Un passage manuel ignore la période de re-recherche : l'admin veut voir un résultat maintenant.
            core.xseed_sweep_with(true).await;
            core.xseed_running.store(false, Ordering::Release);
            core.bump_live();
        });
        true
    }

    /// Un test d'indexeur est-il permis maintenant ? Sinon, secondes à attendre.
    pub fn xseed_test_allowed(&self) -> Result<(), u64> {
        let wait = self.xseed_test_next_ms.load(Ordering::Relaxed) - now_ms_precise();
        if wait > 0 {
            Err(u64::try_from(wait / 1000 + 1).unwrap_or(1))
        } else {
            Ok(())
        }
    }

    /// Un test vient d'avoir lieu : 5 s avant le suivant, 30 s si l'indexeur a refusé l'accès (clé, compte, adresse IP).
    pub fn xseed_test_done(&self, access_refused: bool) {
        let gap = if access_refused { 30_000 } else { 5_000 };
        self.xseed_test_next_ms
            .store(now_ms_precise() + gap, Ordering::Relaxed);
    }

    /// Lève l'espacement des tests (tests automatisés uniquement).
    #[doc(hidden)]
    pub fn xseed_reset_test_guard(&self) {
        self.xseed_test_next_ms.store(0, Ordering::Relaxed);
    }

    pub fn xseed_is_running(&self) -> bool {
        self.xseed_running.load(Ordering::Acquire)
    }

    /// Un passage complet : suivi des candidats ajoutés, puis recherche pour un lot de torrents complets.
    pub async fn xseed_sweep(self: &Arc<Self>) -> XseedSummary {
        self.xseed_sweep_with(false).await
    }

    /// `force` : ignore la période de re-recherche (lancement manuel).
    pub async fn xseed_sweep_with(self: &Arc<Self>, force: bool) -> XseedSummary {
        let mut sum = XseedSummary::default();
        self.xseed_follow_up().await;
        let mode = self.xseed_mode();
        if mode == XseedMode::Off {
            return sum;
        }
        let mut clients: Vec<(IndexerRow, Torznab)> = Vec::new();
        for ix in self.store.indexers().unwrap_or_default() {
            if !ix.enabled {
                continue;
            }
            let Ok(Some(key)) = self.store.indexer_key(&ix.id) else {
                continue;
            };
            if let Ok(c) = Torznab::new(&ix.url, &key) {
                clients.push((ix, c));
            }
        }
        if clients.is_empty() {
            return sum;
        }
        let jobs = self.xseed_jobs(force);
        for (def, node_id) in jobs {
            let Ok(Some(file)) = self.store.torrent_file(&def.infohash) else {
                continue;
            };
            let Ok(parent) = metainfo::parse(&file) else {
                continue;
            };
            let Some(key) = parent.content_key() else {
                continue;
            };
            let query = search_query(&parent.name);
            if query.is_empty() {
                continue;
            }
            let mut searched = false;
            for (ix, client) in &clients {
                // Chaque requête passe par le garde : pause, budgets, délai entre requêtes, réaction aux erreurs.
                let Ok(items) = self.indexer_call(ix, client.search(&query)).await else {
                    continue;
                };
                searched = true;
                for item in items.into_iter().take(ix.max_candidates as usize) {
                    match self
                        .xseed_try(mode, &def, &node_id, &parent, &key, ix, client, item)
                        .await
                    {
                        Tried::Matched => sum.matched += 1,
                        Tried::Skipped => {}
                        // Budget épuisé, pause ou erreur : on cesse de solliciter cet indexeur pour ce torrent.
                        Tried::Stop => break,
                    }
                }
            }
            // La recherche n'est comptée (et la période de re-recherche n'est lancée) que si un indexeur a répondu :
            // un torrent dont les indexeurs étaient en pause sera retenté dès qu'ils reprennent.
            if searched {
                let _ = self.store.xseed_mark_checked(&def.infohash, &node_id);
                sum.searched += 1;
            }
        }
        if sum.matched > 0 || sum.searched > 0 {
            self.bump_live();
        }
        sum
    }

    /// Torrents complets, gérés, avec leur `.torrent`, à chercher (lot borné, une recherche par période).
    fn xseed_jobs(&self, force: bool) -> Vec<(TorrentDef, String)> {
        const BATCH: usize = 20;
        let cooldown =
            i64::try_from(self.xseed_cooldown_secs.load(Ordering::Relaxed)).unwrap_or(i64::MAX);
        let now = unix_now();
        let children = self.store.xseed_children().unwrap_or_default();
        let defs: std::collections::HashMap<String, TorrentDef> = self
            .store
            .list_torrents()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !t.deleted && t.has_file)
            .map(|t| (t.infohash.clone(), t))
            .collect();
        let mut jobs = Vec::new();
        for n in self.store.list_nodes().unwrap_or_default() {
            if n.state != "Online" {
                continue;
            }
            let tel = self.telemetry(&n.id);
            for o in &tel.torrents {
                if jobs.len() >= BATCH {
                    return jobs;
                }
                let Some(def) = defs.get(&o.infohash) else {
                    continue;
                };
                let complete =
                    o.progress >= DONE && o.status != proto::pb::TorrentStatus::Checking as i32;
                if !complete || children.contains_key(&o.infohash) {
                    continue;
                }
                let held = self
                    .store
                    .assignments_for_torrent(&o.infohash)
                    .unwrap_or_default()
                    .iter()
                    .any(|a| a.node_id == n.id && !a.removed && a.role != Role::Xseed.as_str());
                if !held {
                    continue;
                }
                let recent = self
                    .store
                    .xseed_checked_at(&o.infohash, &n.id)
                    .ok()
                    .flatten()
                    .is_some_and(|ts| !force && now - ts < cooldown);
                if !recent {
                    jobs.push((def.clone(), n.id.clone()));
                }
            }
        }
        jobs
    }

    /// Examine un résultat (simulation : journalisé ; actif : ajouté).
    #[allow(clippy::too_many_arguments)]
    async fn xseed_try(
        self: &Arc<Self>,
        mode: XseedMode,
        parent_def: &TorrentDef,
        node_id: &str,
        parent: &Metainfo,
        key: &str,
        ix: &IndexerRow,
        client: &Torznab,
        item: Item,
    ) -> Tried {
        let indexer = ix.name.as_str();
        // Pré-filtres SANS requête (chaque téléchargement de .torrent compte pour l'indexeur) : même taille, pas déjà
        // connu, pas déjà examiné récemment.
        if item.size != 0 && item.size != parent.size {
            return Tried::Skipped;
        }
        if let Some(ih) = &item.infohash {
            if *ih == parent_def.infohash
                || self.store.get_torrent(ih).ok().flatten().is_some()
                || self
                    .store
                    .xseed_known(&parent_def.infohash, node_id, ih)
                    .unwrap_or(false)
            {
                return Tried::Skipped;
            }
        }
        let seen_key = seen_key(&ix.id, &item);
        if self
            .store
            .xseed_seen(&seen_key, unix_now() - SEEN_TTL_SECS)
            .unwrap_or(false)
        {
            return Tried::Skipped;
        }
        let bytes = match self.indexer_call(ix, client.download(&item.link)).await {
            Ok(b) => b,
            // Lien mort (404…) : propre à ce résultat, sans conséquence pour l'indexeur ; on passe au suivant.
            Err(CallStop::Failed(TorznabError::BadResponse(_) | TorznabError::Remote(_))) => {
                return Tried::Skipped
            }
            Err(_) => return Tried::Stop,
        };
        // Quelle qu'en soit l'issue, ce résultat ne sera pas retéléchargé de sitôt.
        let _ = self.store.xseed_mark_seen(&seen_key);
        let Ok(cand) = metainfo::parse(&bytes) else {
            return Tried::Skipped;
        };
        if cand.infohash == parent_def.infohash
            || self
                .store
                .get_torrent(&cand.infohash)
                .ok()
                .flatten()
                .is_some()
            || self
                .store
                .xseed_known(&parent_def.infohash, node_id, &cand.infohash)
                .unwrap_or(false)
        {
            return Tried::Skipped;
        }
        // Disposition : mêmes fichiers aux mêmes chemins (clé de contenu identique), ou à défaut une correspondance fichier
        // par fichier (noms ou dossier racine différents). Sinon c'est un autre contenu : pas de bruit dans le journal.
        let exact = cand.content_key().as_deref() == Some(key);
        let pairs = metainfo::plan_links(parent, &cand);
        if !exact && pairs.is_none() {
            return Tried::Skipped;
        }
        let tracker = tracker_host(&cand);
        let title: String = item.title.chars().take(200).collect();
        let log = |state: &str, detail: &str| {
            let _ = self.store.xseed_upsert(
                &parent_def.infohash,
                node_id,
                &cand.infohash,
                indexer,
                &title,
                &tracker,
                state,
                detail,
            );
        };
        // Même tracker que l'original : ce serait un doublon, pas un cross-seed.
        if !tracker.is_empty()
            && parent
                .trackers
                .iter()
                .any(|t| tracker_host_of(t) == tracker)
        {
            log("refused", "même tracker que le torrent d'origine");
            return Tried::Skipped;
        }
        // Autre dossier racine ou autres noms : le client ne retrouverait pas les fichiers tels quels. On crée alors, côté
        // nœud, des LIENS PHYSIQUES vers les fichiers existants sous la disposition du candidat (aucune copie).
        let linked = metainfo::needs_links(parent, &cand);
        if linked && pairs.is_none() {
            log("refused", "disposition des fichiers non liable");
            return Tried::Skipped;
        }
        let how = if linked {
            format!(
                " (liens physiques, {} fichier{})",
                cand.files.len(),
                if cand.files.len() > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        };
        if mode == XseedMode::Dry {
            log("found", &format!("simulation : serait ajouté{how}"));
            return Tried::Matched;
        }
        match self
            .xseed_add(parent_def, node_id, &bytes, &cand, linked)
            .await
        {
            Ok(()) => {
                log(
                    "adding",
                    &if linked {
                        format!("liens en cours de création{how}, puis vérification")
                    } else {
                        "ajouté, vérification des fichiers en cours".to_owned()
                    },
                );
                Tried::Matched
            }
            Err(e) => {
                log("refused", &format!("ajout impossible : {e}"));
                Tried::Skipped
            }
        }
    }

    // ---------- Garde des indexeurs : protège les comptes des utilisateurs ----------

    /// Autorise ou non une requête à cet indexeur, d'après l'état ACTUEL en base (réglages, pause, budgets), et la
    /// cadence : attend si la précédente est trop récente. Enregistre la requête (elle compte dans les budgets).
    async fn indexer_gate(&self, id: &str) -> Result<(), Blocked> {
        let Some(ix) = self
            .store
            .indexers()
            .unwrap_or_default()
            .into_iter()
            .find(|i| i.id == id)
        else {
            return Err(Blocked::Disabled);
        };
        if !ix.enabled {
            return Err(Blocked::Disabled);
        }
        let now_s = unix_now();
        if ix.paused_until > now_s {
            return Err(Blocked::Paused(ix.paused_until));
        }
        let now_ms = now_s * 1000;
        if self
            .store
            .indexer_requests_since(id, now_ms - 3_600_000)
            .unwrap_or(0)
            >= ix.max_per_hour
        {
            return Err(Blocked::Budget("heure"));
        }
        if self
            .store
            .indexer_requests_since(id, now_ms - 86_400_000)
            .unwrap_or(0)
            >= ix.max_per_day
        {
            return Err(Blocked::Budget("jour"));
        }
        // Cadence : jamais deux requêtes plus proches que le délai de l'indexeur.
        let forced = self.xseed_delay_ms.load(Ordering::Relaxed);
        let min = if forced == u64::MAX {
            ix.min_interval_ms
        } else {
            forced
        };
        if let Ok(Some(last)) = self.store.indexer_last_request_ms(id) {
            let now = now_ms_precise();
            let wait = last + i64::try_from(min).unwrap_or(i64::MAX) - now;
            if wait > 0 {
                tokio::time::sleep(Duration::from_millis(u64::try_from(wait).unwrap_or(0))).await;
            }
        }
        let _ = self.store.indexer_record_request(id, now_ms_precise());
        Ok(())
    }

    /// Exécute une requête à un indexeur à travers le garde, puis réagit à l'issue : succès (remet les échecs à zéro) ou
    /// erreur (pause, désactivation automatique…). Voir `indexer_react`.
    async fn indexer_call<T>(
        &self,
        ix: &IndexerRow,
        fut: impl std::future::Future<Output = Result<T, TorznabError>>,
    ) -> Result<T, CallStop> {
        self.indexer_gate(&ix.id).await.map_err(|b| {
            tracing::debug!(indexer = %ix.name, reason = %b.describe(), "requête à l'indexeur non émise");
            CallStop::Blocked
        })?;
        match fut.await {
            Ok(v) => {
                let _ = self.store.indexer_success(&ix.id);
                Ok(v)
            }
            Err(e) => {
                self.indexer_react(ix, &e);
                Err(CallStop::Failed(e))
            }
        }
    }

    /// Réaction à une erreur d'indexeur, pensée pour ne JAMAIS faire bannir le compte de l'utilisateur :
    /// - accès refusé (401/403, codes Torznab 100-199) ou adresse invalide : **désactivation automatique** (on n'insiste
    ///   pas avec une clé refusée) ; l'admin doit la rétablir à la main ;
    /// - 429 / 503 + `Retry-After` : pause d'au moins ce que l'indexeur demande (au moins 1 min, au plus 24 h) ;
    /// - quota atteint (Torznab 500/501/910) : pause de 12 h ;
    /// - panne, délai dépassé, erreur serveur : attente croissante (1 min, 5 min, 15 min, 1 h, 6 h) ;
    /// - une réponse inattendue propre à UN résultat (lien mort) n'a pas de conséquence pour l'indexeur.
    fn indexer_react(&self, ix: &IndexerRow, e: &TorznabError) {
        let now = unix_now();
        // Dernier état connu : le compteur d'échecs a pu évoluer pendant ce passage.
        let fails = self
            .store
            .indexers()
            .unwrap_or_default()
            .into_iter()
            .find(|i| i.id == ix.id)
            .map_or(ix.fail_count, |i| i.fail_count);
        let backoff = BACKOFF_SECS[(fails as usize).min(BACKOFF_SECS.len() - 1)];
        let (disable, pause): (Option<String>, i64) = match e {
            TorznabError::Auth(m) => (
                Some(format!(
                    "{m} : désactivé automatiquement pour ne pas risquer le bannissement du compte"
                )),
                0,
            ),
            TorznabError::BadUrl(m) => (
                Some(format!("adresse refusée ({m}) : désactivé automatiquement")),
                0,
            ),
            TorznabError::RateLimited(ra) => (
                None,
                i64::try_from(
                    ra.unwrap_or(RATE_LIMIT_DEFAULT_SECS)
                        .clamp(RATE_LIMIT_MIN_SECS, RATE_LIMIT_MAX_SECS),
                )
                .unwrap_or(300)
                .max(backoff),
            ),
            TorznabError::Quota(_) => (None, QUOTA_PAUSE_SECS),
            TorznabError::Unreachable | TorznabError::Transient(_) => (None, backoff),
            // Réponse illisible ou erreur propre à la requête : pas de pénalité pour l'indexeur.
            TorznabError::BadResponse(_) | TorznabError::Remote(_) => return,
        };
        let msg = e.to_string();
        if let Some(reason) = disable {
            let _ = self.store.indexer_auto_disable(&ix.id, &reason);
            self.store.audit(
                "system",
                "xseed_indexer_disabled",
                &format!("{}|{reason}", ix.name),
            );
        } else {
            let _ = self.store.indexer_failure(&ix.id, &msg, now + pause);
            self.store.audit(
                "system",
                "xseed_indexer_paused",
                &format!("{}|{}|{msg}", ix.name, now + pause),
            );
        }
        tracing::warn!(indexer = %ix.name, error = %msg, "indexeur du cross-seed ralenti ou désactivé");
        self.bump_live();
    }

    async fn xseed_add(
        self: &Arc<Self>,
        parent: &TorrentDef,
        node_id: &str,
        bytes: &[u8],
        cand: &Metainfo,
        linked: bool,
    ) -> Result<(), ManageError> {
        let _g = self.reconcile_lock.lock().await;
        if self.store.get_torrent(&cand.infohash)?.is_some() {
            return Ok(());
        }
        let def = TorrentDef {
            infohash: cand.infohash.clone(),
            magnet: format!("magnet:?xt=urn:btih:{}", cand.infohash),
            // Posé directement sur les fichiers de l'original (même dossier) seulement si les données sont PROUVÉES
            // identiques ; sinon dans un dossier ISOLÉ de liens (voir `metainfo::needs_links`).
            save_subdir: if linked {
                link_dir(&parent.save_subdir, &cand.infohash)
            } else {
                parent.save_subdir.clone()
            },
            requires: Vec::new(),
            replicate_group: None,
            size_hint: cand.size,
            paused: false,
            completed: false,
            deleted: false,
            name: cand.name.clone(),
            has_file: true,
        };
        self.store.insert_torrent(&def, Some(bytes))?;
        self.store.set_xseed_of(&cand.infohash, &parent.infohash)?;
        let phase = if linked {
            Phase::Linking
        } else {
            Phase::Adding
        };
        if linked {
            self.store.set_xseed_linked(&cand.infohash)?;
        }
        self.store
            .insert_assignment(&cand.infohash, node_id, Role::Xseed, phase.as_str())?;
        self.store.bump_rev(node_id)?;
        self.store.audit(
            "system",
            "xseed_added",
            &format!("{} {} {node_id}", cand.infohash, parent.infohash),
        );
        self.reconcile_locked(node_id).await;
        Ok(())
    }

    /// Suivi des torrents ajoutés : en partage -> « seeding » ; vérification terminée mais contenu incomplet ->
    /// retiré (fichiers conservés) : le cross-seed ne télécharge jamais de données.
    pub async fn xseed_follow_up(self: &Arc<Self>) {
        let now = unix_now();
        for row in self.store.xseed_log(500).unwrap_or_default() {
            if row.state != "adding" {
                continue;
            }
            let Some(a) = self
                .store
                .assignments_for_torrent(&row.candidate)
                .unwrap_or_default()
                .into_iter()
                .find(|a| a.node_id == row.node_id)
            else {
                continue;
            };
            if a.removed {
                continue;
            }
            // Liens demandés mais jamais confirmés par l'agent (hors ligne, commande perdue) : on renonce.
            if a.phase == Phase::Linking.as_str() && now - row.ts >= LINK_TIMEOUT_SECS {
                self.xseed_discard(
                    &row.candidate,
                    &row.node_id,
                    "l'agent n'a pas confirmé la création des liens dans le délai",
                )
                .await;
                continue;
            }
            if a.phase == Phase::Active.as_str() {
                let _ = self.store.xseed_set_state(
                    &row.parent,
                    &row.node_id,
                    &row.candidate,
                    "seeding",
                    "fichiers vérifiés, en partage",
                );
                self.store.audit(
                    "system",
                    "xseed_seeding",
                    &format!("{} {}", row.candidate, row.node_id),
                );
                self.bump_live();
                continue;
            }
            let settle =
                i64::try_from(self.xseed_settle_secs.load(Ordering::Relaxed)).unwrap_or(i64::MAX);
            if a.phase != Phase::Rechecking.as_str() || now - row.ts < settle {
                continue;
            }
            let tel = self.telemetry(&row.node_id);
            let Some(o) = tel.torrents.iter().find(|o| o.infohash == row.candidate) else {
                continue;
            };
            let checking = o.status == proto::pb::TorrentStatus::Checking as i32
                || o.status == proto::pb::TorrentStatus::Queued as i32;
            if checking || o.progress >= DONE {
                continue;
            }
            let pct = (o.progress.clamp(0.0, 1.0) * 100.0).floor();
            let _g = self.reconcile_lock.lock().await;
            // Un candidat sur des liens physiques est retiré AVEC ses liens (les fichiers d'origine restent) ; un
            // candidat posé directement sur les fichiers de l'original ne les supprime jamais.
            let linked = self
                .store
                .xseed_linked_set()
                .is_ok_and(|s| s.contains(&row.candidate));
            let _ = self
                .store
                .mark_removed(&row.candidate, &row.node_id, linked);
            let _ = self
                .store
                .set_torrent_flag(&row.candidate, TorrentFlag::Deleted, true);
            let _ = self.store.bump_rev(&row.node_id);
            let _ = self.store.xseed_set_state(
                &row.parent,
                &row.node_id,
                &row.candidate,
                "removed",
                &format!(
                    "contenu incomplet après vérification ({pct} %) : retiré, fichiers conservés"
                ),
            );
            self.store.audit(
                "system",
                "xseed_removed",
                &format!("{} {}", row.candidate, row.node_id),
            );
            self.reconcile_locked(&row.node_id).await;
            self.bump_live();
        }
    }

    /// Construit la commande `LinkTree` d'un candidat : fichiers de l'original -> disposition du candidat, dans le même
    /// dossier d'enregistrement. `Err(raison)` si ce n'est plus possible (fichiers de torrent manquants, disposition…).
    pub(crate) fn build_link_cmd(
        &self,
        cand_ih: &str,
    ) -> Result<proto::pb::LinkTreeCmd, &'static str> {
        let parent_ih = self
            .store
            .xseed_parent(cand_ih)
            .ok()
            .flatten()
            .ok_or("torrent d'origine inconnu")?;
        let parent_def = self
            .store
            .get_torrent(&parent_ih)
            .ok()
            .flatten()
            .ok_or("torrent d'origine inconnu")?;
        let load = |ih: &str| {
            self.store
                .torrent_file(ih)
                .ok()
                .flatten()
                .and_then(|b| metainfo::parse(&b).ok())
        };
        let (parent, cand) = (
            load(&parent_ih).ok_or(".torrent de l'original illisible")?,
            load(cand_ih).ok_or(".torrent du candidat illisible")?,
        );
        let cand_def = self
            .store
            .get_torrent(cand_ih)
            .ok()
            .flatten()
            .ok_or("candidat inconnu")?;
        let pairs = metainfo::plan_links(&parent, &cand).ok_or("disposition non liable")?;
        // Mono-fichier : le « dossier racine » n'existe pas, le fichier est directement dans le dossier d'enregistrement.
        let root = |m: &Metainfo| {
            if m.single_file {
                String::new()
            } else {
                m.name.clone()
            }
        };
        Ok(proto::pb::LinkTreeCmd {
            source_dir: parent_def.save_subdir.clone(),
            source_name: root(&parent),
            // Le dossier isolé choisi à l'ajout du candidat (`link_dir`), c'est aussi son dossier d'enregistrement.
            dest_dir: cand_def.save_subdir,
            dest_name: root(&cand),
            pairs: pairs
                .into_iter()
                .map(|p| proto::pb::LinkPair {
                    src: p.src,
                    dst: p.dst,
                })
                .collect(),
        })
    }

    /// Renonce à un candidat (liens impossibles ou jamais confirmés) : opérations de base de données seulement, sans
    /// verrou (appelable pendant une réconciliation). Le candidat n'a jamais été ajouté au client ; l'agent annule
    /// lui-même ses liens en cas d'échec, il n'y a donc rien à nettoyer côté fichiers.
    pub(crate) fn xseed_discard_state(&self, cand_ih: &str, node_id: &str, reason: &str) {
        let parent = self
            .store
            .xseed_parent(cand_ih)
            .ok()
            .flatten()
            .unwrap_or_default();
        let _ = self.store.mark_removed(cand_ih, node_id, false);
        let _ = self
            .store
            .set_torrent_flag(cand_ih, TorrentFlag::Deleted, true);
        let _ = self.store.bump_rev(node_id);
        let _ = self
            .store
            .xseed_set_state(&parent, node_id, cand_ih, "refused", reason);
        self.store.audit(
            "system",
            "xseed_link_failed",
            &format!("{cand_ih} {node_id}"),
        );
    }

    /// Variante qui prend le verrou de réconciliation puis réconcilie le nœud.
    pub async fn xseed_discard(self: &Arc<Self>, cand_ih: &str, node_id: &str, reason: &str) {
        let _g = self.reconcile_lock.lock().await;
        self.xseed_discard_state(cand_ih, node_id, reason);
        self.reconcile_locked(node_id).await;
        self.bump_live();
    }

    /// Réponse de l'agent à une commande `LinkTree` (`cmd_id` : `r<rev>-lnk-<8 premiers caractères de l'infohash>`).
    pub async fn on_link_ack(self: &Arc<Self>, node_id: &str, ack: &proto::pb::Ack) {
        let Some((_, prefix)) = ack.cmd_id.split_once("-lnk-") else {
            return;
        };
        let Some(cand) = self
            .store
            .all_assignments()
            .unwrap_or_default()
            .into_iter()
            .find(|a| {
                a.node_id == node_id
                    && !a.removed
                    && a.phase == Phase::Linking.as_str()
                    && a.infohash.starts_with(prefix)
            })
            .map(|a| a.infohash)
        else {
            return; // réponse tardive ou en double : déjà traitée
        };
        if ack.ok {
            let _g = self.reconcile_lock.lock().await;
            let _ = self.store.set_phase(&cand, node_id, Phase::Adding.as_str());
            let _ = self.store.bump_rev(node_id);
            self.store
                .audit("system", "xseed_linked", &format!("{cand} {node_id}"));
            // Les liens existent : le torrent peut être ajouté (en pause) puis vérifié.
            self.reconcile_locked(node_id).await;
        } else {
            let why: String = ack.error.chars().take(160).collect();
            self.xseed_discard(&cand, node_id, &format!("liens impossibles : {why}"))
                .await;
        }
    }

    /// Torrents cross-seedés rattachés à ce torrent d'origine.
    pub fn xseed_members(&self, parent: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .store
            .xseed_children()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, p)| p == parent)
            .map(|(c, _)| c)
            .collect();
        v.sort();
        v
    }

    /// Pause/reprise d'une ligne : s'applique à tout le groupe (les membres d'abord, au mieux).
    pub async fn group_set_paused_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
        paused: bool,
    ) -> Result<(), ManageError> {
        for m in self.xseed_members(ih) {
            let _ = self.set_torrent_paused_as(actor, &m, paused).await;
        }
        self.set_torrent_paused_as(actor, ih, paused).await
    }

    pub async fn group_recheck_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
    ) -> Result<usize, ManageError> {
        for m in self.xseed_members(ih) {
            let _ = self.recheck_torrent_as(actor, &m).await;
        }
        self.recheck_torrent_as(actor, ih).await
    }

    /// Suppression d'une ligne : les torrents cross-seedés partent AVEC elle, TOUJOURS sans supprimer de fichiers
    /// (ce sont les mêmes que ceux de l'original ; seule la suppression de l'original, si demandée, les efface).
    pub async fn group_remove_as(
        self: &Arc<Self>,
        actor: &str,
        ih: &str,
        delete_files: bool,
    ) -> Result<(), ManageError> {
        // Un torrent sur des LIENS PHYSIQUES part toujours avec ses liens (les fichiers d'origine restent tant que
        // l'original existe). Un torrent posé directement sur les fichiers de l'original suit le choix de l'utilisateur.
        let linked = self.store.xseed_linked_set().unwrap_or_default();
        for m in self.xseed_members(ih) {
            let df = linked.contains(&m) || delete_files;
            let _ = self.remove_torrent_as(actor, &m, df).await;
        }
        let df = linked.contains(ih) || delete_files;
        self.remove_torrent_as(actor, ih, df).await
    }

    /// Infohash des torrents ajoutés par le cross-seed (pour l'affichage groupé).
    pub fn xseed_children_set(&self) -> HashSet<String> {
        self.store
            .xseed_children()
            .map(|m| m.into_keys().collect())
            .unwrap_or_default()
    }
}

fn tracker_host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_text_drops_separators_and_the_release_group() {
        assert_eq!(search_query("Film.2024.1080p-GRP"), "Film 2024 1080p");
        assert_eq!(search_query("Ma_Série S01E02 720p"), "Ma Série S01E02 720p");
        // Un tiret suivi de plusieurs mots n'est pas un groupe de release.
        assert_eq!(
            search_query("Spider-Man Homecoming"),
            "Spider-Man Homecoming"
        );
        assert_eq!(search_query("  "), "");
        assert_eq!(search_query("a-b"), "a");
    }

    #[test]
    fn modes_round_trip_and_default_to_simulation() {
        for m in [XseedMode::Off, XseedMode::Dry, XseedMode::Active] {
            assert_eq!(XseedMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(XseedMode::parse("autre"), None);
    }

    fn core_with_indexer() -> (Arc<Core>, IndexerRow, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(
            crate::ca::Ca::load_or_create(dir.path()).unwrap(),
            crate::store::Store::open_in_memory().unwrap(),
        );
        core.store
            .indexer_add("i1", "Idx", "http://idx.test", "k")
            .unwrap();
        let ix = core.store.indexers().unwrap().remove(0);
        (core, ix, dir)
    }

    fn row(core: &Core) -> IndexerRow {
        core.store.indexers().unwrap().remove(0)
    }

    #[test]
    fn consecutive_failures_back_off_1m_5m_15m_1h_then_6h_and_a_success_resets() {
        let (core, ix, _d) = core_with_indexer();
        let mut seen = Vec::new();
        for _ in 0..7 {
            core.indexer_react(&ix, &TorznabError::Unreachable);
            let r = row(&core);
            seen.push(r.paused_until - unix_now());
        }
        let near = |got: i64, want: i64| (got - want).abs() <= 3;
        for (got, want) in seen
            .iter()
            .zip([60, 300, 900, 3600, 21_600, 21_600, 21_600])
        {
            assert!(
                near(*got, want),
                "attente {got} s, {want} s attendues : {seen:?}"
            );
        }
        assert_eq!(row(&core).fail_count, 7);
        core.store.indexer_success("i1").unwrap();
        let r = row(&core);
        assert_eq!((r.fail_count, r.paused_until), (0, 0));
    }

    #[test]
    fn a_dead_link_or_unreadable_answer_never_penalises_the_indexer() {
        let (core, ix, _d) = core_with_indexer();
        for e in [
            TorznabError::BadResponse("statut HTTP inattendu"),
            TorznabError::Remote("item introuvable".into()),
        ] {
            core.indexer_react(&ix, &e);
        }
        let r = row(&core);
        assert!(r.enabled && r.fail_count == 0 && r.paused_until == 0 && r.last_error.is_empty());
    }

    #[test]
    fn access_errors_disable_and_never_retry_by_themselves() {
        for e in [
            TorznabError::Auth("HTTP 403".into()),
            TorznabError::BadUrl("hôte absent"),
        ] {
            let (core, ix, _d) = core_with_indexer();
            core.indexer_react(&ix, &e);
            let r = row(&core);
            assert!(
                !r.enabled && r.disabled_reason.contains("désactivé automatiquement"),
                "{e:?}"
            );
            let log = core
                .store
                .audit_list(5, None, Some("xseed_indexer_disabled"))
                .unwrap();
            assert_eq!(log.len(), 1);
            // Le journal ne contient que le nom et la raison : jamais l'adresse de l'indexeur.
            assert!(log[0].detail.starts_with("Idx|") && !log[0].detail.contains("http"));
        }
    }
}
