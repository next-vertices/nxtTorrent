//! Machine à états d'un nœud (fonctions pures, testables).
//!
//! Pending --pair--> Online --tunnel fermé / heartbeats manqués--> Offline
//! Offline --reconnexion + FullState--> Online
//! Offline --délai de grâce--> Offline(réassignable)   (dérivé, non stocké)
//! Tout état --révocation--> Revoked (terminal)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeState {
    Pending,
    Online,
    Offline,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Paired,
    Connected,
    Lost,
    Revoked,
}

impl NodeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Online => "Online",
            Self::Offline => "Offline",
            Self::Revoked => "Revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Pending" => Self::Pending,
            "Online" => Self::Online,
            "Offline" => Self::Offline,
            "Revoked" => Self::Revoked,
            _ => return None,
        })
    }
}

pub fn next(state: NodeState, t: Trigger) -> NodeState {
    use NodeState::{Offline, Online, Revoked};
    match (state, t) {
        (Revoked, _) | (_, Trigger::Revoked) => Revoked,
        (_, Trigger::Paired | Trigger::Connected) => Online,
        (Online, Trigger::Lost) => Offline,
        (s, Trigger::Lost) => s,
    }
}

/// Un nœud hors ligne depuis plus que le délai de grâce devient réassignable.
pub fn is_reassignable(
    state: NodeState,
    offline_since: Option<i64>,
    now: i64,
    grace_secs: i64,
) -> bool {
    state == NodeState::Offline && offline_since.is_some_and(|t| now - t >= grace_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use NodeState::*;

    #[test]
    fn transitions() {
        assert_eq!(next(Pending, Trigger::Paired), Online);
        assert_eq!(next(Online, Trigger::Lost), Offline);
        assert_eq!(next(Offline, Trigger::Connected), Online);
        assert_eq!(next(Offline, Trigger::Lost), Offline);
        assert_eq!(next(Pending, Trigger::Lost), Pending);
    }

    #[test]
    fn revoked_is_terminal() {
        for s in [Pending, Online, Offline, Revoked] {
            assert_eq!(next(s, Trigger::Revoked), Revoked);
        }
        for t in [Trigger::Paired, Trigger::Connected, Trigger::Lost] {
            assert_eq!(next(Revoked, t), Revoked);
        }
    }

    #[test]
    fn reassignable_after_grace() {
        assert!(!is_reassignable(Offline, Some(100), 150, 60));
        assert!(is_reassignable(Offline, Some(100), 160, 60));
        assert!(!is_reassignable(Online, Some(100), 999, 60));
        assert!(!is_reassignable(Offline, None, 999, 60));
    }

    #[test]
    fn str_roundtrip() {
        for s in [Pending, Online, Offline, Revoked] {
            assert_eq!(NodeState::parse(s.as_str()), Some(s));
        }
    }
}
