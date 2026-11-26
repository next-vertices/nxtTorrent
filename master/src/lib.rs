//! Master : CA intégrée, appairage, tunnel, (phase 4) scheduler, API admin.
#![forbid(unsafe_code)]

pub mod admin;
pub mod agentdist;
pub mod alerts;
pub mod backup;
pub mod ca;
pub mod crossseed;
pub mod health;
pub mod limiter;
pub mod manage;
pub mod notify;
pub mod overview;
pub mod reconcile;
pub mod scheduler;
pub mod selfupdate;
pub mod service;
pub mod state;
pub mod stats;
pub mod store;
pub mod tlsaccept;
pub mod torrentjobs;
pub mod torznab;
pub mod tracker;
pub mod traffic;
pub mod tunnel;
pub mod updates;
pub mod webhook;

use ca::Ca;
use limiter::FailureLimiter;
use notify::{LogNotifier, Notification, Notifier, PresenceKind};
use proto::pairing::{PairingCode, SECRET_LEN};
use proto::pb::{master_msg::M as MasterM, Ack, Command, Event, TorrentInfo};
use rand::RngCore;
use state::{NodeState, Trigger};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::Store;

pub const PAIRING_CODE_TTL: Duration = Duration::from_secs(15 * 60);

/// Paramètres de fonctionnement (défauts de production ; surchargés par les tests).
#[derive(Clone, Debug)]
pub struct Settings {
    /// Silence maximal toléré sur le tunnel (≈ 3 heartbeats de 10 s).
    pub hb_timeout: Duration,
    /// Anti-flapping : délai avant de notifier une transition de présence.
    pub confirm_delay: Duration,
    /// Délai de grâce avant qu'un nœud hors ligne soit réassignable.
    pub offline_grace: Duration,
    /// Durée de vie des certificats de nœud.
    pub cert_validity: Duration,
    /// Espace disque à laisser libre en plus de la taille du torrent.
    pub disk_reserve: u64,
    /// Période du balayage de l'ordonnanceur (réassignation, réplicas manquants).
    pub sweep_interval: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hb_timeout: Duration::from_secs(30),
            confirm_delay: Duration::from_secs(60),
            offline_grace: Duration::from_secs(15 * 60),
            cert_validity: Duration::from_secs(14 * 24 * 3600),
            disk_reserve: 1024 * 1024 * 1024,
            sweep_interval: Duration::from_secs(30),
        }
    }
}

const MAX_KEPT: usize = 1000;

/// Dernier état connu d'un nœud (le master reste la source de vérité de l'état DÉSIRÉ ;
/// ceci n'est que l'état OBSERVÉ, remonté par l'agent).
#[derive(Default, Clone, Debug)]
pub struct NodeTelemetry {
    pub torrents: Vec<TorrentInfo>,
    pub disk_free: u64,
    pub applied_rev: u64,
    pub dl_rate: u64,
    pub ul_rate: u64,
    pub heartbeats: u64,
    pub full_states: u64,
    pub client_kind: String,
    pub client_version: String,
    /// Plateforme annoncée (une de `proto::PLATFORMS`, sinon vide) et capacité à se mettre à jour.
    pub platform: String,
    pub update_capable: bool,
    /// Dernière progression de mise à jour rapportée par le nœud.
    pub update: Option<proto::pb::UpdateStatus>,
    /// Mise à jour demandée : jusqu'à cet instant, une absence du nœud est attendue (ni alerte ni notification).
    pub updating_until: i64,
    pub events: Vec<Event>,
    pub acks: Vec<Ack>,
    /// Santé réseau du client (dernier heartbeat).
    pub health: proto::pb::NodeHealth,
}

/// Ce que l'API expose d'un nœud côté télémétrie (voir `Core::node_digests`).
#[derive(Clone, Debug)]
pub struct NodeDigest {
    pub applied_rev: u64,
    pub client_kind: String,
    pub client_version: String,
    pub disk_free: u64,
    pub torrent_count: usize,
    pub dl_rate: u64,
    pub ul_rate: u64,
    pub network: health::Assessment,
}

