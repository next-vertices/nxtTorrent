//! Alertes configurables : règles (nœud hors ligne, port fermé, disque presque plein, torrents en erreur) évaluées
//! périodiquement, avec délai de confirmation (anti-battement) et notification de rétablissement, envoyées vers des
//! canaux (webhook JSON générique, ntfy).
//!
//! L'état (« depuis quand ça dure », « déjà notifié ») est en mémoire : un redémarrage du master repart de zéro et
//! renotifie ce qui est encore anormal après le délai de confirmation. Les URL des canaux peuvent contenir un secret
//! (token ntfy, webhook privé) : chiffrées au repos, jamais renvoyées par l'API, jamais journalisées.
use crate::health::Assessment;
use crate::store::NodeDetail;
use crate::NodeDigest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub enabled: bool,
    /// Durée pendant laquelle la condition doit durer avant d'alerter (secondes).
    pub after_secs: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DiskRule {
    pub enabled: bool,
    pub after_secs: i64,
    /// Alerte quand l'espace libre d'un nœud passe sous ce seuil (Go).
    pub threshold_gb: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rules {
    pub node_offline: Rule,
    pub port_closed: Rule,
    pub disk_low: DiskRule,
    pub torrent_errors: Rule,
    /// Envoyer aussi un message quand le problème disparaît.
    pub notify_resolved: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            node_offline: Rule {
                enabled: true,
                after_secs: 120,
            },
            port_closed: Rule {
                enabled: true,
                after_secs: 600,
            },
            disk_low: DiskRule {
                enabled: true,
                after_secs: 300,
                threshold_gb: 50,
            },
            torrent_errors: Rule {
                enabled: false,
                after_secs: 300,
            },
            notify_resolved: true,
        }
    }
}

