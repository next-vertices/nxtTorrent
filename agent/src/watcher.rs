//! Surveillance du client torrent, INDÉPENDANTE du tunnel : le client continue de travailler
//! quand le master est injoignable, et les événements (Completed…) sont mis en outbox.
use crate::client::TorrentClient;
use crate::outbox::Outbox;
use proto::pb::event::Kind as EK;
use proto::pb::{Event, TorrentInfo};
use rand::Rng;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Default)]
pub struct Snapshot {
    pub torrents: Vec<TorrentInfo>,
    pub disk_free: u64,
    /// Faux tant que le client n'a pas été interrogé avec succès.
    pub valid: bool,
    /// Santé réseau du client (rafraîchie toutes les 30 s seulement : elle bouge lentement).
    pub health: proto::pb::NodeHealth,
}

impl Snapshot {
    /// Signature (infohash, statut, complet) : change quand un torrent apparaît, disparaît, change
    /// d'état ou termine, pas à chaque pourcentage de progression (évite de noyer le master de FullState).
    /// Empreinte de l'activité (progression au pour-cent près, débits) : change tant que des transferts
    /// avancent, reste stable pour un parc au repos (rien à renvoyer au master dans ce cas).
    pub fn activity_signature(&self) -> u64 {
        let mut v: Vec<(&str, u32, u64, u64)> = self
            .torrents
            .iter()
            .map(|t| {
                (
                    t.infohash.as_str(),
                    (t.progress.clamp(0.0, 1.0) * 100.0) as u32,
                    t.dl_rate,
                    t.ul_rate,
                )
            })
            .collect();
        v.sort_unstable();
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    pub fn signature(&self) -> u64 {
        // Le critère « complet » est indispensable : une vérification qui aboutit sans changement
        // de statut (Paused -> Paused) doit quand même être signalée au master.
        let mut v: Vec<(&str, i32, bool)> = self
            .torrents
            .iter()
            .map(|t| (t.infohash.as_str(), t.status, t.progress >= DONE))
            .collect();
        v.sort_unstable();
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }
}

fn new_event_id() -> String {
    let b: [u8; 16] = rand::thread_rng().gen();
    data_encoding::HEXLOWER.encode(&b)
}

/// Seuil de progression à partir duquel un torrent est considéré complet.
const DONE: f32 = 0.9999;

/// Événements déduits de deux instantanés successifs (Added, Removed, Completed).
pub fn diff_events(prev: &HashMap<String, f32>, now: &[TorrentInfo]) -> Vec<Event> {
    let mut out = Vec::new();
    let mut ev = |kind: EK| {
        out.push(Event {
            event_id: new_event_id(),
            kind: Some(kind),
        })
    };
    for t in now {
        match prev.get(&t.infohash) {
            None => {
                ev(EK::Added(t.infohash.clone()));
                if t.progress >= DONE {
                    ev(EK::Completed(t.infohash.clone()));
                }
            }
            Some(p) if *p < DONE && t.progress >= DONE => ev(EK::Completed(t.infohash.clone())),
            Some(_) => {}
        }
    }
    for h in prev.keys() {
        if !now.iter().any(|t| &t.infohash == h) {
            ev(EK::Removed(h.clone()));
        }
    }
    out
}

async fn take(
    client: &dyn TorrentClient,
    last_disk: u64,
) -> Result<Snapshot, crate::client::ClientError> {
    let torrents = client.list().await?;
    // Espace disque : une erreur ponctuelle ne doit ni masquer la liste ni écraser la dernière
    // valeur connue par 0 (le scheduler exclurait le nœud).
    let disk_free = client.disk_free().await.unwrap_or(last_disk);
    Ok(Snapshot {
        torrents,
        disk_free,
        valid: true,
        health: proto::pb::NodeHealth::default(),
    })
}

/// Démarre la surveillance. Le premier instantané sert de ligne de base silencieuse.
const HEALTH_EVERY: Duration = Duration::from_secs(30);

pub fn spawn(
    client: Arc<dyn TorrentClient>,
    outbox: Arc<Outbox>,
    interval: Duration,
) -> (JoinHandle<()>, watch::Receiver<Arc<Snapshot>>) {
    let (tx, rx) = watch::channel(Arc::new(Snapshot::default()));
    let handle = tokio::spawn(async move {
        let mut known: Option<HashMap<String, f32>> = None;
        let mut last_disk = 0u64;
        let mut tick = tokio::time::interval(interval);
        let mut health = proto::pb::NodeHealth::default();
        let mut health_at: Option<tokio::time::Instant> = None;
        loop {
            tick.tick().await;
            match take(client.as_ref(), last_disk).await {
                Ok(mut snap) => {
                    if health_at.is_none_or(|t| t.elapsed() >= HEALTH_EVERY) {
                        health_at = Some(tokio::time::Instant::now());
                        // Une erreur ponctuelle garde la dernière valeur connue.
                        if let Ok(h) = client.health().await {
                            health = h;
                        }
                    }
                    snap.health = health;
                    let now: HashMap<String, f32> = snap
                        .torrents
                        .iter()
                        .map(|t| (t.infohash.clone(), t.progress))
                        .collect();
                    if let Some(prev) = &known {
                        // Persistance AVANT publication : aucun événement ne peut être perdu.
                        for e in diff_events(prev, &snap.torrents) {
                            outbox.push(e);
                        }
                    }
                    known = Some(now);
                    last_disk = snap.disk_free;
                    tx.send_replace(Arc::new(snap));
                }
                Err(e) => tracing::warn!(error = %e, "client torrent indisponible"),
            }
        }
    });
    (handle, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ti(h: &str, p: f32) -> TorrentInfo {
        TorrentInfo {
            infohash: h.into(),
            progress: p,
            ..TorrentInfo::default()
        }
    }

    #[test]
    fn events_from_snapshots() {
        let prev: HashMap<String, f32> =
            [("a".into(), 0.5), ("b".into(), 1.0), ("gone".into(), 0.1)].into();
        let ev = diff_events(&prev, &[ti("a", 1.0), ti("b", 1.0), ti("new", 0.0)]);
        let kinds: Vec<_> = ev.iter().filter_map(|e| e.kind.clone()).collect();
        assert!(kinds.contains(&EK::Completed("a".into())));
        assert!(kinds.contains(&EK::Added("new".into())));
        assert!(kinds.contains(&EK::Removed("gone".into())));
        assert_eq!(kinds.len(), 3, "b déjà terminé : aucun événement");
        let ids: std::collections::HashSet<_> = ev.iter().map(|e| e.event_id.clone()).collect();
        assert_eq!(ids.len(), 3, "event_id uniques");
    }

    #[test]
    fn signature_ignores_progress_but_not_status() {
        let mut a = Snapshot {
            torrents: vec![ti("x", 0.1)],
            disk_free: 0,
            valid: true,
            health: proto::pb::NodeHealth::default(),
        };
        let s1 = a.signature();
        a.torrents[0].progress = 0.7;
        assert_eq!(a.signature(), s1);
        a.torrents[0].status = 4;
        assert_ne!(a.signature(), s1);
        a.torrents[0].status = 0;
        a.torrents[0].progress = 1.0;
        assert_ne!(
            a.signature(),
            s1,
            "la complétion seule doit changer la signature"
        );
    }
}