impl NodeDigest {
    /// Nœud sans télémétrie (jamais connecté depuis le démarrage du master).
    pub fn empty() -> Self {
        Self {
            applied_rev: 0,
            client_kind: String::new(),
            client_version: String::new(),
            disk_free: 0,
            torrent_count: 0,
            dl_rate: 0,
            ul_rate: 0,
            network: health::assess(false, proto::pb::NodeHealth::default(), &[]),
        }
    }
}

impl NodeTelemetry {
    /// Dédoublonnage par `event_id` (les agents rejouent leurs événements après une coupure).
    pub fn push_event(&mut self, e: Event) {
        if self.events.iter().any(|x| x.event_id == e.event_id) {
            return;
        }
        if self.events.len() >= MAX_KEPT {
            self.events.remove(0);
        }
        self.events.push(e);
    }

    pub fn push_ack(&mut self, a: Ack) {
        if self.acks.len() >= MAX_KEPT {
            self.acks.remove(0);
        }
        self.acks.push(a);
    }
}

#[derive(Default)]
struct Presence {
    epoch: u64,
    notified: Option<NodeState>,
}

/// État partagé du master.
/// Réplication entre nœuds par le protocole BitTorrent : tracker privé hébergé par le master et son adresse publique.
pub struct Swarm {
    pub tracker: Arc<tracker::Tracker>,
    /// `http://hôte[:port]` sous lequel les NŒUDS joignent le tracker (sans « / » final).
    pub public_base: String,
}

pub struct Core {
    pub ca: Ca,
    pub store: Store,
    pub pair_limiter: FailureLimiter,
    /// Au plus une entrée d'audit « mtls_failed » par IP et par minute (un scanner ne remplit pas le journal).
    pub(crate) mtls_limiter: FailureLimiter,
    pub settings: Settings,
    /// Adresse des releases (constante en production ; un faux serveur dans les tests : cf. `updates`).
    update_source: Mutex<String>,
    selfupdate: std::sync::OnceLock<selfupdate::SelfUpdate>,
    agentdist: std::sync::OnceLock<agentdist::AgentDist>,
    pub sessions: tunnel::Sessions,
    pub(crate) reconcile_lock: tokio::sync::Mutex<()>,
    notifiers: Vec<Arc<dyn Notifier>>,
    presence: Mutex<HashMap<String, Presence>>,
    telemetry: Mutex<HashMap<String, NodeTelemetry>>,
    /// Relevés de référence des compteurs de trafic (chargés de la base au premier échantillon). Ordre des verrous :
    /// toujours ce registre AVANT la télémétrie.
    pub(crate) traffic_book: Mutex<Option<traffic::Book>>,
    /// Compteur de révision de l'état visible (télémétrie, présence, actions admin) : l'UI en direct
    /// s'abonne pour savoir quand rafraîchir, sans interroger en boucle.
    live: tokio::sync::watch::Sender<u64>,
    /// Période d'échantillonnage de l'historique (secondes ; 60 par défaut).
    metrics_every: std::sync::atomic::AtomicU64,
    /// État des alertes (depuis quand une condition dure, ce qui a déjà été notifié) : en mémoire.
    alert_state: Mutex<alerts::AlertState>,
    /// Réplication par swarm : absente tant que le tracker n'est pas activé (stockage partagé supposé, comme avant).
    swarm: std::sync::OnceLock<Swarm>,
    /// (torrent, nœud) pour lesquels le .torrent a déjà été demandé depuis le démarrage : une seule demande, et
    /// une réponse non sollicitée est ignorée.
    pub(crate) metainfo_asked: Mutex<std::collections::HashSet<(String, String)>>,
    /// Préfixes d'infohash dont la demande de .torrent a échoué (client sans export) : on n'attend plus.
    metainfo_failed: Mutex<std::collections::HashSet<String>>,
    /// Cross-seed : délai entre deux requêtes à un indexeur (ms) et période de re-recherche (s).
    pub(crate) xseed_delay_ms: std::sync::atomic::AtomicU64,
    pub(crate) xseed_cooldown_secs: std::sync::atomic::AtomicU64,
    pub(crate) xseed_settle_secs: std::sync::atomic::AtomicU64,
    pub(crate) xseed_running: std::sync::atomic::AtomicBool,
    /// Prochain instant (ms Unix) où un « test d'indexeur » est permis : chaque test avec une mauvaise clé est une
    /// tentative d'accès que l'indexeur peut sanctionner.
    pub(crate) xseed_test_next_ms: std::sync::atomic::AtomicI64,
    /// Création de .torrent sur les nœuds (tâches en mémoire) et listage de leurs dossiers.
    pub(crate) torrent_jobs: torrentjobs::Jobs,
}

