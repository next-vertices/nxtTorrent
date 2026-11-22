//! Traduit une entrée brute du journal d'audit en phrase lisible : qui a fait quoi, sur quoi, avec quelle
//! gravité. Le journal reste stocké sous forme brute (stable, filtrable) ; la lecture humaine est calculée
//! à la demande, en remplaçant les identifiants par les noms actuels des nœuds et des torrents.
use crate::store::AuditRow;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Serialize)]
pub struct Described {
    /// `access` | `nodes` | `torrents` | `security` | `system`
    pub category: &'static str,
    pub severity: Severity,
    /// Phrase courte, complète, destinée à un humain.
    pub title: String,
    /// Précision complémentaire (adresse IP, motif…), vide si inutile.
    pub detail: String,
    /// `admin` | `node` | `system` | `anonymous`
    pub actor_kind: &'static str,
    pub actor_label: String,
}

/// Noms courants pour remplacer les identifiants.
#[derive(Default)]
pub struct Names {
    pub nodes: HashMap<String, String>,
    pub torrents: HashMap<String, String>,
}

impl Names {
    fn node(&self, id: &str) -> String {
        match self.nodes.get(id) {
            Some(n) => format!("« {n} »"),
            None if id.len() > 8 => format!("{}…", id.chars().take(8).collect::<String>()),
            None => id.to_owned(),
        }
    }

