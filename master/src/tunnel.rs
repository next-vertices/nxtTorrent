//! Tunnel `Connect` côté master : une session par nœud, identité = SAN du certificat client.
use crate::state::{NodeState, Trigger};
use crate::{store::unix_now, Core};
use proto::certinfo::CertInfo;
use proto::pb::agent_msg::M as A;
use proto::pb::master_msg::M as M_;
use proto::pb::{AgentMsg, CertRenewed, Disconnect, MasterMsg, Welcome};
use proto::PROTOCOL_VERSION;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::{Status, Streaming};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ID_LEN: usize = 128;
const MAX_TORRENTS: usize = 20_000;
const MAX_FIELD: usize = 1024;
const MAX_CSR: usize = 4096;

struct Entry {
    sid: u64,
    kill: mpsc::UnboundedSender<&'static str>,
    out: Out,
}

/// Registre des sessions actives : au plus une par `node_id`.
#[derive(Default)]
pub struct Sessions {
    next: AtomicU64,
    inner: Mutex<HashMap<String, Entry>>,
}

impl Sessions {
    /// Enregistre une session ; l'éventuelle ancienne est fermée (`true` = doublon).
    pub fn register(
        &self,
        node_id: &str,
        out: Out,
    ) -> (u64, mpsc::UnboundedReceiver<&'static str>, bool) {
        let sid = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let old = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(node_id.to_owned(), Entry { sid, kill: tx, out });
        let dup = old.is_some();
        if let Some(old) = old {
            let _ = old.kill.send("duplicate_session");
        }
        (sid, rx, dup)
    }

    /// Retire la session si elle est toujours la session courante du nœud.
    pub fn unregister_if(&self, node_id: &str, sid: u64) -> bool {
        let mut m = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if m.get(node_id).is_some_and(|e| e.sid == sid) {
            m.remove(node_id);
            true
        } else {
            false
        }
    }

    /// Demande la fermeture de la session courante du nœud (la session se désenregistre seule).
    pub fn kill(&self, node_id: &str, reason: &'static str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(node_id)
            .is_some_and(|e| e.kill.send(reason).is_ok())
    }

    /// Envoie un message au nœud via son tunnel actif. `false` si hors ligne.
    pub async fn send(&self, node_id: &str, m: M_) -> bool {
        let out = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(node_id)
            .map(|e| e.out.clone());
        // Jamais d'attente : cette méthode est appelée sous le verrou global d'orchestration.
        // Canal plein = agent qui ne lit plus son flux -> on ferme sa session.
        match out {
            Some(tx) => match tx.try_send(Ok(MasterMsg { m: Some(m) })) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.kill(node_id, "slow_consumer");
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            },
            None => false,
        }
    }

    pub fn is_connected(&self, node_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(node_id)
    }
}

pub type Out = mpsc::Sender<Result<MasterMsg, Status>>;

/// Envoi vers l'agent, borné dans le temps : un agent qui ne lit plus ne doit pas figer sa session
/// (le signal de fermeture et le timeout de heartbeat doivent rester observables).
async fn send(tx: &Out, m: M_) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, tx.send(Ok(MasterMsg { m: Some(m) }))).await,
        Ok(Ok(()))
    )
}

async fn bye(tx: &Out, reason: &str) {
    let _ = send(
        tx,
        M_::Bye(Disconnect {
            reason: reason.to_owned(),
        }),
    )
    .await;
}

/// (type, infohash) d'un événement.
fn event_parts(e: &proto::pb::Event) -> Option<(&'static str, &str)> {
    use proto::pb::event::Kind;
    match e.kind.as_ref()? {
        Kind::Completed(h) => Some(("completed", h)),
        Kind::Error(h) => Some(("error", h)),
        Kind::Added(h) => Some(("added", h)),
        Kind::Removed(h) => Some(("removed", h)),
    }
}

fn bad_id(s: &str) -> bool {
    s.is_empty() || s.len() > MAX_ID_LEN || s.chars().any(char::is_control)
}

