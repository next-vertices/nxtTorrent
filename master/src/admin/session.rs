//! Sessions d'administration : courtes (inactivité ET durée absolue), en mémoire, jeton aléatoire
//! 256 bits dont seul le condensat est conservé.
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Session {
    user: String,
    created: Instant,
    last_seen: Instant,
}

pub struct Sessions {
    idle: Duration,
    absolute: Duration,
    inner: Mutex<HashMap<[u8; 32], Session>>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl Sessions {
    pub fn new(idle: Duration, absolute: Duration) -> Self {
        Self {
            idle,
            absolute,
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Session>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Crée une session et rend le jeton à placer dans le cookie (jamais stocké en clair).
    pub fn create(&self, user: &str) -> String {
        let mut raw = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        let token = data_encoding::HEXLOWER.encode(&raw);
        let now = Instant::now();
        let mut m = self.lock();
        // Purge opportuniste des sessions expirées.
        m.retain(|_, s| s.last_seen.elapsed() < self.idle && s.created.elapsed() < self.absolute);
        m.insert(
            digest(&token),
            Session {
                user: user.to_owned(),
                created: now,
                last_seen: now,
            },
        );
        token
    }

    /// Utilisateur de la session si elle est valide ; l'inactivité est glissante, la durée absolue non.
    pub fn lookup(&self, token: &str) -> Option<String> {
        let key = digest(token);
        let mut m = self.lock();
        let s = m.get_mut(&key)?;
        if s.last_seen.elapsed() >= self.idle || s.created.elapsed() >= self.absolute {
            m.remove(&key);
            return None;
        }
        s.last_seen = Instant::now();
        Some(s.user.clone())
    }

    /// Comme `lookup` mais SANS prolonger l'inactivité : pour une connexion longue (WebSocket) qui ne doit pas
    /// maintenir une session ouverte à elle seule.
    pub fn is_valid(&self, token: &str) -> bool {
        let key = digest(token);
        let mut m = self.lock();
        let Some(s) = m.get(&key) else { return false };
        if s.last_seen.elapsed() >= self.idle || s.created.elapsed() >= self.absolute {
            m.remove(&key);
            return false;
        }
        true
    }

    pub fn user_of(&self, token: &str) -> Option<String> {
        self.lock().get(&digest(token)).map(|s| s.user.clone())
    }

    pub fn remove(&self, token: &str) {
        self.lock().remove(&digest(token));
    }

    /// Ferme toutes les sessions d'un utilisateur sauf (éventuellement) celle dont on fournit le jeton.
    pub fn remove_user_except(&self, user: &str, keep: Option<&str>) {
        let keep = keep.map(digest);
        self.lock()
            .retain(|k, s| s.user != user || Some(*k) == keep);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_resolve_to_their_user() {
        let s = Sessions::new(Duration::from_secs(60), Duration::from_secs(600));
        let (a, b) = (s.create("alice"), s.create("alice"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(s.lookup(&a).as_deref(), Some("alice"));
        assert!(s.lookup("inconnu").is_none());
        s.remove(&a);
        assert!(s.lookup(&a).is_none());
        assert!(s.lookup(&b).is_some());
    }

    #[test]
    fn idle_timeout_is_sliding_but_absolute_is_not() {
        let s = Sessions::new(Duration::from_millis(150), Duration::from_millis(400));
        let t = s.create("alice");
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(80));
            assert!(s.lookup(&t).is_some(), "l'activité prolonge l'inactivité");
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            s.lookup(&t).is_none(),
            "durée absolue dépassée malgré l'activité"
        );
        let t2 = s.create("alice");
        std::thread::sleep(Duration::from_millis(200));
        assert!(s.lookup(&t2).is_none(), "inactivité dépassée");
    }

    #[test]
    fn password_change_closes_other_sessions_only() {
        let s = Sessions::new(Duration::from_secs(60), Duration::from_secs(600));
        let (keep, other, bob) = (s.create("alice"), s.create("alice"), s.create("bob"));
        s.remove_user_except("alice", Some(&keep));
        assert!(s.lookup(&keep).is_some());
        assert!(s.lookup(&other).is_none());
        assert!(s.lookup(&bob).is_some());
    }
}
