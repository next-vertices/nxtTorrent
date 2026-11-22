//! Tunnel persistant vers le master (mTLS), heartbeat, reconnexion et renouvellement de certificat.
//! L'agent n'ouvre AUCUN port d'écoute : il initie toujours la connexion.
use crate::client::TorrentClient;
use crate::exec::Executor;
use crate::outbox::Outbox;
use crate::pair::identity_paths;
use crate::tls::connect_channel;
use crate::watcher::{self, Snapshot};
use proto::certinfo::parse_pem;
use proto::fsutil::write_secret_atomic;
use proto::pb::agent_msg::M as A;
use proto::pb::command::Kind;
use proto::pb::master_msg::M as M_;
use proto::pb::orchestrator_client::OrchestratorClient;
use proto::pb::{Ack, AgentMsg, CertRenewReq, FullState, Heartbeat, Hello};
use proto::PROTOCOL_VERSION;
use rand::Rng;
use rcgen::{CertificateParams, KeyPair};
use rustls::crypto::ring;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Clone, Debug)]
pub struct TunnelConfig {
    pub data_dir: PathBuf,
    pub hb_interval: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Fréquence de vérification de l'échéance du certificat.
    pub renew_check: Duration,
    /// Client torrent piloté (None = tunnel seul, sans torrents).
    pub client: Option<Arc<dyn TorrentClient>>,
    /// Racine locale de téléchargement : tous les chemins du master y sont relatifs.
    pub root: PathBuf,
    /// Fréquence d'interrogation du client pour détecter les événements.
    pub poll_interval: Duration,
    /// Quitter (code d'erreur) si le master reste injoignable aussi longtemps. Désactivé par défaut.
    /// Utile sous un superviseur (Docker, systemd) : un agent qui partage la pile réseau d'un autre
    /// conteneur la perd quand celui-ci est recréé, et seul un redémarrage le rattache à la nouvelle.
    pub exit_after_disconnected: Option<Duration>,
}

impl TunnelConfig {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            hb_interval: Duration::from_secs(10),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            renew_check: Duration::from_secs(3600),
            client: None,
            root: PathBuf::new(),
            poll_interval: Duration::from_secs(1),
            exit_after_disconnected: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunnelStatus {
    Connecting,
    Connected,
    Disconnected(String),
    /// Nœud révoqué / inconnu : l'agent s'arrête, inutile de réessayer.
    Revoked,
    Stopped,
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("stockage local illisible: {0}")]
    Storage(String),
    #[error("identité illisible ou absente (agent non appairé ?): {0}")]
    Identity(String),
    #[error("master injoignable depuis {0} s : arrêt pour que le superviseur relance l'agent (réseau périmé ?)")]
    Unreachable(u64),
}

struct Identity {
    key_pem: String,
    cert_pem: String,
    ca_pem: String,
    node_id: String,
    addr: String,
}

fn load_identity(dir: &Path) -> Result<Identity, TunnelError> {
    let (k, c, ca, id) = identity_paths(dir);
    let rd = |p: &Path| {
        std::fs::read_to_string(p)
            .map_err(|e| TunnelError::Identity(format!("{}: {e}", p.display())))
    };
    Ok(Identity {
        key_pem: rd(&k)?,
        cert_pem: rd(&c)?,
        ca_pem: rd(&ca)?,
        node_id: rd(&id)?.trim().to_owned(),
        addr: rd(&dir.join("master_addr"))?.trim().to_owned(),
    })
}