/// Validation stricte des messages d'un agent (taille, champs requis).
fn validate(m: &A) -> Result<(), &'static str> {
    match m {
        A::Hello(_) => Err("hello en double"),
        A::State(s) => {
            if s.torrents.len() > MAX_TORRENTS
                || s.torrents.iter().any(|t| {
                    t.infohash.is_empty()
                        || t.infohash.len() > MAX_ID_LEN
                        || t.name.len() > MAX_FIELD
                        || t.message.len() > MAX_FIELD
                        || t.tracker.len() > MAX_FIELD
                        || t.save_subdir.len() > MAX_FIELD
                })
            {
                return Err("full_state invalide");
            }
            Ok(())
        }
        A::Hb(_) => Ok(()),
        A::DirListing(l) => {
            if bad_id(&l.req_id)
                || l.entries.len() > 2000
                || l.error.len() > 300
                || l.entries.iter().any(|e| {
                    e.name.is_empty() || e.name.len() > 255 || e.name.chars().any(char::is_control)
                })
            {
                return Err("dir_listing invalide");
            }
            Ok(())
        }
        A::CreateProgress(p) => {
            if bad_id(&p.job_id) {
                return Err("create_progress invalide");
            }
            Ok(())
        }
        A::TorrentCreated(t) => {
            if bad_id(&t.job_id)
                || t.error.len() > 300
                || t.torrent_file.len() > proto::metainfo::MAX_TORRENT_FILE
                || (t.error.is_empty() && t.torrent_file.is_empty())
            {
                return Err("torrent_created invalide");
            }
            Ok(())
        }
        A::Metainfo(m) => {
            if bad_id(&m.infohash)
                || m.torrent_file.is_empty()
                || m.torrent_file.len() > proto::metainfo::MAX_TORRENT_FILE
            {
                return Err("metainfo invalide");
            }
            Ok(())
        }
        A::Event(e) => {
            if bad_id(&e.event_id) || event_parts(e).is_none_or(|(_, ih)| bad_id(ih)) {
                return Err("event invalide");
            }
            Ok(())
        }
        A::Ack(a) => {
            if bad_id(&a.cmd_id) || a.error.len() > MAX_FIELD {
                return Err("ack invalide");
            }
            Ok(())
        }
        A::Renew(r) => {
            if r.csr_pem.is_empty() || r.csr_pem.len() > MAX_CSR {
                return Err("renew invalide");
            }
            Ok(())
        }
    }
}

enum End {
    Closed,
    Reason(&'static str),
}

pub async fn run_session(
    core: Arc<Core>,
    node_id: String,
    cert: CertInfo,
    mut inbound: Streaming<AgentMsg>,
    tx: Out,
) {
    // 1. Hello obligatoire, avec délai.
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, inbound.message()).await {
        Ok(Ok(Some(AgentMsg {
            m: Some(A::Hello(h)),
        }))) => h,
        _ => {
            core.store
                .audit(&node_id, "protocol_violation", "hello attendu");
            bye(&tx, "protocol_error").await;
            return;
        }
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        bye(&tx, "protocol_version").await;
        return;
    }
    core.record(&node_id, |t| {
        t.client_kind.clone_from(&hello.client_kind);
        t.client_version.clone_from(&hello.client_version);
    });
    if hello.node_id != node_id {
        // L'identité déclarée est ignorée ; seule celle du certificat compte.
        tracing::warn!(node_id = %node_id, "identité déclarée différente du certificat (ignorée)");
    }

