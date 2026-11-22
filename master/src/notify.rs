//! Notifications de transition de présence (interface extensible : webhook, ntfy…).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceKind {
    WentOffline,
    CameOnline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub node_id: String,
    pub kind: PresenceKind,
}

pub trait Notifier: Send + Sync {
    fn notify(&self, n: &Notification);
}

/// Notifier par défaut : journal structuré uniquement.
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, n: &Notification) {
        tracing::warn!(node_id = %n.node_id, kind = ?n.kind, "changement de présence confirmé");
    }
}
