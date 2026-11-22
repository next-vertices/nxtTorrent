//! Outbox persistante : événements non acquittés par le master, rejoués à la reconnexion.
//! Format : une ligne par événement, protobuf encodé en hexadécimal. Réécriture atomique à l'acquittement.
use data_encoding::HEXLOWER;
use prost::Message;
use proto::pb::Event;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Au-delà, les plus anciens événements sont abandonnés (master injoignable très longtemps).
const MAX_PENDING: usize = 10_000;

pub struct Outbox {
    path: PathBuf,
    pending: Mutex<Vec<Event>>,
}

impl Outbox {
    pub fn open(path: PathBuf) -> std::io::Result<Arc<Self>> {
        let mut pending = Vec::new();
        let mut dirty = false;
        if let Ok(text) = std::fs::read_to_string(&path) {
            // Un crash en cours d'écriture laisse une ligne tronquée SANS saut de ligne : sans
            // réparation, l'événement suivant lui serait accolé et les deux seraient perdus.
            dirty = !text.is_empty() && !text.ends_with('\n');
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match HEXLOWER
                    .decode(line.trim().as_bytes())
                    .ok()
                    .and_then(|b| Event::decode(b.as_slice()).ok())
                {
                    Some(e) => pending.push(e),
                    None => dirty = true,
                }
            }
        }
        let outbox = Arc::new(Self {
            path,
            pending: Mutex::new(pending),
        });
        if dirty {
            tracing::warn!("outbox réparée (ligne corrompue ou tronquée ignorée)");
            outbox.rewrite(&outbox.lock())?;
        }
        Ok(outbox)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Event>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn rewrite(&self, events: &[Event]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp)?;
        for e in events {
            writeln!(f, "{}", HEXLOWER.encode(&e.encode_to_vec()))?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Ajoute un événement (persisté avant tout envoi).
    pub fn push(&self, e: Event) {
        let mut g = self.lock();
        let res = if g.len() >= MAX_PENDING {
            g.remove(0);
            g.push(e);
            self.rewrite(&g)
        } else {
            let line = HEXLOWER.encode(&e.encode_to_vec());
            g.push(e);
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .and_then(|mut f| {
                    writeln!(f, "{line}")?;
                    // Garantie annoncée : persisté AVANT tout envoi.
                    f.sync_data()
                })
        };
        if let Err(err) = res {
            tracing::error!(error = %err, "écriture de l'outbox impossible");
        }
    }

    pub fn pending(&self) -> Vec<Event> {
        self.lock().clone()
    }

    /// Purge les événements acquittés par le master.
    pub fn ack(&self, ids: &[String]) {
        let mut g = self.lock();
        let before = g.len();
        g.retain(|e| !ids.contains(&e.event_id));
        if g.len() != before {
            if let Err(err) = self.rewrite(&g) {
                tracing::error!(error = %err, "réécriture de l'outbox impossible");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::pb::event::Kind;

    fn ev(id: &str) -> Event {
        Event {
            event_id: id.into(),
            kind: Some(Kind::Completed("h".into())),
        }
    }

    #[test]
    fn persists_replays_and_acks() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("outbox.log");
        let o = Outbox::open(p.clone()).unwrap();
        o.push(ev("a"));
        o.push(ev("b"));
        o.push(ev("c"));
        // Redémarrage : tout est rejoué.
        let o2 = Outbox::open(p.clone()).unwrap();
        assert_eq!(
            o2.pending()
                .iter()
                .map(|e| e.event_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        o2.ack(&["b".into(), "inconnu".into()]);
        let o3 = Outbox::open(p).unwrap();
        assert_eq!(
            o3.pending()
                .iter()
                .map(|e| e.event_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
    }

    #[test]
    fn torn_last_line_does_not_swallow_the_next_event() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("outbox.log");
        let o = Outbox::open(p.clone()).unwrap();
        o.push(ev("a"));
        // Crash en pleine écriture : début de ligne, pas de saut de ligne final.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"0a1b2c")
            .unwrap();
        let o2 = Outbox::open(p.clone()).unwrap(); // répare
        o2.push(ev("b"));
        let ids: Vec<_> = Outbox::open(p)
            .unwrap()
            .pending()
            .into_iter()
            .map(|e| e.event_id)
            .collect();
        assert_eq!(ids, ["a", "b"], "aucun événement perdu après réparation");
    }

    #[test]
    fn complete_line_missing_only_its_newline_is_not_glued_to_the_next() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("outbox.log");
        let o = Outbox::open(p.clone()).unwrap();
        o.push(ev("a"));
        // Crash entre l'écriture de la ligne et celle du saut de ligne : la ligne est VALIDE.
        let line = HEXLOWER.encode(&ev("b").encode_to_vec());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
        let o2 = Outbox::open(p.clone()).unwrap();
        o2.push(ev("c"));
        let ids: Vec<_> = Outbox::open(p)
            .unwrap()
            .pending()
            .into_iter()
            .map(|e| e.event_id)
            .collect();
        assert_eq!(ids, ["a", "b", "c"], "ni perte ni fusion de lignes");
    }

    #[test]
    fn corrupt_line_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("outbox.log");
        let o = Outbox::open(p.clone()).unwrap();
        o.push(ev("a"));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"zz-corrompu\n")
            .unwrap();
        assert_eq!(Outbox::open(p).unwrap().pending().len(), 1);
    }
}
