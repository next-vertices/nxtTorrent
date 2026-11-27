//! Sessions d'administration, bornées en inactivité ET en durée absolue. Jeton aléatoire 256 bits dont seul
//! le condensat est conservé. Deux sortes : ordinaires (en mémoire, perdues au redémarrage) et durables
//! (« Rester connecté », enregistrées dans la base).
use crate::store::{unix_now, Store, StoreError};
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

/// Sessions durables conservées par compte ; au-delà, les moins récemment utilisées sont fermées.
const MAX_REMEMBERED: usize = 20;

/// Bornes d'une sorte de session.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub idle: Duration,
    pub absolute: Duration,
}

impl Limits {
    fn secs(self) -> (i64, i64) {
        let s = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        (s(self.idle), s(self.absolute))
    }
}

pub struct Sessions {
    idle: Duration,
    absolute: Duration,
    remember: Limits,
    inner: Mutex<HashMap<[u8; 32], Session>>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl Sessions {
    pub fn new(short: Limits, remember: Limits) -> Self {
        Self {
            idle: short.idle,
            absolute: short.absolute,
            remember,
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Session>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn new_token() -> String {
        let mut raw = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        data_encoding::HEXLOWER.encode(&raw)
    }

    /// Crée une session et rend le jeton à placer dans le cookie (jamais stocké en clair).
    /// `remember` : session durable, enregistrée dans la base.
    pub fn create(&self, store: &Store, user: &str, remember: bool) -> Result<String, StoreError> {
        let token = Self::new_token();
        if remember {
            let (idle, absolute) = self.remember.secs();
            store.admin_session_insert(
                &digest(&token),
                user,
                unix_now(),
                idle,
                absolute,
                MAX_REMEMBERED,
            )?;
            return Ok(token);
        }
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
        Ok(token)
    }

    /// Utilisateur de la session si elle est valide ; l'inactivité est glissante, la durée absolue non.
    pub fn lookup(&self, store: &Store, token: &str) -> Option<String> {
        self.find(store, token, true)
    }

    /// Comme `lookup` mais SANS prolonger l'inactivité : pour les rafraîchissements automatiques et les
    /// connexions longues (WebSocket), qui ne doivent pas maintenir une session ouverte à eux seuls.
    pub fn peek(&self, store: &Store, token: &str) -> Option<String> {
        self.find(store, token, false)
    }

    fn find(&self, store: &Store, token: &str, touch: bool) -> Option<String> {
        let key = digest(token);
        {
            let mut m = self.lock();
            if let Some(s) = m.get_mut(&key) {
                if s.last_seen.elapsed() >= self.idle || s.created.elapsed() >= self.absolute {
                    m.remove(&key);
                    return None;
                }
                if touch {
                    s.last_seen = Instant::now();
                }
                return Some(s.user.clone());
            }
        }
        let (idle, absolute) = self.remember.secs();
        // Une erreur de base refuse la session (jamais l'inverse).
        store
            .admin_session_lookup(&key, unix_now(), idle, absolute, touch, (idle / 10).min(60))
            .unwrap_or_else(|error| {
                tracing::error!(%error, "lecture d'une session impossible");
                None
            })
    }

    pub fn remove(&self, store: &Store, token: &str) -> Result<(), StoreError> {
        let key = digest(token);
        self.lock().remove(&key);
        store.admin_session_delete(&key)
    }

    /// Ferme toutes les sessions d'un utilisateur sauf (éventuellement) celle dont on fournit le jeton.
    pub fn remove_user_except(
        &self,
        store: &Store,
        user: &str,
        keep: Option<&str>,
    ) -> Result<(), StoreError> {
        let keep = keep.map(digest);
        self.lock()
            .retain(|k, s| s.user != user || Some(*k) == keep);
        store.admin_session_delete_user(user, keep.as_ref().map(<[u8; 32]>::as_slice))
    }

    /// Nombre de sessions ordinaires (en mémoire).
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

    fn limits(idle: Duration, absolute: Duration) -> Limits {
        Limits { idle, absolute }
    }

    fn sessions() -> Sessions {
        let l = limits(Duration::from_secs(60), Duration::from_secs(600));
        Sessions::new(l, l)
    }

    #[test]
    fn tokens_are_random_and_resolve_to_their_user() {
        let (s, db) = (sessions(), Store::open_in_memory().unwrap());
        for remember in [false, true] {
            let a = s.create(&db, "alice", remember).unwrap();
            let b = s.create(&db, "alice", remember).unwrap();
            assert_ne!(a, b);
            assert_eq!(a.len(), 64);
            assert_eq!(s.lookup(&db, &a).as_deref(), Some("alice"));
            assert!(s.lookup(&db, "inconnu").is_none());
            s.remove(&db, &a).unwrap();
            assert!(s.lookup(&db, &a).is_none());
            assert!(s.lookup(&db, &b).is_some());
        }
    }

    #[test]
    fn idle_timeout_is_sliding_but_absolute_is_not() {
        let l = limits(Duration::from_millis(150), Duration::from_millis(400));
        let (s, db) = (Sessions::new(l, l), Store::open_in_memory().unwrap());
        let t = s.create(&db, "alice", false).unwrap();
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(80));
            assert!(
                s.lookup(&db, &t).is_some(),
                "l'activité prolonge l'inactivité"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            s.lookup(&db, &t).is_none(),
            "durée absolue dépassée malgré l'activité"
        );
        let t2 = s.create(&db, "alice", false).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(s.lookup(&db, &t2).is_none(), "inactivité dépassée");
    }

    #[test]
    fn remembered_sessions_survive_a_restart_but_ordinary_ones_do_not() {
        let db = Store::open_in_memory().unwrap();
        let before = sessions();
        let short = before.create(&db, "alice", false).unwrap();
        let long = before.create(&db, "alice", true).unwrap();
        let after = sessions();
        assert!(after.lookup(&db, &short).is_none());
        assert_eq!(after.lookup(&db, &long).as_deref(), Some("alice"));
    }

    #[test]
    fn remembered_sessions_expire_and_are_capped() {
        let db = Store::open_in_memory().unwrap();
        let find = |d: &[u8], now, touch| {
            db.admin_session_lookup(d, now, 100, 1000, touch, 10)
                .unwrap()
        };
        db.admin_session_insert(b"a", "alice", 0, 100, 1000, 3)
            .unwrap();
        // Sans activité (rafraîchissement passif), l'inactivité court toujours.
        assert!(find(b"a", 99, false).is_some());
        assert!(find(b"a", 100, false).is_none());
        // L'activité prolonge l'inactivité, jamais la durée absolue.
        db.admin_session_insert(b"b", "alice", 0, 100, 1000, 3)
            .unwrap();
        for now in (90..1000).step_by(90) {
            assert!(find(b"b", now, true).is_some(), "à {now}");
        }
        assert!(find(b"b", 1000, true).is_none());
        for (i, d) in [b"1", b"2", b"3", b"4"].iter().enumerate() {
            let at = 2000 + i64::try_from(i).unwrap();
            db.admin_session_insert(*d, "alice", at, 100, 1000, 3)
                .unwrap();
        }
        assert!(find(b"1", 2010, false).is_none(), "la plus ancienne cède");
        assert!(find(b"4", 2010, false).is_some());
    }

    #[test]
    fn password_change_closes_other_sessions_only() {
        let (s, db) = (sessions(), Store::open_in_memory().unwrap());
        let new = |u, r| s.create(&db, u, r).unwrap();
        let keep = new("alice", true);
        let others = [new("alice", false), new("alice", true)];
        let bob = [new("bob", false), new("bob", true)];
        s.remove_user_except(&db, "alice", Some(&keep)).unwrap();
        assert!(s.lookup(&db, &keep).is_some());
        assert!(others.iter().all(|t| s.lookup(&db, t).is_none()));
        assert!(bob.iter().all(|t| s.lookup(&db, t).is_some()));
        s.remove_user_except(&db, "alice", None).unwrap();
        assert!(s.lookup(&db, &keep).is_none());
    }
}