    // 2. Le nœud doit exister et ne pas être révoqué.
    // Une erreur de base de données n'est PAS « nœud inconnu » : l'agent traiterait cela comme une
    // révocation définitive et ne se reconnecterait plus. Raison réessayable à la place.
    let info = match core.store.node_info(&node_id) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, "base de données indisponible à la connexion");
            bye(&tx, "internal_error").await;
            return;
        }
    };
    let state = info.and_then(|i| NodeState::parse(&i.state));
    match state {
        None => {
            core.store
                .audit(&node_id, "connect_refused", "nœud inconnu");
            bye(&tx, "unknown_node").await;
            return;
        }
        Some(NodeState::Revoked) => {
            core.store
                .audit(&node_id, "connect_refused", "nœud révoqué");
            bye(&tx, "revoked").await;
            return;
        }
        Some(_) => {}
    }

    // 3. Un seul tunnel actif par node_id.
    let (sid, mut kill_rx, dup) = core.sessions.register(&node_id, tx.clone());
    if dup {
        core.store
            .audit(&node_id, "duplicate_session", "ancien tunnel fermé");
        tracing::warn!(node_id = %node_id, "ALERTE: second tunnel avec le même certificat");
    }
    core.apply(&node_id, Trigger::Connected);
    if !send(
        &tx,
        M_::Welcome(Welcome {
            node_id: node_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            desired_rev: core.store.node_rev(&node_id).unwrap_or(0),
        }),
    )
    .await
    {
        core.sessions.unregister_if(&node_id, sid);
        core.apply(&node_id, Trigger::Lost);
        return;
    }
    tracing::info!(node_id = %node_id, "tunnel établi");

    // 4. Boucle principale.
    let mut first_state = true;
    let end = loop {
        tokio::select! {
            reason = kill_rx.recv() => {
                break End::Reason(reason.unwrap_or("closed"));
            }
            msg = tokio::time::timeout(core.settings.hb_timeout, inbound.message()) => {
                let m = match msg {
                    Err(_) => break End::Reason("heartbeat_timeout"),
                    Ok(Ok(Some(AgentMsg { m: Some(m) }))) => m,
                    Ok(Ok(Some(_))) => break End::Reason("protocol_error"),
                    Ok(Ok(None) | Err(_)) => break End::Closed,
                };
                if let Err(why) = validate(&m) {
                    core.store.audit(&node_id, "protocol_violation", why);
                    break End::Reason("protocol_error");
                }
                let _ = core.store.touch(&node_id, unix_now());
                match m {
                    A::Renew(r) => handle_renew(&core, &node_id, &cert, &r.csr_pem, &tx).await,
                    // Création de liens physiques (cross-seed) : l'issue décide de la suite (ajout ou renoncement).
                    A::Ack(a) if a.cmd_id.contains("-lnk-") => {
                        if !a.ok {
                            let detail: String = a.error.chars().take(200).collect();
                            core.store.audit(&node_id, "command_refused", &format!("{} {detail}", a.cmd_id));
                        }
                        core.on_link_ack(&node_id, &a).await;
                        core.record(&node_id, |t| t.push_ack(a));
                    }
                    A::Ack(a) if !a.ok => {
                        let detail: String = a.error.chars().take(200).collect();
                        if a.cmd_id.contains("-fmi-") {
                            // Le client ne sait pas exporter le .torrent : les copies passent à l'ancien cycle.
                            core.note_metainfo_failure(&a.cmd_id);
                            core.schedule_pending().await;
                        }
                        core.store.audit(&node_id, "command_refused", &format!("{} {detail}", a.cmd_id));
                        core.record(&node_id, |t| t.push_ack(a));
                    }
                    A::Ack(a) => core.record(&node_id, |t| t.push_ack(a)),
                    A::State(st) => {
                        // Un torrent déjà complet dans le FullState vaut événement Completed : couvre
                        // les complétions survenues pendant l'arrêt du processus agent.
                        let done: Vec<String> = st
                            .torrents
                            .iter()
                            .filter(|t| t.progress >= 0.9999)
                            .map(|t| t.infohash.clone())
                            .collect();
                        core.record(&node_id, |t| {
                            t.torrents = st.torrents;
                            t.disk_free = st.disk_free_bytes;
                            t.applied_rev = st.applied_rev;
                            t.full_states += 1;
                        });
                        for ih in &done {
                            core.on_completed(&node_id, ih).await;
                        }
                        // Le master reste la source de vérité : on pousse ce qui manque au nœud.
                        core.schedule_pending().await;
                        core.reconcile(&node_id).await;
                        // Limites de débit voulues : une fois par session (le client a pu redémarrer entre-temps).
                        if std::mem::take(&mut first_state) {
                            core.apply_limits(&node_id).await;
                        }
                    }
                    A::Hb(h) => core.record(&node_id, |t| {
                        t.applied_rev = h.applied_rev;
                        if h.disk_free > 0 {
                            t.disk_free = h.disk_free;
                        }
                        t.dl_rate = h.dl_rate;
                        t.ul_rate = h.ul_rate;
                        if let Some(nh) = h.health {
                            t.health = nh;
                        }
                        t.heartbeats += 1;
                    }),
                    A::Event(e) => handle_event(&core, &node_id, e, &tx).await,
                    A::DirListing(l) => core.on_dir_listing(&node_id, l),
                    A::CreateProgress(p) => core.on_create_progress(&node_id, &p),
                    A::TorrentCreated(t) => core.on_torrent_created(&node_id, t).await,
                    A::Metainfo(m) => core.on_metainfo(&node_id, &m.infohash, &m.torrent_file).await,
                    A::Hello(_) => {}
                }
            }
        }
    };

    // 5. Fin de session.
    if let End::Reason(r) = &end {
        if *r != "closed" {
            bye(&tx, r).await;
        }
    }
    if core.sessions.unregister_if(&node_id, sid) {
        core.fail_torrent_jobs_of(&node_id);
        core.apply(&node_id, Trigger::Lost);
        tracing::info!(node_id = %node_id, "tunnel fermé");
    }
}