impl Rules {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |s: i64| (30..=86_400).contains(&s);
        if ![
            self.node_offline.after_secs,
            self.port_closed.after_secs,
            self.disk_low.after_secs,
            self.torrent_errors.after_secs,
        ]
        .into_iter()
        .all(ok)
        {
            return Err("le délai doit être compris entre 30 secondes et 24 heures".into());
        }
        if !(1..=100_000).contains(&self.disk_low.threshold_gb) {
            return Err(
                "le seuil d'espace libre doit être compris entre 1 Go et 100 000 Go".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

/// Une condition évaluée : active ou non, avec le message à envoyer si elle se déclare.
#[derive(Clone, Debug)]
pub struct Cond {
    pub key: String,
    pub active: bool,
    pub severity: Severity,
    pub title: String,
    pub text: String,
    pub after_secs: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub key: String,
    /// `true` : l'alerte se déclenche ; `false` : le problème est rétabli.
    pub firing: bool,
    pub severity: Severity,
    pub title: String,
    pub text: String,
}

#[derive(Default)]
struct Slot {
    since: Option<i64>,
    firing: bool,
    severity: Option<Severity>,
    title: String,
}

#[derive(Default)]
pub struct AlertState {
    slots: HashMap<String, Slot>,
}

impl AlertState {
    /// Fait avancer la machine à états : une condition doit rester active `after_secs` avant de se déclarer ; une
    /// alerte déclarée n'est notifiée qu'une fois ; sa disparition produit un événement de rétablissement.
    pub fn step(&mut self, conds: &[Cond], now: i64) -> Vec<Event> {
        let mut out = Vec::new();
        for c in conds {
            let s = self.slots.entry(c.key.clone()).or_default();
            if c.active {
                let since = *s.since.get_or_insert(now);
                if !s.firing && now - since >= c.after_secs {
                    s.firing = true;
                    s.severity = Some(c.severity);
                    s.title.clone_from(&c.title);
                    out.push(Event {
                        key: c.key.clone(),
                        firing: true,
                        severity: c.severity,
                        title: c.title.clone(),
                        text: c.text.clone(),
                    });
                }
            } else {
                s.since = None;
                if s.firing {
                    s.firing = false;
                    out.push(Event {
                        key: c.key.clone(),
                        firing: false,
                        severity: s.severity.unwrap_or(Severity::Warning),
                        title: s.title.clone(),
                        text: "Le problème est rétabli.".into(),
                    });
                }
            }
        }
        // Conditions disparues (nœud révoqué, règle désactivée) : on rétablit ce qui était déclaré, puis on oublie.
        let live: std::collections::HashSet<&str> = conds.iter().map(|c| c.key.as_str()).collect();
        let gone: Vec<String> = self
            .slots
            .keys()
            .filter(|k| !live.contains(k.as_str()))
            .cloned()
            .collect();
        for k in gone {
            if let Some(s) = self.slots.remove(&k) {
                if s.firing {
                    out.push(Event {
                        key: k,
                        firing: false,
                        severity: s.severity.unwrap_or(Severity::Warning),
                        title: s.title,
                        text: "La surveillance de cet élément a été arrêtée (règle désactivée ou nœud retiré).".into(),
                    });
                }
            }
        }
        out
    }
}

/// Construit les conditions à partir de l'état du cluster.
pub fn conditions(
    rules: &Rules,
    nodes: &[NodeDetail],
    digests: &HashMap<String, NodeDigest>,
    torrents_in_error: u64,
) -> Vec<Cond> {
    let mut out = Vec::new();
    for n in nodes.iter().filter(|n| n.state != "Revoked") {
        let name = n
            .name
            .clone()
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| n.hostname.clone());
        let id = &n.id;
        let d = digests.get(id);
        if rules.node_offline.enabled {
            out.push(Cond {
                key: format!("node_offline:{id}"),
                // Un nœud « jamais connecté » (code pas encore utilisé) n'est pas une panne.
                active: n.state == "Offline",
                severity: Severity::Critical,
                title: format!("Le nœud « {name} » est hors ligne"),
                text: "Le master ne reçoit plus rien de ce nœud. Le client torrent continue de fonctionner de son côté ; vérifiez la machine et sa connexion.".into(),
                after_secs: rules.node_offline.after_secs,
            });
        }
        if rules.port_closed.enabled {
            out.push(Cond {
                key: format!("port_closed:{id}"),
                active: n.state == "Online" && d.is_some_and(|d| matches!(d.network, Assessment { incoming: "closed", .. })),
                severity: Severity::Warning,
                title: format!("Port d'écoute probablement fermé sur « {name} »"),
                text: "Aucune connexion entrante observée alors que des pairs sont connectés : redirection de port ou pare-feu à vérifier. Le partage est nettement moins efficace.".into(),
                after_secs: rules.port_closed.after_secs,
            });
        }
        if rules.disk_low.enabled {
            let thr = rules.disk_low.threshold_gb.saturating_mul(1_000_000_000);
            let free = d.map_or(0, |d| d.disk_free);
            out.push(Cond {
                key: format!("disk_low:{id}"),
                // Espace inconnu (0) : on ne conclut pas.
                active: n.state == "Online" && free > 0 && free < thr,
                severity: Severity::Warning,
                title: format!("Espace disque faible sur « {name} »"),
                text: format!(
                    "Il reste environ {} Go libres (seuil : {} Go). Libérez de la place avant que les téléchargements échouent.",
                    free / 1_000_000_000,
                    rules.disk_low.threshold_gb
                ),
                after_secs: rules.disk_low.after_secs,
            });
        }
    }
    if rules.torrent_errors.enabled {
        out.push(Cond {
            key: "torrent_errors".into(),
            active: torrents_in_error > 0,
            severity: Severity::Warning,
            title: format!("{torrents_in_error} torrent{} en erreur", if torrents_in_error > 1 { "s" } else { "" }),
            text: "Des torrents sont en erreur (disque, droits, fichiers manquants…). Ouvrez la page Torrents, filtre « Erreur », pour l'explication de chacun.".into(),
            after_secs: rules.torrent_errors.after_secs,
        });
    }
    out
}

// ---------- Canaux ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Webhook,
    Ntfy,
}

impl ChannelKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(Self::Webhook),
            "ntfy" => Some(Self::Ntfy),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Ntfy => "ntfy",
        }
    }
}

