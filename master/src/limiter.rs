//! Limiteur d'échecs à fenêtre glissante (anti brute-force) : par IP pour l'appairage, par IP ET par
//! nom de compte pour la connexion admin.
use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct FailureLimiter<K = IpAddr> {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<K, Vec<Instant>>>,
}

impl<K: Hash + Eq + Clone> FailureLimiter<K> {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<K, Vec<Instant>>> {
        self.hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Durée restante de blocage (`None` = pas bloqué). Le blocage cesse quand le plus ancien des
    /// échecs retenus sort de la fenêtre.
    pub fn blocked_for(&self, key: &K) -> Option<Duration> {
        let mut m = self.lock();
        let v = m.get_mut(key)?;
        v.retain(|t| t.elapsed() < self.window);
        if v.len() < self.max {
            return None;
        }
        let oldest = v.iter().min()?;
        Some(self.window.saturating_sub(oldest.elapsed()))
    }

    pub fn is_blocked(&self, key: K) -> bool {
        self.blocked_for(&key).is_some()
    }

    pub fn record_failure(&self, key: K) {
        let mut m = self.lock();
        if m.len() > 10_000 {
            m.retain(|_, v| v.iter().any(|t| t.elapsed() < self.window));
        }
        m.entry(key).or_default().push(Instant::now());
    }

    /// Une connexion réussie remet le compteur de cette clé à zéro.
    pub fn clear(&self, key: &K) {
        self.lock().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_max() {
        let l = FailureLimiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..3 {
            assert!(!l.is_blocked(ip));
            l.record_failure(ip);
        }
        assert!(l.is_blocked(ip));
        assert!(!l.is_blocked("10.0.0.2".parse().unwrap()));
    }

    #[test]
    fn keyed_by_string_reports_remaining_time_and_clears() {
        let l: FailureLimiter<String> = FailureLimiter::new(2, Duration::from_secs(60));
        let k = "admin".to_owned();
        assert!(l.blocked_for(&k).is_none());
        l.record_failure(k.clone());
        l.record_failure(k.clone());
        let left = l.blocked_for(&k).expect("bloqué");
        assert!(left > Duration::from_secs(50) && left <= Duration::from_secs(60));
        l.clear(&k);
        assert!(l.blocked_for(&k).is_none());
    }

    #[test]
    fn block_expires_with_the_window() {
        let l: FailureLimiter<String> = FailureLimiter::new(1, Duration::from_millis(80));
        l.record_failure("a".into());
        assert!(l.is_blocked("a".into()));
        std::thread::sleep(Duration::from_millis(120));
        assert!(!l.is_blocked("a".into()));
    }
}