impl Core {
    /// Active la réplication par swarm : crée le tracker privé (secret dérivé de la clé du master) et retient l'adresse
    /// publique à écrire dans les .torrent des copies. Sans effet si déjà activée.
    pub fn enable_swarm(&self, public_base: &str) -> anyhow::Result<Arc<tracker::Tracker>> {
        let base = public_base.trim_end_matches('/');
        anyhow::ensure!(
            base.starts_with("http://")
                && base.len() <= 200
                && base.bytes().all(|b| (0x21..=0x7e).contains(&b))
                && !base["http://".len()..].contains(['/', '@', '?', '#']),
            "adresse du tracker invalide (attendu http://hôte[:port])"
        );
        let t = tracker::Tracker::new(&self.store.derived_key("tracker"));
        let _ = self.swarm.set(Swarm {
            tracker: t.clone(),
            public_base: base.to_owned(),
        });
        Ok(t)
    }

    /// Note que le .torrent de ce torrent a été demandé à ce nœud (la réponse sera acceptée ; pas de seconde demande).
    pub fn note_metainfo_asked(&self, ih: &str, node_id: &str) {
        self.metainfo_asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((ih.to_owned(), node_id.to_owned()));
    }

    /// La demande de .torrent a échoué (`cmd_id` de la forme `r<rev>-fmi-<8 premiers caractères de l'infohash>`).
    pub fn note_metainfo_failure(&self, cmd_id: &str) {
        if let Some((_, prefix)) = cmd_id.split_once("-fmi-") {
            self.metainfo_failed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(prefix.to_owned());
        }
    }