    fn torrent(&self, ih: &str) -> String {
        match self.torrents.get(ih).filter(|n| !n.is_empty()) {
            Some(n) => format!("« {} »", shorten(n, 70)),
            None => format!("{}…", ih.chars().take(10).collect::<String>()),
        }
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Reçoit `user=<nom>` ou `<ip> user=<nom>` (format écrit par l'API de connexion).
fn split_login(detail: &str) -> (String, String) {
    let mut ip = String::new();
    let mut user = String::new();
    for p in detail.split_whitespace() {
        match p.strip_prefix("user=") {
            Some(u) => user = u.to_owned(),
            None => ip = p.to_owned(),
        }
    }
    (ip, user)
}

fn from_ip(ip: &str) -> String {
    if ip.is_empty() {
        String::new()
    } else {
        format!("depuis {ip}")
    }
}

/// Durée de pause lisible : « 5 min », « 2 h »…
fn pause_label(secs: i64) -> String {
    match secs {
        0..=89 => format!("{secs} s"),
        90..=5399 => format!("{} min", (secs + 30) / 60),
        _ => format!("{} h", (secs + 1800) / 3600),
    }
}

pub fn describe(r: &AuditRow, names: &Names) -> Described {
    use Severity::{Critical, Info, Warning};
    let (actor_kind, actor_label) = if let Some(u) = r.actor.strip_prefix("admin:") {
        ("admin", u.to_owned())
    } else if r.actor == "admin" {
        ("admin", "administrateur".to_owned())
    } else if r.actor == "anonymous" {
        ("anonymous", "Inconnu (non authentifié)".to_owned())
    } else if r.actor == "system" {
        ("system", "Système".to_owned())
    } else if r.actor == "local-cli" {
        ("system", "Ligne de commande locale".to_owned())
    } else if names.nodes.contains_key(&r.actor) || r.actor.len() == 36 {
        (
            "node",
            names
                .node(&r.actor)
                .trim_matches(['«', '»', ' '])
                .to_owned(),
        )
    } else {
        ("system", r.actor.clone())
    };
    // L'appairage est déclenché par le futur nœud lui-même (anonyme jusqu'à ce moment) : on l'attribue au nœud.
    let (actor_kind, actor_label) = if r.action == "node_paired" {
        let id = r.detail.split_whitespace().next().unwrap_or("");
        (
            "node",
            names.node(id).trim_matches(['«', '»', ' ']).to_owned(),
        )
    } else {
        (actor_kind, actor_label)
    };
    let d = r.detail.as_str();
    let first = d.split_whitespace().next().unwrap_or("");
    let last = d.split_whitespace().last().unwrap_or("");

    let (category, severity, title, detail): (&'static str, Severity, String, String) =
        match r.action.as_str() {
            "admin_created" => (
                "access",
                Info,
                "Compte administrateur créé".into(),
                from_ip(d),
            ),
            "admin_login" => ("access", Info, "Connexion à l'interface".into(), from_ip(d)),
            "admin_logout" => ("access", Info, "Déconnexion".into(), String::new()),
            "admin_login_failed" => {
                let (ip, user) = split_login(d);
                (
                    "security",
                    Warning,
                    format!("Échec de connexion pour le compte « {user} »"),
                    from_ip(&ip),
                )
            }
            "admin_login_blocked" => {
                let (ip, user) = split_login(d);
                (
                    "security",
                    Critical,
                    format!("Connexions bloquées après trop d'échecs (compte « {user} »)"),
                    from_ip(&ip),
                )
            }
            "admin_login_password_ok" => {
                let (ip, user) = split_login(d);
                (
                    "access",
                    Info,
                    format!("Mot de passe validé pour « {user} » : code de double authentification demandé"),
                    from_ip(&ip),
                )
            }
            "admin_login_totp_failed" => {
                let (ip, user) = split_login(d);
                (
                    "security",
                    Warning,
                    format!("Code de double authentification refusé pour le compte « {user} »"),
                    from_ip(&ip),
                )
            }
            "admin_recovery_code_used" => (
                "security",
                Warning,
                "Connexion avec un code de secours".into(),
                format!("{d} code(s) de secours restant(s)"),
            ),
            "agent_install_created" => {
                let (name, labels) = d.split_once('|').unwrap_or(("", d));
                let mut parts = Vec::new();
                if !name.is_empty() {
                    parts.push(format!("nom prévu « {name} »"));
                }
                if !labels.is_empty() {
                    parts.push(format!("labels {labels}"));
                }
                (
                    "nodes",
                    Info,
                    "Script d'installation d'un agent généré".into(),
                    parts.join(", "),
                )
            }
            "alert_fired" => {
                let title = d.split_once('|').map_or(d, |(_, t)| t);
                ("system", Warning, format!("Alerte : {title}"), String::new())
            }
            "alert_resolved" => {
                let title = d.split_once('|').map_or(d, |(_, t)| t);
                ("system", Info, format!("Rétabli : {title}"), String::new())
            }
            "alert_channel_added" => (
                "system",
                Info,
                "Canal de notification ajouté".into(),
                d.to_owned(),
            ),
            "alert_channel_removed" => (
                "system",
                Info,
                "Canal de notification supprimé".into(),
                d.to_owned(),
            ),
            "alert_rules_changed" => (
                "system",
                Info,
                "Règles d'alerte modifiées".into(),
                String::new(),
            ),
            "node_limits_changed" => {
                let mut p = d.splitn(3, '|');
                let id = p.next().unwrap_or("");
                let rate = |s: &str| {
                    let n: u64 = s.parse().unwrap_or(0);
                    if n == 0 {
                        "illimité".to_owned()
                    } else {
                        format!("{} Mo/s", n / 1_000_000)
                    }
                };
                let (dl, ul) = (rate(p.next().unwrap_or("0")), rate(p.next().unwrap_or("0")));
                (
                    "nodes",
                    Info,
                    format!("Limites de débit du nœud {} modifiées", names.node(id)),
                    format!("↓ {dl} · ↑ {ul}"),
                )
            }
            "torrent_rechecked" => (
                "torrents",
                Info,
                format!("Vérification des fichiers demandée pour {}", names.torrent(d)),
                String::new(),
            ),
            "backup_created" => (
                "security",
                Info,
                "Sauvegarde chiffrée téléchargée".into(),
                String::new(),
            ),
            "backup_refused" => (
                "security",
                Warning,
                "Téléchargement de sauvegarde refusé (mot de passe ou code incorrect)".into(),
                String::new(),
            ),
            "totp_enabled" => (
                "access",
                Info,
                "Double authentification activée".into(),
                String::new(),
            ),
            "totp_disabled" => (
                "security",
                Warning,
                "Double authentification désactivée".into(),
                String::new(),
            ),
            "totp_disable_failed" => (
                "security",
                Warning,
                "Tentative de désactivation de la double authentification refusée".into(),
                String::new(),
            ),
            "admin_password_changed" => {
                ("access", Info, "Mot de passe modifié".into(), String::new())
            }
            "admin_password_change_failed" => (
                "security",
                Warning,
                "Tentative de changement de mot de passe refusée (mot de passe actuel incorrect)"
                    .into(),
                String::new(),
            ),
            "admin_password_set" => (
                "access",
                Info,
                format!("Mot de passe de « {d} » défini en ligne de commande"),
                String::new(),
            ),
            "pairing_code_created" => {
                // Nouveau format `nom|labels` ; les anciennes entrées ne contiennent que les labels.
                let (name, labels) = d.split_once('|').unwrap_or(("", d));
                let mut parts = Vec::new();
                if !name.is_empty() {
                    parts.push(format!("nom prévu « {name} »"));
                }
                if !labels.is_empty() {
                    parts.push(format!("labels {labels}"));
                }
                (
                    "nodes",
                    Info,
                    "Code d'appairage créé".into(),
                    parts.join(", "),
                )
            }
            "node_paired" => (
                "nodes",
                Info,
                format!("Nœud {} appairé", names.node(first)),
                from_ip(last.rsplit_once(':').map_or(last, |(h, _)| h)),
            ),
            "node_renamed" => {
                let mut p = d.splitn(3, '|');
                let (_, old, new) = (p.next(), p.next().unwrap_or(""), p.next().unwrap_or(""));
                (
                    "nodes",
                    Info,
                    format!("Nœud renommé : « {old} » → « {new} »"),
                    String::new(),
                )
            }
            "node_labels_changed" => {
                let mut p = d.splitn(3, '|');
                let (id, old, new) = (
                    p.next().unwrap_or(""),
                    p.next().unwrap_or(""),
                    p.next().unwrap_or(""),
                );
                let show = |s: &str| {
                    if s.is_empty() {
                        "aucun".to_owned()
                    } else {
                        s.replace(',', ", ")
                    }
                };
                (
                    "nodes",
                    Info,
                    format!("Labels du nœud {} modifiés", names.node(id)),
                    format!("{} → {}", show(old), show(new)),
                )
            }
            "torrent_placement_changed" => {
                let mut p = d.splitn(5, '|');
                let ih = p.next().unwrap_or("");
                let (or, nr) = (p.next().unwrap_or(""), p.next().unwrap_or(""));
                let (og, ng) = (p.next().unwrap_or(""), p.next().unwrap_or(""));
                let show = |s: &str| {
                    if s.is_empty() {
                        "aucun".to_owned()
                    } else {
                        s.replace(',', ", ")
                    }
                };
                (
                    "torrents",
                    Info,
                    format!("Placement du torrent {} modifié", names.torrent(ih)),
                    format!(
                        "labels requis : {} → {} ; réplication : {} → {}",
                        show(or),
                        show(nr),
                        show(og),
                        show(ng)
                    ),
                )
            }
            "node_location_changed" => {
                let (id, loc) = d.split_once('|').unwrap_or((d, ""));
                (
                    "nodes",
                    Info,
                    format!("Localisation du nœud {} modifiée", names.node(id)),
                    if loc.is_empty() {
                        "effacée".to_owned()
                    } else {
                        format!("« {loc} »")
                    },
                )
            }
            "node_revoked" => (
                "nodes",
                Warning,
                format!("Nœud {} révoqué", names.node(d)),
                "il ne peut plus se reconnecter".into(),
            ),
            "node_state" => {
                let to = d.rsplit("->").next().unwrap_or("").trim();
                let (verb, sev) = match to {
                    "Online" => ("est en ligne", Info),
                    "Offline" => ("est hors ligne", Warning),
                    "Revoked" => ("a été révoqué", Warning),
                    other => (other, Info),
                };
                (
                    "nodes",
                    sev,
                    format!("Le nœud {} {verb}", names.node(first)),
                    String::new(),
                )
            }
            "torrent_added" => (
                "torrents",
                Info,
                format!("Torrent {} ajouté", names.torrent(d)),
                String::new(),
            ),
            "torrent_removed" => (
                "torrents",
                Info,
                format!("Torrent {} supprimé", names.torrent(first)),
                if d.contains("delete_files") {
                    "fichiers supprimés".into()
                } else {
                    String::new()
                },
            ),
            "torrent_paused" => (
                "torrents",
                Info,
                format!("Torrent {} mis en pause", names.torrent(d)),
                String::new(),
            ),
            "torrent_resumed" => (
                "torrents",
                Info,
                format!("Torrent {} relancé", names.torrent(d)),
                String::new(),
            ),
            "torrent_completed" => (
                "torrents",
                Info,
                format!("Torrent {} terminé", names.torrent(d)),
                format!("sur {}", names.node(&r.actor)),
            ),
            "torrent_assigned" => (
                "torrents",
                Info,
                format!(
                    "Torrent {} assigné au nœud {}",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "torrent_reassigned" => (
                "torrents",
                Warning,
                format!(
                    "Torrent {} réassigné au nœud {} (l'ancien nœud est resté hors ligne trop longtemps)",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "replica_assigned" => (
                "torrents",
                Info,
                format!(
                    "Copie de {} placée sur le nœud {}",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "replica_transfer" => (
                "torrents",
                Info,
                format!(
                    "Copie de {} vers le nœud {} par le protocole BitTorrent (sans stockage partagé)",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "replica_phase" => {
                let mut w = d.split_whitespace().skip(1);
                let node = names.node(w.next().unwrap_or(""));
                let what = match w.next() {
                    Some("Retarget") => "transfert terminé, retour au torrent d'origine en cours",
                    Some("Active") => "copie vérifiée, partage sur le torrent d'origine",
                    _ => "étape suivante",
                };
                (
                    "torrents",
                    Info,
                    format!("Copie de {} sur le nœud {node} : {what}", names.torrent(first)),
                    String::new(),
                )
            }
            "metainfo_received" => (
                "torrents",
                Info,
                format!(
                    "Le nœud {} a fourni le .torrent de {} : la copie entre nœuds est possible",
                    names.node(&r.actor),
                    names.torrent(first)
                ),
                String::new(),
            ),
            "metainfo_refused" => (
                "security",
                Warning,
                "Un nœud a envoyé un .torrent non demandé ou invalide".into(),
                String::new(),
            ),
            "replication_limit_changed" => (
                "torrents",
                Info,
                match d.parse::<u64>() {
                    Ok(0) => "Plafond de débit des copies entre nœuds retiré".to_owned(),
                    Ok(v) => format!("Plafond de débit des copies entre nœuds : {} Mo/s", v / 1_000_000),
                    Err(_) => "Plafond de débit des copies modifié".to_owned(),
                },
                String::new(),
            ),
            "torrent_create_started" => (
                "torrents",
                Info,
                format!("Création d'un .torrent lancée sur le nœud {}", names.node(d)),
                String::new(),
            ),
            "torrent_created" => (
                "torrents",
                Info,
                format!(
                    "Torrent {} créé à partir de fichiers du nœud {}",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "xseed_mode_changed" => (
                "torrents",
                Info,
                match d {
                    "off" => "Cross-seed désactivé".to_owned(),
                    "dry" => "Cross-seed en simulation (rien n'est ajouté)".to_owned(),
                    "active" => "Cross-seed activé".to_owned(),
                    _ => "Mode du cross-seed modifié".to_owned(),
                },
                String::new(),
            ),
            "xseed_run" => (
                "torrents",
                Info,
                "Recherche cross-seed lancée manuellement".into(),
                String::new(),
            ),
            "xseed_indexer_added" => (
                "system",
                Info,
                format!("Indexeur « {} » ajouté au cross-seed", shorten(d, 60)),
                String::new(),
            ),
            "xseed_indexer_enabled" => (
                "system",
                Info,
                if d.ends_with("true") {
                    "Indexeur du cross-seed activé".to_owned()
                } else {
                    "Indexeur du cross-seed désactivé".to_owned()
                },
                String::new(),
            ),
            "xseed_indexer_deleted" => (
                "system",
                Info,
                "Indexeur du cross-seed supprimé".into(),
                String::new(),
            ),
            "xseed_indexer_limits" => {
                let p: Vec<&str> = d.split('|').collect();
                (
                    "system",
                    Info,
                    "Réglages de prudence d'un indexeur du cross-seed modifiés".into(),
                    match p.as_slice() {
                        [_, ms, h, day, c] => format!(
                            "au moins {} s entre deux requêtes, {h}/heure, {day}/jour, {c} résultats par recherche",
                            ms.parse::<u64>().map_or_else(|_| ms.to_string(), |v| format!("{:.1}", v as f64 / 1000.0))
                        ),
                        _ => String::new(),
                    },
                )
            }
            "xseed_indexer_disabled" => {
                let (name, reason) = d.split_once('|').unwrap_or((d, ""));
                (
                    "security",
                    Warning,
                    format!("Indexeur « {} » désactivé automatiquement", shorten(name, 60)),
                    shorten(reason, 200),
                )
            }
            "xseed_indexer_paused" => {
                let mut p = d.splitn(3, '|');
                let name = p.next().unwrap_or("");
                let until: i64 = p.next().and_then(|u| u.parse().ok()).unwrap_or(0);
                let why = p.next().unwrap_or("");
                (
                    "system",
                    Warning,
                    format!("Indexeur « {} » mis en pause : plus aucune requête pendant {}", shorten(name, 60), pause_label((until - r.ts).max(0))),
                    shorten(why, 200),
                )
            }
            "xseed_indexer_error" => (
                "system",
                Warning,
                format!(
                    "L'indexeur « {} » n'a pas répondu correctement",
                    shorten(d.split('|').next().unwrap_or(""), 60)
                ),
                shorten(d.split_once('|').map_or("", |x| x.1), 140),
            ),
            "xseed_added" => {
                let mut w = d.split_whitespace();
                let cand = w.next().unwrap_or("");
                let _ = w.next();
                (
                    "torrents",
                    Info,
                    format!(
                        "Cross-seed : {} ajouté sur le nœud {} (mêmes fichiers, autre tracker)",
                        names.torrent(cand),
                        names.node(w.next().unwrap_or(""))
                    ),
                    String::new(),
                )
            }
            "xseed_linked" => (
                "torrents",
                Info,
                format!(
                    "Cross-seed : liens physiques créés pour {} sur le nœud {} (aucune copie)",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "xseed_link_failed" => (
                "torrents",
                Warning,
                format!(
                    "Cross-seed : {} abandonné sur le nœud {} (liens physiques impossibles)",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "xseed_seeding" => (
                "torrents",
                Info,
                format!(
                    "Cross-seed : {} vérifié et partagé sur le nœud {}",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "xseed_removed" => (
                "torrents",
                Warning,
                format!(
                    "Cross-seed : {} retiré du nœud {} (contenu incomplet, fichiers conservés)",
                    names.torrent(first),
                    names.node(last)
                ),
                String::new(),
            ),
            "tracker_added" => (
                "system",
                Info,
                format!(
                    "Le nœud {} annonce {} au tracker de réplication",
                    names.node(last),
                    names.torrent(first)
                ),
                String::new(),
            ),
            "command_sent" => (
                "system",
                Info,
                format!("Commande envoyée au nœud {}", names.node(d)),
                String::new(),
            ),
            "command_refused" => (
                "security",
                Warning,
                "Un nœud a refusé une commande".into(),
                shorten(d, 140),
            ),
            "cert_renewed" => (
                "system",
                Info,
                format!("Certificat du nœud {} renouvelé", names.node(&r.actor)),
                String::new(),
            ),
            "renew_refused" => (
                "security",
                Warning,
                format!("Renouvellement de certificat refusé au nœud {}", names.node(&r.actor)),
                d.to_owned(),
            ),
            "mtls_failed" => (
                "security",
                Warning,
                "Connexion TLS rejetée (certificat invalide, expiré ou absent)".into(),
                shorten(d, 140),
            ),
            "mtls_rejected" => (
                "security",
                Warning,
                "Certificat client sans identité de nœud valide".into(),
                String::new(),
            ),
            "pair_rejected" => (
                "security",
                Warning,
                "Appairage refusé".into(),
                shorten(d, 140),
            ),
            "pair_rate_limited" => (
                "security",
                Critical,
                "Trop de tentatives d'appairage : adresse bloquée temporairement".into(),
                d.to_owned(),
            ),
            "connect_refused" => (
                "security",
                Warning,
                format!("Connexion refusée au nœud {}", names.node(&r.actor)),
                d.to_owned(),
            ),
            "duplicate_session" => (
                "security",
                Critical,
                format!(
                    "Second tunnel avec le même certificat pour le nœud {} : l'ancien a été fermé",
                    names.node(&r.actor)
                ),
                "le certificat est peut-être copié sur une autre machine".into(),
            ),
            "protocol_violation" => (
                "security",
                Critical,
                format!("Message invalide du nœud {}", names.node(&r.actor)),
                shorten(d, 140),
            ),
            other => ("system", Info, other.replace('_', " "), shorten(d, 140)),
        };
    Described {
        category,
        severity,
        title,
        detail,
        actor_kind,
        actor_label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(actor: &str, action: &str, detail: &str) -> AuditRow {
        AuditRow {
            id: 1,
            ts: 0,
            actor: actor.into(),
            action: action.into(),
            detail: detail.into(),
        }
    }

    fn names() -> Names {
        let mut n = Names::default();
        n.nodes.insert("11111111-2222".into(), "nas-salon".into());
        n.torrents.insert("a".repeat(40), "Ubuntu 24.04".into());
        n
    }

    #[test]
    fn replaces_identifiers_by_names() {
        let d = describe(
            &row("admin:alice", "torrent_added", &"a".repeat(40)),
            &names(),
        );
        assert_eq!(d.title, "Torrent « Ubuntu 24.04 » ajouté");
        assert_eq!(d.actor_kind, "admin");
        assert_eq!(d.actor_label, "alice");

        let d = describe(
            &row("system", "node_state", "11111111-2222 Online -> Offline"),
            &names(),
        );
        assert_eq!(d.title, "Le nœud « nas-salon » est hors ligne");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.category, "nodes");
    }

    #[test]
    fn security_events_are_graded() {
        let d = describe(
            &row("anonymous", "admin_login_failed", "203.0.113.9 user=root"),
            &names(),
        );
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.category, "security");
        assert!(d.title.contains("« root »") && d.detail == "depuis 203.0.113.9");
        let d = describe(&row("11111111-2222", "duplicate_session", ""), &names());
        assert_eq!(d.severity, Severity::Critical);
        assert!(d.title.contains("nas-salon"));
        assert_eq!(d.actor_kind, "node");
    }

    #[test]
    fn unknown_ids_are_shortened_and_unknown_actions_still_readable() {
        let d = describe(
            &row("system", "replica_assigned", "ffff ffffffffffffffff"),
            &Names::default(),
        );
        assert!(d.title.starts_with("Copie de ffff…"), "{}", d.title);
        let d = describe(&row("system", "some_new_action", "x"), &Names::default());
        assert_eq!(d.title, "some new action");
    }

    #[test]
    fn pairing_code_old_and_new_formats() {
        let d = describe(
            &row("admin:a", "pairing_code_created", "nas|ssd,vpn"),
            &names(),
        );
        assert_eq!(d.detail, "nom prévu « nas », labels ssd,vpn");
        let d = describe(&row("admin:a", "pairing_code_created", "ssd"), &names());
        assert_eq!(d.detail, "labels ssd");
    }
}