/// URL de canal : http(s) uniquement, longueur bornée. Les identifiants éventuels dans l'URL sont acceptés (ntfy
/// avec authentification) mais ne sont jamais réaffichés.
pub fn parse_channel_url(u: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(u.trim()).map_err(|_| "URL invalide".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || u.len() > 500 {
        return Err("l'URL doit être en http(s) et ne pas dépasser 500 caractères".into());
    }
    Ok(url)
}

/// Indice affichable sans révéler de secret : schéma + hôte, le reste (chemin, token) est masqué.
pub fn url_hint(u: &str) -> String {
    reqwest::Url::parse(u).map_or_else(
        |_| "…".to_owned(),
        |p| {
            let port = p.port().map(|x| format!(":{x}")).unwrap_or_default();
            format!("{}://{}{port}/…", p.scheme(), p.host_str().unwrap_or("?"))
        },
    )
}

fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Envoie un événement. Ne renvoie jamais l'URL dans l'erreur.
pub async fn send(kind: ChannelKind, url: &str, e: &Event) -> Result<(), String> {
    let url = parse_channel_url(url)?;
    let c = client().map_err(|_| "client HTTP indisponible".to_owned())?;
    let req = match kind {
        ChannelKind::Webhook => c.post(url).json(&serde_json::json!({
            "event": "alert",
            "state": if e.firing { "firing" } else { "resolved" },
            "severity": e.severity,
            "key": e.key,
            "title": e.title,
            "text": e.text,
            "timestamp": crate::store::unix_now(),
        })),
        ChannelKind::Ntfy => {
            let prio = match (e.firing, e.severity) {
                (false, _) => "3",
                (true, Severity::Critical) => "5",
                (true, Severity::Warning) => "4",
            };
            let tags = if e.firing {
                "warning"
            } else {
                "white_check_mark"
            };
            let title = if e.firing {
                e.title.clone()
            } else {
                format!("Rétabli : {}", e.title)
            };
            c.post(url)
                .header(
                    "Title",
                    reqwest::header::HeaderValue::from_bytes(title.as_bytes())
                        .map_err(|_| "titre invalide".to_owned())?,
                )
                .header("Priority", prio)
                .header("Tags", tags)
                .body(e.text.clone())
        }
    };
    req.send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map(|_| ())
        .map_err(|e| e.without_url().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(key: &str, active: bool, after: i64) -> Cond {
        Cond {
            key: key.into(),
            active,
            severity: Severity::Critical,
            title: format!("T {key}"),
            text: "x".into(),
            after_secs: after,
        }
    }

    #[test]
    fn an_alert_needs_to_last_then_fires_once_then_resolves() {
        let mut s = AlertState::default();
        // Brève panne (moins que le délai) : aucune alerte, aucun message de rétablissement.
        assert!(s.step(&[cond("a", true, 120)], 1000).is_empty());
        assert!(s.step(&[cond("a", true, 120)], 1060).is_empty());
        assert!(
            s.step(&[cond("a", false, 120)], 1090).is_empty(),
            "retombé avant le délai : rien à dire"
        );
        // Panne durable : une seule alerte, pas de répétition.
        assert!(s.step(&[cond("a", true, 120)], 2000).is_empty());
        let ev = s.step(&[cond("a", true, 120)], 2125);
        assert_eq!(ev.len(), 1);
        assert!(ev[0].firing && ev[0].title == "T a");
        assert!(
            s.step(&[cond("a", true, 120)], 2500).is_empty(),
            "pas de spam"
        );
        // Rétablissement : un message, puis silence.
        let ev = s.step(&[cond("a", false, 120)], 3000);
        assert_eq!(ev.len(), 1);
        assert!(!ev[0].firing);
        assert!(s.step(&[cond("a", false, 120)], 3100).is_empty());
        // Une nouvelle panne repart du délai complet (le battement du timer est remis à zéro).
        assert!(s.step(&[cond("a", true, 120)], 4000).is_empty());
        assert!(!s.step(&[cond("a", true, 120)], 4121).is_empty());
    }

    #[test]
    fn disappearing_conditions_are_resolved_not_forgotten_silently() {
        let mut s = AlertState::default();
        s.step(&[cond("a", true, 0), cond("b", true, 0)], 0);
        let ev = s.step(&[cond("a", true, 0), cond("b", true, 0)], 1);
        assert!(ev.is_empty(), "déjà notifiées au premier passage");
        // Le nœud « b » est révoqué : sa surveillance s'arrête, on le dit une fois.
        let ev = s.step(&[cond("a", true, 0)], 2);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].key, "b");
        assert!(!ev[0].firing);
        assert!(s.step(&[cond("a", true, 0)], 3).is_empty());
    }

    #[test]
    fn rules_are_validated() {
        let mut r = Rules::default();
        assert!(r.validate().is_ok());
        r.node_offline.after_secs = 5;
        assert!(r.validate().is_err());
        let mut r = Rules::default();
        r.disk_low.threshold_gb = 0;
        assert!(r.validate().is_err());
    }

    fn node(id: &str, state: &str) -> NodeDetail {
        NodeDetail {
            id: id.into(),
            name: Some(format!("nas-{id}")),
            hostname: "h".into(),
            agent_version: "v".into(),
            labels: vec![],
            state: state.into(),
            created_at: 0,
            last_seen: None,
            offline_since: None,
            desired_rev: 0,
            location: None,
            last_ip: None,
            limits_managed: false,
            dl_limit: 0,
            ul_limit: 0,
        }
    }

    fn digest(free: u64, port: &'static str) -> NodeDigest {
        let mut d = NodeDigest::empty();
        d.disk_free = free;
        d.network.incoming = port;
        d
    }

    #[test]
    fn conditions_reflect_the_cluster_and_never_alert_on_unknowns() {
        let nodes = [
            node("1", "Online"),
            node("2", "Offline"),
            node("3", "Pending"),
            node("4", "Revoked"),
            node("5", "Online"),
        ];
        let mut dg = HashMap::new();
        dg.insert("1".to_owned(), digest(10_000_000_000, "closed")); // 10 Go libres, port fermé
        dg.insert("5".to_owned(), digest(0, "unknown")); // espace inconnu
        let rules = Rules {
            torrent_errors: Rule {
                enabled: true,
                after_secs: 60,
            },
            ..Rules::default()
        };
        let c = conditions(&rules, &nodes, &dg, 3);
        let active: Vec<&str> = c
            .iter()
            .filter(|c| c.active)
            .map(|c| c.key.as_str())
            .collect();
        assert!(
            active.contains(&"node_offline:2")
                && active.contains(&"port_closed:1")
                && active.contains(&"disk_low:1")
                && active.contains(&"torrent_errors")
        );
        assert!(
            !active.contains(&"node_offline:3"),
            "jamais connecté : pas une panne"
        );
        assert!(
            !active.iter().any(|k| k.ends_with(":4")),
            "révoqué : ignoré"
        );
        assert!(
            !active.contains(&"disk_low:5"),
            "espace inconnu : pas de conclusion"
        );
        assert!(c
            .iter()
            .any(|c| c.key == "torrent_errors" && c.title == "3 torrents en erreur"));
        // Règle désactivée : plus aucune condition de ce type.
        let off = Rules {
            node_offline: Rule {
                enabled: false,
                after_secs: 120,
            },
            ..Rules::default()
        };
        assert!(!conditions(&off, &nodes, &dg, 0)
            .iter()
            .any(|c| c.key.starts_with("node_offline")));
    }

    #[test]
    fn channel_urls_are_checked_and_hints_never_leak_the_secret() {
        assert!(parse_channel_url("https://ntfy.sh/mon-topic-secret").is_ok());
        assert!(parse_channel_url("http://192.168.1.5:8080/hook").is_ok());
        for bad in [
            "ftp://x.org/a",
            "javascript:alert(1)",
            "pas une url",
            "file:///etc/passwd",
            "",
        ] {
            assert!(parse_channel_url(bad).is_err(), "{bad}");
        }
        let h = url_hint("https://ntfy.sh/mon-topic-secret?auth=TOKEN");
        assert_eq!(h, "https://ntfy.sh/…");
        assert!(!h.contains("secret") && !h.contains("TOKEN"));
        assert_eq!(ChannelKind::parse("ntfy"), Some(ChannelKind::Ntfy));
        assert_eq!(ChannelKind::parse("sms"), None);
    }
}