    pub(crate) fn metainfo_failed(&self, ih: &str) -> bool {
        self.metainfo_failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|p| ih.starts_with(p.as_str()))
    }

    /// Plafond de débit (octets/s) appliqué au téléchargement d'une copie entre nœuds ; 0 = illimité.
    pub fn copy_dl_limit(&self) -> u64 {
        self.store
            .setting_get("copy_dl_limit")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    pub fn swarm(&self) -> Option<&Swarm> {
        self.swarm.get()
    }

    pub fn open(data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let ca = Ca::load_or_create(data_dir)?;
        let store = Store::open(&data_dir.join("master.db"))?;
        Ok(Self::new(ca, store))
    }

    pub fn new(ca: Ca, store: Store) -> Arc<Self> {
        Self::with_settings(ca, store, Settings::default(), vec![Arc::new(LogNotifier)])
    }

    pub fn with_settings(
        ca: Ca,
        store: Store,
        settings: Settings,
        notifiers: Vec<Arc<dyn Notifier>>,
    ) -> Arc<Self> {
        match store.reset_presence(store::unix_now()) {
            Ok(n) if n > 0 => tracing::info!(nodes = n, "présence réinitialisée au démarrage"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "réinitialisation de la présence impossible"),
        }
        Arc::new(Self {
            ca,
            store,
            pair_limiter: FailureLimiter::new(5, Duration::from_secs(15 * 60)),
            mtls_limiter: FailureLimiter::new(1, Duration::from_secs(60)),
            settings,
            update_source: Mutex::new(updates::RELEASES_URL.to_owned()),
            selfupdate: std::sync::OnceLock::new(),
            agentdist: std::sync::OnceLock::new(),
            sessions: tunnel::Sessions::default(),
            reconcile_lock: tokio::sync::Mutex::new(()),
            notifiers,
            presence: Mutex::new(HashMap::new()),
            telemetry: Mutex::new(HashMap::new()),
            traffic_book: Mutex::new(None),
            live: tokio::sync::watch::channel(0).0,
            metrics_every: std::sync::atomic::AtomicU64::new(60),
            alert_state: Mutex::new(alerts::AlertState::default()),
            swarm: std::sync::OnceLock::new(),
            metainfo_asked: Mutex::new(std::collections::HashSet::new()),
            metainfo_failed: Mutex::new(std::collections::HashSet::new()),
            xseed_delay_ms: std::sync::atomic::AtomicU64::new(u64::MAX),
            xseed_cooldown_secs: std::sync::atomic::AtomicU64::new(24 * 3600),
            xseed_settle_secs: std::sync::atomic::AtomicU64::new(120),
            xseed_running: std::sync::atomic::AtomicBool::new(false),
            xseed_test_next_ms: std::sync::atomic::AtomicI64::new(0),
            torrent_jobs: torrentjobs::Jobs::default(),
        })
    }

    pub fn record(&self, node_id: &str, f: impl FnOnce(&mut NodeTelemetry)) {
        let mut m = self
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(m.entry(node_id.to_owned()).or_default());
        drop(m);
        self.bump_live();
    }

    /// Règles d'alerte courantes (celles de la base, sinon les valeurs par défaut).
    pub fn alert_rules(&self) -> alerts::Rules {
        self.store
            .alert_rules_get()
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    /// Évalue les règles et notifie les canaux actifs. Appelée toutes les 15 s par la tâche de fond.
    pub async fn alert_tick(self: &Arc<Self>) {
        self.alert_tick_at(store::unix_now()).await;
    }

    /// Variante à horloge injectée (tests).
    pub async fn alert_tick_at(self: &Arc<Self>, now: i64) {
        let rules = self.alert_rules();
        let Ok(nodes) = self.store.node_details() else {
            return;
        };
        let digests = self.node_digests(|id| self.sessions.is_connected(id));
        let errors = if rules.torrent_errors.enabled {
            self.cluster_stats().map_or(0, |s| s.torrents_error)
        } else {
            0
        };
        let mut conds = alerts::conditions(&rules, &nodes, &digests, errors);
        // Un nœud qui redémarre pour se mettre à jour n’est pas « hors ligne » tant que sa fenêtre court.
        for c in &mut conds {
            if c.key
                .strip_prefix("node_offline:")
                .is_some_and(|id| self.is_updating(id))
            {
                c.active = false;
            }
        }
        let events = self
            .alert_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .step(&conds, now);
        if events.is_empty() {
            return;
        }
        let channels = self.store.alert_channels().unwrap_or_default();
        for e in events {
            self.store.audit(
                "system",
                if e.firing {
                    "alert_fired"
                } else {
                    "alert_resolved"
                },
                &format!("{}|{}", e.key, e.title),
            );
            self.bump_live();
            if !e.firing && !rules.notify_resolved {
                continue;
            }
            for ch in channels.iter().filter(|c| c.enabled) {
                let Some(kind) = alerts::ChannelKind::parse(&ch.kind) else {
                    continue;
                };
                let (url, ev, id) = (ch.url.clone(), e.clone(), ch.id.clone());
                tokio::spawn(async move {
                    // L'URL est un secret : jamais journalisée, seule l'identité du canal l'est.
                    if let Err(err) = alerts::send(kind, &url, &ev).await {
                        tracing::warn!(channel = %id, error = %err, "échec d'envoi d'une alerte");
                    }
                });
            }
        }
    }

    /// Change la période d'échantillonnage de l'historique (réglage avancé, utile aux démonstrations et aux tests).
    pub fn set_metrics_interval(&self, d: Duration) {
        self.metrics_every
            .store(d.as_secs().max(1), std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn metrics_interval(&self) -> Duration {
        Duration::from_secs(
            self.metrics_every
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Signale à l'UI en direct que l'état visible a changé.
    pub fn bump_live(&self) {
        self.live.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub fn subscribe_live(&self) -> tokio::sync::watch::Receiver<u64> {
        self.live.subscribe()
    }

    /// Résumé léger de chaque nœud, calculé en UN passage sous le verrou, sans copier les listes de torrents
    /// (avec 100 nœuds de milliers de torrents, cloner chaque seconde serait prohibitif).
    pub fn node_digests(&self, online: impl Fn(&str) -> bool) -> HashMap<String, NodeDigest> {
        let mut out = HashMap::new();
        self.each_telemetry(|id, t| {
            out.insert(
                id.to_owned(),
                NodeDigest {
                    applied_rev: t.applied_rev,
                    client_kind: t.client_kind.clone(),
                    client_version: t.client_version.clone(),
                    disk_free: t.disk_free,
                    torrent_count: t.torrents.len(),
                    dl_rate: t.dl_rate,
                    ul_rate: t.ul_rate,
                    network: health::assess(online(id), t.health, &t.torrents),
                },
            );
        });
        out
    }

    /// Parcourt la télémétrie sans la cloner (la liste de torrents d'un nœud peut compter des milliers d'entrées).
    pub(crate) fn each_telemetry(&self, mut f: impl FnMut(&str, &NodeTelemetry)) {
        let m = self
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (id, t) in m.iter() {
            f(id, t);
        }
    }

    pub(crate) fn telemetry_torrent_count(&self, node_id: &str) -> usize {
        let m = self
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.get(node_id).map_or(0, |t| t.torrents.len())
    }

    /// (débit descendant, débit montant, espace libre) d'un nœud, sans copier sa liste de torrents.
    pub(crate) fn telemetry_summary(&self, node_id: &str) -> (u64, u64, u64) {
        let m = self
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.get(node_id)
            .map_or((0, 0, 0), |t| (t.dl_rate, t.ul_rate, t.disk_free))
    }

    pub fn telemetry(&self, node_id: &str) -> NodeTelemetry {
        let m = self
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.get(node_id).cloned().unwrap_or_default()
    }

    /// Envoie une commande (jeu fermé) au nœud. `false` si le nœud n'a pas de tunnel actif.
    pub async fn send_command(&self, node_id: &str, cmd: Command) -> bool {
        self.store.audit("admin", "command_sent", node_id);
        self.sessions.send(node_id, MasterM::Cmd(cmd)).await
    }

    /// Génère un code à usage unique ; seul son condensat est stocké.
    pub fn create_pairing_code(&self, ttl: Duration) -> anyhow::Result<String> {
        self.create_pairing_code_with_labels(ttl, &[])
    }

    /// Code d'appairage dont les labels sont imposés par l'admin (le nœud ne les choisit pas).
    pub fn create_pairing_code_with_labels(
        &self,
        ttl: Duration,
        labels: &[String],
    ) -> anyhow::Result<String> {
        self.create_pairing_code_as("admin", ttl, labels)
    }

    /// Comme ci-dessus, en nommant l'auteur dans le journal d'audit (jamais le code lui-même).
    pub fn create_pairing_code_as(
        &self,
        actor: &str,
        ttl: Duration,
        labels: &[String],
    ) -> anyhow::Result<String> {
        self.create_pairing_code_named(actor, ttl, labels, None)
    }

    /// Code d'appairage portant en plus le nom d'affichage que recevra le nœud.
    pub fn create_pairing_code_named(
        &self,
        actor: &str,
        ttl: Duration,
        labels: &[String],
        name: Option<&str>,
    ) -> anyhow::Result<String> {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        anyhow::ensure!(name.is_none_or(valid_node_name), "nom invalide");
        anyhow::ensure!(
            labels.iter().all(|l| manage::valid_label(l)),
            "label invalide"
        );
        let mut secret = [0u8; SECRET_LEN];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let code = PairingCode::new(secret, self.ca.fingerprint());
        let ttl = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX / 2);
        self.store.insert_code(
            &code.secret_hash(),
            store::unix_now().saturating_add(ttl),
            labels,
            name,
        )?;
        self.store.audit(
            actor,
            "pairing_code_created",
            &format!("{}|{}", name.unwrap_or(""), labels.join(",")),
        );
        Ok(code.encode())
    }

    /// Applique une transition de la machine à états, persiste et planifie la notification.
    pub fn apply(self: &Arc<Self>, node_id: &str, t: Trigger) -> Option<NodeState> {
        let info = match self.store.node_info(node_id) {
            Ok(Some(i)) => i,
            _ => return None,
        };
        let cur = NodeState::parse(&info.state)?;
        let new = state::next(cur, t);
        let now = store::unix_now();
        if let Err(e) = self.store.set_state(node_id, new.as_str(), now) {
            tracing::error!(error = %e, "persistance de l'état impossible");
            return None;
        }
        self.bump_live();
        if t == Trigger::Connected {
            let _ = self.store.touch(node_id, now);
        }
        // Une connexion replanifie toujours la confirmation : cela établit la ligne de base
        // « Online » d'un nœud fraîchement appairé (déjà Online en base), sans notifier.
        if new != cur {
            self.store.audit(
                "system",
                "node_state",
                &format!("{node_id} {} -> {}", cur.as_str(), new.as_str()),
            );
        }
        if new != cur || t == Trigger::Connected {
            self.schedule_notification(node_id, new);
        }
        Some(new)
    }

    /// Révocation admin : l'état devient terminal et le tunnel est fermé.
    pub fn revoke_node(self: &Arc<Self>, node_id: &str) {
        self.revoke_node_as("admin", node_id);
    }

    pub fn revoke_node_as(self: &Arc<Self>, actor: &str, node_id: &str) {
        self.apply(node_id, Trigger::Revoked);
        self.store.audit(actor, "node_revoked", node_id);
        self.sessions.kill(node_id, "revoked");
    }

    fn schedule_notification(self: &Arc<Self>, node_id: &str, new: NodeState) {
        if !matches!(new, NodeState::Online | NodeState::Offline) {
            return;
        }
        let epoch = {
            let mut m = self
                .presence
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let p = m.entry(node_id.to_owned()).or_default();
            p.epoch += 1;
            p.epoch
        };
        let (core, id) = (self.clone(), node_id.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(core.settings.confirm_delay).await;
            core.confirm_presence(&id, epoch);
        });
    }

    fn confirm_presence(&self, node_id: &str, epoch: u64) {
        // Un nœud qui redémarre pour se mettre à jour n'est pas en panne. S'il ne revient pas, l'alerte « nœud hors
        // ligne » se déclenche à la fin de la fenêtre.
        if self.is_updating(node_id) {
            return;
        }
        let kind = {
            let mut m = self
                .presence
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(p) = m.get_mut(node_id) else { return };
            if p.epoch != epoch {
                return; // l'état a changé entre-temps : flapping, on ne notifie pas
            }
            let cur = match self.store.node_info(node_id) {
                Ok(Some(i)) => NodeState::parse(&i.state),
                _ => None,
            };
            let kind = match (p.notified, cur) {
                (Some(NodeState::Online), Some(NodeState::Offline)) => {
                    Some(PresenceKind::WentOffline)
                }
                (Some(NodeState::Offline), Some(NodeState::Online)) => {
                    Some(PresenceKind::CameOnline)
                }
                _ => None,
            };
            if matches!(cur, Some(NodeState::Online | NodeState::Offline)) {
                p.notified = cur;
            }
            kind
        };
        if let Some(kind) = kind {
            let n = Notification {
                node_id: node_id.to_owned(),
                kind,
            };
            for notifier in &self.notifiers {
                notifier.notify(&n);
            }
        }
    }
}

/// Nom d'affichage d'un nœud : 1 à 64 caractères, sans caractère de contrôle.
pub fn valid_node_name(n: &str) -> bool {
    !n.trim().is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control)
}