async fn handle_event(core: &Arc<Core>, node_id: &str, e: proto::pb::Event, tx: &Out) {
    let Some((kind, ih)) = event_parts(&e).map(|(k, h)| (k, h.to_owned())) else {
        return;
    };
    let id = e.event_id.clone();
    // Dédoublonnage persistant par event_id : les agents rejouent leur outbox après une coupure.
    let Ok(is_new) = core.store.record_event(&id, node_id, kind, &ih) else {
        return; // pas d'acquittement : l'agent rejouera
    };
    core.record(node_id, |t| t.push_event(e));
    let _ = send(
        tx,
        M_::EventAck(proto::pb::EventAck {
            event_ids: vec![id],
        }),
    )
    .await;
    if is_new && kind == "completed" {
        core.on_completed(node_id, &ih).await;
    }
}

async fn handle_renew(core: &Arc<Core>, node_id: &str, cert: &CertInfo, csr: &str, tx: &Out) {
    let active = core
        .store
        .node_info(node_id)
        .ok()
        .flatten()
        .and_then(|i| NodeState::parse(&i.state))
        .is_some_and(|st| st != NodeState::Revoked);
    if !active || !cert.needs_renewal(unix_now()) {
        core.store.audit(
            node_id,
            "renew_refused",
            if active { "trop tôt" } else { "révoqué" },
        );
        return;
    }
    match core
        .ca
        .sign_node_csr(csr, node_id, core.settings.cert_validity)
    {
        Ok(pem) => {
            core.store.audit(node_id, "cert_renewed", "");
            let _ = send(tx, M_::Renewed(CertRenewed { cert_pem: pem })).await;
        }
        Err(e) => {
            tracing::warn!(node_id, error = %e, "CSR de renouvellement refusé");
            core.store.audit(node_id, "renew_refused", "csr invalide");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bye_msg() -> M_ {
        M_::Bye(Disconnect { reason: "x".into() })
    }

    #[tokio::test]
    async fn full_channel_closes_the_slow_session_without_blocking() {
        let sessions = Sessions::default();
        let (tx, _rx_never_read) = mpsc::channel(2);
        let (_sid, mut kill_rx, _) = sessions.register("n", tx);
        assert!(sessions.send("n", bye_msg()).await);
        assert!(sessions.send("n", bye_msg()).await);
        // Canal plein : retour immédiat (pas d'attente sous le verrou global) + fermeture.
        let r =
            tokio::time::timeout(Duration::from_millis(200), sessions.send("n", bye_msg())).await;
        assert_eq!(r, Ok(false));
        assert_eq!(kill_rx.recv().await, Some("slow_consumer"));
    }

    #[tokio::test]
    async fn send_to_unknown_node_is_false() {
        assert!(!Sessions::default().send("absent", bye_msg()).await);
    }
}