/// Configuration TLS normale : la CA épinglée lors de l'appairage est la seule ancre de confiance.
fn client_config(id: &Identity) -> Result<ClientConfig, String> {
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(id.ca_pem.as_bytes()) {
        roots
            .add(c.map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(id.cert_pem.as_bytes())
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
    let key = PrivateKeyDer::from_pem_slice(id.key_pem.as_bytes()).map_err(|e| e.to_string())?;
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|e| e.to_string())
}

/// Backoff exponentiel plafonné, avec jitter (`jitter` dans [0,1) → facteur 0,5..1).
pub fn backoff_delay(attempt: u32, base: Duration, max: Duration, jitter: f64) -> Duration {
    let exp = base.saturating_mul(2u32.saturating_pow(attempt.min(20)));
    exp.min(max).mul_f64(0.5 + 0.5 * jitter.clamp(0.0, 1.0))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

enum Outcome {
    Shutdown,
    Revoked,
    /// `connected` : le tunnel a été établi (remet le backoff à zéro).
    Retry {
        connected: bool,
        wait_max: bool,
    },
    Reconnect,
}

/// Boucle principale : se (re)connecte jusqu'à arrêt ou révocation.
pub async fn run(
    cfg: TunnelConfig,
    status: watch::Sender<TunnelStatus>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), TunnelError> {
    tokio::pin!(shutdown);
    let outbox = Outbox::open(cfg.data_dir.join("outbox.log"))
        .map_err(|e| TunnelError::Storage(e.to_string()))?;
    let applied_path = cfg.data_dir.join("applied_rev");
    let applied_rev = Arc::new(AtomicU64::new(
        std::fs::read_to_string(&applied_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0),
    ));
    // Surveillance du client torrent : vit aussi longtemps que l'agent, tunnel ouvert ou non.
    let (_watcher, snap_rx) = match &cfg.client {
        Some(c) => {
            let (h, rx) = watcher::spawn(c.clone(), outbox.clone(), cfg.poll_interval);
            (Some(AbortOnDrop(h)), Some(rx))
        }
        None => (None, None),
    };
    let shared = Shared {
        jobs: Arc::new(crate::jobs::JobTable::new()),
        outbox,
        applied_rev,
        snap_rx,
        applied_path,
    };
    let mut attempt = 0u32;
    let mut last_ok = tokio::time::Instant::now();
    loop {
        if let Some(limit) = cfg.exit_after_disconnected {
            if last_ok.elapsed() >= limit {
                let _ = status.send(TunnelStatus::Stopped);
                return Err(TunnelError::Unreachable(limit.as_secs()));
            }
        }
        let id = load_identity(&cfg.data_dir)?;
        let _ = status.send(TunnelStatus::Connecting);
        let outcome = tokio::select! {
            () = &mut shutdown => Outcome::Shutdown,
            o = session(&cfg, &id, &status, &shared) => o,
        };
        let delay = match outcome {
            Outcome::Shutdown => {
                let _ = status.send(TunnelStatus::Stopped);
                return Ok(());
            }
            Outcome::Revoked => {
                let _ = status.send(TunnelStatus::Revoked);
                return Ok(());
            }
            Outcome::Reconnect => {
                attempt = 0;
                Duration::ZERO
            }
            Outcome::Retry {
                connected,
                wait_max,
            } => {
                if connected {
                    last_ok = tokio::time::Instant::now(); // la dernière connexion valide date de maintenant
                }
                attempt = if connected {
                    0
                } else {
                    attempt.saturating_add(1)
                };
                if wait_max {
                    cfg.backoff_max
                } else {
                    let j: f64 = rand::thread_rng().gen();
                    backoff_delay(attempt, cfg.backoff_base, cfg.backoff_max, j)
                }
            }
        };
        tokio::select! {
            () = &mut shutdown => { let _ = status.send(TunnelStatus::Stopped); return Ok(()); }
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// État partagé entre toutes les sessions successives (survit aux coupures du tunnel).
struct Shared {
    /// Créations de torrent en cours (survivent aux coupures du tunnel, annulables).
    jobs: Arc<crate::jobs::JobTable>,
    outbox: Arc<Outbox>,
    applied_rev: Arc<AtomicU64>,
    snap_rx: Option<watch::Receiver<Arc<Snapshot>>>,
    applied_path: PathBuf,
}

/// Arrête la tâche de surveillance quand l'agent s'arrête.
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn wait_changed(rx: &mut Option<watch::Receiver<Arc<Snapshot>>>) {
    if let Some(r) = rx {
        if r.changed().await.is_ok() {
            return;
        }
    }
    std::future::pending::<()>().await;
}

fn send_msg(tx: &mpsc::Sender<AgentMsg>, m: A) -> bool {
    tx.try_send(AgentMsg { m: Some(m) }).is_ok()
}

fn full_state(snap: &Snapshot, applied: u64) -> A {
    A::State(FullState {
        torrents: snap.torrents.clone(),
        disk_free_bytes: snap.disk_free,
        applied_rev: applied,
    })
}

fn heartbeat(snap: &Snapshot, applied: u64) -> A {
    A::Hb(Heartbeat {
        applied_rev: applied,
        dl_rate: snap.torrents.iter().map(|t| t.dl_rate).sum(),
        ul_rate: snap.torrents.iter().map(|t| t.ul_rate).sum(),
        torrent_count: u32::try_from(snap.torrents.len()).unwrap_or(u32::MAX),
        disk_free: snap.disk_free,
        health: Some(snap.health),
    })
}

/// Hôte (sans port) de l'adresse `hôte:port` du master.
fn master_host(addr: &str) -> Option<String> {
    let (h, _) = addr.rsplit_once(':')?;
    Some(h.trim_matches(['[', ']']).to_owned()).filter(|h| !h.is_empty())
}

async fn session(
    cfg: &TunnelConfig,
    id: &Identity,
    status: &watch::Sender<TunnelStatus>,
    shared: &Shared,
) -> Outcome {
    let fail = |msg: String, connected: bool| {
        let _ = status.send(TunnelStatus::Disconnected(msg));
        Outcome::Retry {
            connected,
            wait_max: false,
        }
    };
    let tls = match client_config(id) {
        Ok(c) => c,
        Err(e) => return fail(format!("tls: {e}"), false),
    };
    // Délai borné : vers un réseau mort, un connect() TCP pourrait attendre plusieurs minutes.
    let chan =
        match tokio::time::timeout(Duration::from_secs(15), connect_channel(&id.addr, tls)).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return fail(format!("connexion: {e}"), false),
            Err(_) => return fail("connexion: délai dépassé".into(), false),
        };
    // Canal généreux : Hello + FullState + replay de l'outbox sont postés avant l'ouverture du flux.
    let (tx, rx) = mpsc::channel::<AgentMsg>(1024);
    let client = cfg.client.clone();
    let exec = client.as_ref().map(|c| {
        let e = Executor::new(c.clone(), cfg.root.clone());
        match master_host(&id.addr) {
            Some(h) => e.with_tracker_host(&h),
            None => e,
        }
    });
    let client_version = match &client {
        Some(c) => c.version().await.unwrap_or_default(),
        None => String::new(),
    };

    let mut snap_rx = shared.snap_rx.clone();
    let mut snap = Arc::new(Snapshot::default());
    if let Some(r) = snap_rx.as_mut() {
        // Attend le premier instantané réel du client (borné).
        let _ = tokio::time::timeout(Duration::from_secs(5), r.wait_for(|s| s.valid)).await;
        snap = r.borrow_and_update().clone();
    }
    let applied = shared.applied_rev.load(Ordering::Relaxed);
    let mut last_sig = snap.signature();
    let mut last_act = snap.activity_signature();

    let hello = A::Hello(Hello {
        node_id: id.node_id.clone(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: PROTOCOL_VERSION,
        client_kind: client.as_ref().map_or("none", |c| c.kind()).to_owned(),
        client_version,
    });
    // Hello, FullState, puis replay des événements non acquittés (dédoublonnés par event_id côté master).
    let mut sent: HashSet<String> = HashSet::new();
    let mut ok = send_msg(&tx, hello) && send_msg(&tx, full_state(&snap, applied));
    for e in shared.outbox.pending() {
        sent.insert(e.event_id.clone());
        ok &= send_msg(&tx, A::Event(e));
    }
    if !ok {
        return fail("canal interne saturé".into(), false);
    }
    // Un fichier .torrent peut peser jusqu'à 4 Mio : la limite par défaut de tonic (4 Mio) ne suffirait pas.
    let mut inbound = match OrchestratorClient::new(chan)
        .max_decoding_message_size(proto::MAX_MASTER_MSG)
        .connect(ReceiverStream::new(rx))
        .await
    {
        Ok(r) => r.into_inner(),
        Err(s) => return fail(format!("connect: {}", s.message()), false),
    };

    let mut hb = tokio::time::interval(cfg.hb_interval);
    let mut renew = tokio::time::interval(cfg.renew_check);
    let mut connected = false;
    let mut renew_sent = false;
    loop {
        tokio::select! {
            msg = inbound.message() => match msg {
                Ok(Some(m)) => match m.m {
                    Some(M_::Welcome(_)) => {
                        connected = true;
                        let _ = status.send(TunnelStatus::Connected);
                    }
                    Some(M_::Bye(b)) => {
                        return match b.reason.as_str() {
                            "revoked" | "unknown_node" => Outcome::Revoked,
                            "duplicate_session" => {
                                let _ = status.send(TunnelStatus::Disconnected(b.reason));
                                Outcome::Retry { connected, wait_max: true }
                            }
                            _ => fail(format!("bye: {}", b.reason), connected),
                        };
                    }
                    Some(M_::Renewed(c)) => {
                        // La clé reste la même ; seul le certificat est remplacé, puis on se reconnecte avec.
                        let (_, crt_p, _, _) = identity_paths(&cfg.data_dir);
                        // Validé avant écriture, écrit atomiquement : un certificat invalide ou un
                        // crash en cours d'écriture ne doit jamais remplacer le certificat qui marche.
                        if renewed_cert_ok(&c.cert_pem, &id.node_id, now_secs())
                            && write_secret_atomic(&crt_p, c.cert_pem.as_bytes()).is_ok()
                        {
                            return Outcome::Reconnect;
                        }
                        tracing::warn!("certificat renouvelé refusé");
                        renew_sent = false;
                    }
                    Some(M_::EventAck(a)) => shared.outbox.ack(&a.event_ids),
                    // Commandes ponctuelles du master : lister un dossier, créer un .torrent, annuler. La réponse est un
                    // message à part (pas d'Ack) ; tout reste sous la racine locale.
                    Some(M_::Cmd(c))
                        if matches!(
                            c.kind,
                            Some(Kind::ListDir(_) | Kind::CreateTorrent(_) | Kind::CancelJob(_))
                        ) =>
                    {
                        handle_job_command(&c, &cfg.root, &shared.jobs, &tx);
                    }
                    Some(M_::Cmd(c)) => {
                        let ack = if let Some(Kind::FetchMetainfo(t)) = &c.kind {
                            fetch_metainfo(exec.as_ref(), &c.cmd_id, &t.infohash, &tx).await
                        } else {
                            run_command(exec.as_ref(), &c).await
                        };
                        if !send_msg(&tx, A::Ack(ack)) { return fail("canal interne saturé".into(), connected); }
                    }
                    Some(M_::Desired(d)) => {
                        // Le master est la source de vérité : on applique les commandes, puis on
                        // acquitte la révision (uniquement si toutes ont réussi).
                        let mut all_ok = true;
                        for c in &d.commands {
                            // Remontée de métadonnées : au mieux. Un client qui ne sait pas exporter ne doit pas
                            // empêcher d'acquitter la révision.
                            if let Some(Kind::FetchMetainfo(t)) = &c.kind {
                                let ack = fetch_metainfo(exec.as_ref(), &c.cmd_id, &t.infohash, &tx).await;
                                if !send_msg(&tx, A::Ack(ack)) { return fail("canal interne saturé".into(), connected); }
                                continue;
                            }
                            let ack = run_command(exec.as_ref(), c).await;
                            all_ok &= ack.ok;
                            if !send_msg(&tx, A::Ack(ack)) { return fail("canal interne saturé".into(), connected); }
                        }
                        if all_ok {
                            shared.applied_rev.store(d.rev, Ordering::Relaxed);
                            if let Err(e) = std::fs::write(&shared.applied_path, d.rev.to_string()) {
                                tracing::warn!(error = %e, "applied_rev non persistée");
                            }
                            let applied = d.rev;
                            if !send_msg(&tx, heartbeat(&snap, applied)) { return fail("canal interne saturé".into(), connected); }
                        }
                    }
                    None => {}
                },
                Ok(None) => return fail("tunnel fermé par le master".into(), connected),
                Err(e) => return fail(format!("tunnel: {}", e.message()), connected),
            },
            () = wait_changed(&mut snap_rx) => {
                if let Some(r) = snap_rx.as_mut() {
                    snap = r.borrow_and_update().clone();
                }
                // Nouveaux événements (ceux déjà envoyés dans cette session sont ignorés).
                for e in shared.outbox.pending() {
                    if sent.insert(e.event_id.clone()) && !send_msg(&tx, A::Event(e)) {
                        return fail("canal interne saturé".into(), connected);
                    }
                }
                // État complet : à chaque changement structurel (ajout, retrait, statut) ET tant que la progression
                // ou les débits bougent (affichage en direct). Un parc au repos n'envoie rien.
                let sig = snap.signature();
                let act = snap.activity_signature();
                if sig != last_sig || (act != last_act && snap.valid) {
                    last_sig = sig;
                    last_act = act;
                    let applied = shared.applied_rev.load(Ordering::Relaxed);
                    if !send_msg(&tx, full_state(&snap, applied)) { return fail("canal interne saturé".into(), connected); }
                }
            }
            _ = hb.tick(), if connected => {
                let applied = shared.applied_rev.load(Ordering::Relaxed);
                if !send_msg(&tx, heartbeat(&snap, applied)) { return fail("canal interne saturé".into(), connected); }
            }
            _ = renew.tick(), if connected && !renew_sent => {
                if parse_pem(&id.cert_pem).is_some_and(|c| c.needs_renewal(now_secs())) {
                    if let Some(csr) = new_csr(&id.key_pem) {
                        renew_sent = true;
                        if !send_msg(&tx, A::Renew(CertRenewReq { csr_pem: csr })) { return fail("canal interne saturé".into(), connected); }
                    }
                }
            }
        }
    }
}

/// Exécute une commande ponctuelle du master (listage, création de .torrent, annulation). Ne bloque jamais le fil du
/// tunnel : le listage et le hachage tournent sur des fils dédiés.
fn handle_job_command(
    c: &proto::pb::Command,
    root: &std::path::Path,
    jobs: &Arc<crate::jobs::JobTable>,
    tx: &mpsc::Sender<AgentMsg>,
) {
    use proto::pb::{CreateProgress, DirListing, TorrentCreated};
    match &c.kind {
        Some(Kind::ListDir(l)) => {
            let (root, tx, l) = (root.to_owned(), tx.clone(), l.clone());
            tokio::task::spawn_blocking(move || {
                let reply = match crate::jobs::browse(&root, &l.path) {
                    Ok((entries, truncated)) => DirListing {
                        req_id: l.req_id.clone(),
                        entries,
                        truncated,
                        error: String::new(),
                    },
                    Err(error) => DirListing {
                        req_id: l.req_id.clone(),
                        error,
                        ..DirListing::default()
                    },
                };
                let _ = send_msg(&tx, A::DirListing(reply));
            });
        }
        Some(Kind::CancelJob(j)) => {
            jobs.cancel(&j.job_id);
        }
        Some(Kind::CreateTorrent(t)) => {
            let fail = |msg: &str| {
                let _ = send_msg(
                    tx,
                    A::TorrentCreated(TorrentCreated {
                        job_id: t.job_id.clone(),
                        error: msg.to_owned(),
                        ..TorrentCreated::default()
                    }),
                );
            };
            if !crate::jobs::valid_id(&t.job_id) {
                return; // sans identifiant valide, personne ne saurait à qui répondre
            }
            let cancel = match jobs.start(&t.job_id) {
                Ok(f) => f,
                Err(m) => return fail(m),
            };
            let (root, tx, t, jobs) = (root.to_owned(), tx.clone(), t.clone(), jobs.clone());
            tokio::task::spawn_blocking(move || {
                // Progression : au plus une fois par seconde ; si le tunnel est tombé, on arrête de lire inutilement.
                let mut last = std::time::Instant::now() - std::time::Duration::from_secs(2);
                let mut progress = |done: u64, total: u64| {
                    if tx.is_closed() {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if last.elapsed() >= std::time::Duration::from_secs(1) || done == total {
                        last = std::time::Instant::now();
                        let _ = send_msg(
                            &tx,
                            A::CreateProgress(CreateProgress {
                                job_id: t.job_id.clone(),
                                done,
                                total,
                            }),
                        );
                    }
                };
                let reply = match crate::jobs::create(&root, &t, &cancel, &mut progress) {
                    Ok(bytes) => TorrentCreated {
                        job_id: t.job_id.clone(),
                        torrent_file: bytes,
                        error: String::new(),
                    },
                    Err(error) => TorrentCreated {
                        job_id: t.job_id.clone(),
                        error,
                        ..TorrentCreated::default()
                    },
                };
                jobs.finish(&t.job_id);
                let _ = send_msg(&tx, A::TorrentCreated(reply));
            });
        }
        _ => {}
    }
}

/// Exporte le .torrent d'un torrent et l'envoie au master (message `Metainfo`).
async fn fetch_metainfo(
    exec: Option<&Executor>,
    cmd_id: &str,
    infohash: &str,
    tx: &mpsc::Sender<AgentMsg>,
) -> Ack {
    let res = match exec {
        Some(e) => e.export(infohash).await,
        None => Err("aucun client torrent".to_owned()),
    };
    match res {
        Ok(bytes) => {
            let sent = send_msg(
                tx,
                A::Metainfo(proto::pb::Metainfo {
                    infohash: infohash.to_ascii_lowercase(),
                    torrent_file: bytes,
                }),
            );
            Ack {
                cmd_id: cmd_id.to_owned(),
                ok: sent,
                error: if sent {
                    String::new()
                } else {
                    "canal saturé".to_owned()
                },
            }
        }
        Err(error) => {
            tracing::warn!(%cmd_id, %error, "export du .torrent impossible");
            Ack {
                cmd_id: cmd_id.to_owned(),
                ok: false,
                error,
            }
        }
    }
}

async fn run_command(exec: Option<&Executor>, c: &proto::pb::Command) -> Ack {
    let res = match exec {
        Some(e) => e.execute(c).await,
        None => Err("aucun client torrent".to_owned()),
    };
    if let Err(error) = &res {
        tracing::warn!(cmd_id = %c.cmd_id, %error, "commande refusée");
    }
    match res {
        Ok(()) => Ack {
            cmd_id: c.cmd_id.clone(),
            ok: true,
            error: String::new(),
        },
        Err(error) => Ack {
            cmd_id: c.cmd_id.clone(),
            ok: false,
            error,
        },
    }
}

/// Le certificat reçu doit porter NOTRE identité et ne pas être déjà expiré.
fn renewed_cert_ok(cert_pem: &str, node_id: &str, now: i64) -> bool {
    parse_pem(cert_pem).is_some_and(|c| c.node_id.as_deref() == Some(node_id) && c.not_after > now)
}

fn new_csr(key_pem: &str) -> Option<String> {
    let kp = KeyPair::from_pem(key_pem).ok()?;
    CertificateParams::new(Vec::<String>::new())
        .ok()?
        .serialize_request(&kp)
        .ok()?
        .pem()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewed_cert_must_be_ours_and_valid() {
        assert!(!renewed_cert_ok("", "n", 0));
        assert!(!renewed_cert_ok(
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----",
            "n",
            0
        ));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let (b, m) = (Duration::from_secs(1), Duration::from_secs(60));
        assert_eq!(backoff_delay(0, b, m, 1.0), Duration::from_secs(1));
        assert_eq!(backoff_delay(3, b, m, 1.0), Duration::from_secs(8));
        assert_eq!(backoff_delay(30, b, m, 1.0), m);
        // jitter : entre 50 % et 100 % du palier
        assert_eq!(backoff_delay(3, b, m, 0.0), Duration::from_secs(4));
    }
}
