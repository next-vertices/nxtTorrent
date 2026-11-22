//! Génération du script d'installation d'un agent (Linux + systemd).
//!
//! Le script est un gabarit statique (`assets/install-agent.sh`) dans lequel on injecte des valeurs fournies par
//! l'administrateur. Chaque valeur est **validée par liste blanche de caractères** puis placée entre apostrophes :
//! aucune valeur ne peut sortir de sa chaîne ni injecter une commande dans le script (qui s'exécute en root).
use serde::Deserialize;

const TEMPLATE: &str = include_str!("../../assets/install-agent.sh");
const UNIT: &str = include_str!("../../../deploy/systemd/nxt-agent.service");

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    /// `host:port` du listener des agents, tel que la machine à installer le joint.
    pub master_addr: String,
    /// qbittorrent | transmission | deluge | rutorrent
    pub client: String,
    /// Interface du client : obligatoirement sur la boucle locale de la machine à installer.
    pub client_url: String,
    #[serde(default)]
    pub client_user: String,
    /// Dossier racine des téléchargements (chemin absolu sur la machine à installer).
    pub root: String,
}

pub struct Rendered {
    pub script: String,
}

fn ok_chars(s: &str, extra: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// `host:port` : lettres, chiffres, `.-` pour l'hôte, `[::1]` pour IPv6, port 1-65535 obligatoire.
pub fn valid_master_addr(a: &str) -> bool {
    let Some((host, port)) = a.rsplit_once(':') else {
        return false;
    };
    let host_ok = if let Some(h) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !h.is_empty() && ok_chars(h, ":.")
    } else {
        !host.is_empty()
            && host.len() <= 253
            && ok_chars(host, ".-")
            && !host.starts_with(['.', '-'])
    };
    host_ok && port.parse::<u16>().is_ok_and(|p| p > 0)
}

/// URL de l'API du client : http(s) sur 127.0.0.1 / localhost / [::1], avec port ; rien d'autre.
pub fn valid_client_url(u: &str) -> bool {
    let Some(rest) = u
        .strip_prefix("http://")
        .or_else(|| u.strip_prefix("https://"))
    else {
        return false;
    };
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let local = ["127.0.0.1:", "localhost:", "[::1]:"]
        .iter()
        .find_map(|p| hostport.strip_prefix(p));
    local.is_some_and(|p| p.parse::<u16>().is_ok_and(|p| p > 0)) && ok_chars(path, "/-_.")
}

/// Chemin absolu sans `..`, sans apostrophe ni caractère de contrôle ou d'interprétation par le shell.
pub fn valid_root(p: &str) -> bool {
    p.starts_with('/')
        && p.len() <= 200
        && ok_chars(p, "/._- ")
        && !p.split('/').any(|seg| seg == "..")
}

fn valid_user(u: &str) -> bool {
    u.len() <= 64 && ok_chars(u, "._@-")
}

pub fn valid_client(c: &str) -> bool {
    matches!(c, "qbittorrent" | "transmission" | "deluge" | "rutorrent")
}

/// Architectures pour lesquelles un binaire d'agent peut être publié (noms utilisés dans `{arch}`).
pub const ARCHS: [&str; 2] = ["amd64", "arm64"];

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Binaire à télécharger : URL https stricte + empreinte(s) SHA-256, ou rien. Deux formes :
/// - **une architecture** : URL sans `{arch}` + une empreinte (64 hexadécimaux minuscules) ;
/// - **plusieurs** : URL contenant `{arch}` (remplacé par `amd64` ou `arm64` selon la machine) + une empreinte PAR
///   architecture, `amd64=<sha>,arm64=<sha>` (un parc mixte, par exemple des VPS et des Raspberry Pi).
pub fn valid_binary(url: &str, sha: &str) -> bool {
    if url.is_empty() && sha.is_empty() {
        return true;
    }
    if !(url.starts_with("https://") && url.len() <= 500 && ok_chars(url, ":/.-_~%?=&+{}")) {
        return false;
    }
    // `{arch}` est le seul gabarit permis : toute autre accolade est refusée.
    let per_arch = url.contains("{arch}");
    if url.replace("{arch}", "").contains(['{', '}']) {
        return false;
    }
    if !per_arch {
        return is_sha256(sha);
    }
    let mut seen: Vec<&str> = Vec::new();
    for e in sha.split(',') {
        match e.split_once('=') {
            Some((a, h)) if ARCHS.contains(&a) && is_sha256(h) && !seen.contains(&a) => {
                seen.push(a)
            }
            _ => return false,
        }
    }
    !seen.is_empty()
}

/// `code` est le code d'appairage déjà créé (secret, 15 min). Renvoie une erreur lisible si une valeur est refusée.
pub fn render(
    req: &InstallRequest,
    code: &str,
    bin_url: &str,
    bin_sha256: &str,
) -> Result<Rendered, String> {
    if !valid_master_addr(&req.master_addr) {
        return Err(
            "adresse du master invalide (attendu : hôte:port, ex. master.exemple.org:7443)".into(),
        );
    }
    if !valid_client(&req.client) {
        return Err("client inconnu (qbittorrent, transmission, deluge ou rutorrent)".into());
    }
    if !valid_client_url(&req.client_url) {
        return Err(
            "URL du client invalide : elle doit viser la machine elle-même (http://127.0.0.1:PORT)"
                .into(),
        );
    }
    if !valid_user(&req.client_user) {
        return Err("utilisateur du client invalide (lettres, chiffres, . _ @ -)".into());
    }
    if !valid_root(&req.root) {
        return Err(
            "dossier invalide : chemin absolu, sans « .. » (lettres, chiffres, / . _ - espace)"
                .into(),
        );
    }
    if !code.starts_with("CODE-") || !ok_chars(code, "-") {
        return Err("code d'appairage inattendu".into());
    }
    if !valid_binary(bin_url, bin_sha256) {
        return Err("configuration du binaire invalide (https + SHA-256 en minuscules)".into());
    }
    let q = |s: &str| s.to_owned(); // valeurs déjà limitées à des caractères sans apostrophe
    let script = TEMPLATE
        .replace("@@MASTER_ADDR@@", &q(&req.master_addr))
        .replace("@@CODE@@", &q(code))
        .replace("@@CLIENT@@", &q(&req.client))
        .replace("@@CLIENT_URL@@", &q(&req.client_url))
        .replace("@@CLIENT_USER@@", &q(&req.client_user))
        .replace("@@ROOT@@", &q(&req.root))
        .replace("@@BIN_URL@@", &q(bin_url))
        .replace("@@BIN_SHA256@@", &q(bin_sha256))
        .replace("@@UNIT@@", UNIT.trim_end());
    Ok(Rendered { script })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> InstallRequest {
        InstallRequest {
            master_addr: "master.exemple.org:7443".into(),
            client: "transmission".into(),
            client_url: "http://127.0.0.1:9091".into(),
            client_user: "admin".into(),
            root: "/srv/downloads".into(),
        }
    }

    #[test]
    fn binary_download_accepts_one_architecture_or_a_per_architecture_map_and_nothing_else() {
        let h = "a".repeat(64);
        let u = "https://exemple.org/releases/v1/nxt-agent-linux-amd64";
        let ua = "https://exemple.org/releases/v1/nxt-agent-linux-{arch}";
        assert!(valid_binary("", ""));
        assert!(valid_binary(u, &h), "une seule architecture");
        assert!(
            valid_binary(ua, &format!("amd64={h},arm64={}", "b".repeat(64))),
            "parc mixte"
        );
        assert!(
            valid_binary(ua, &format!("arm64={h}")),
            "une seule architecture dans la liste"
        );
        for (url, sha) in [
            (u, ""),                                    // URL sans empreinte
            (ua, &h),                                   // gabarit sans table d'empreintes
            (u, &format!("amd64={h}")),                 // table sans gabarit
            (ua, &format!("amd64={h},amd64={h}")),      // doublon
            (ua, &format!("riscv={h}")),                // architecture inconnue
            (ua, &format!("amd64={}", "A".repeat(64))), // majuscules
            (ua, &format!("amd64={}", "a".repeat(63))), // trop court
            (ua, &format!("amd64={h},")),               // entrée vide
            (ua, ""),
            ("http://exemple.org/x-{arch}", &format!("amd64={h}")), // pas https
            ("https://exemple.org/{os}-{arch}", &format!("amd64={h}")), // autre gabarit
            ("https://exemple.org/x'{arch}", &format!("amd64={h}")), // apostrophe : injection dans le script
        ] {
            assert!(!valid_binary(url, sha), "devrait être refusé : {url} {sha}");
        }
    }

    #[test]
    fn the_per_architecture_table_is_rendered_into_the_script_as_a_safe_literal() {
        let h = "c".repeat(64);
        let sha = format!("amd64={h},arm64={h}");
        let r = render(
            &req(),
            "CODE-ABCD",
            "https://exemple.org/nxt-agent-linux-{arch}",
            &sha,
        )
        .unwrap();
        assert!(r
            .script
            .contains("BIN_URL='https://exemple.org/nxt-agent-linux-{arch}'"));
        assert!(r.script.contains(&format!("BIN_SHA256='{sha}'")));
        assert!(!r.script.contains("@@"));
    }

    #[test]
    fn renders_a_complete_script_without_leftover_placeholders() {
        let r = render(&req(), "CODE-ABC-DEF", "", "").unwrap();
        assert!(!r.script.contains("@@"), "gabarit complet");
        for must in [
            "MASTER='master.exemple.org:7443'",
            "CODE='CODE-ABC-DEF'",
            "CLIENT='transmission'",
            "ROOT='/srv/downloads'",
            "systemctl enable --now nxt-agent",
            "NoNewPrivileges=yes",
        ] {
            assert!(r.script.contains(must), "{must}");
        }
        assert!(r.script.starts_with("#!/bin/sh"));
    }

    #[test]
    fn every_value_is_allow_listed_so_nothing_can_inject_a_command() {
        let evil = [
            "'; rm -rf / #",
            "a b",
            "x$(id)",
            "x`id`",
            "x;y",
            "x\ny",
            "x&y",
            "x|y",
            "x\"y",
            "x\\y",
            "x'y",
            "",
        ];
        for e in evil {
            let mut r = req();
            r.master_addr = format!("{e}:7443");
            assert!(render(&r, "CODE-A", "", "").is_err(), "master_addr {e:?}");
            let mut r = req();
            r.client_url = format!("http://127.0.0.1:9091/{e}");
            // Chemin vide = simple « / » final : inoffensif ; tout le reste doit être refusé.
            assert!(
                e.is_empty() || render(&r, "CODE-A", "", "").is_err(),
                "client_url {e:?}"
            );
            let mut r = req();
            r.client_user = e.to_owned();
            if !e.is_empty() {
                assert!(render(&r, "CODE-A", "", "").is_err(), "user {e:?}");
            }
            let mut r = req();
            r.root = format!("/srv/{e}");
            if !e.is_empty() && e != "a b" {
                assert!(render(&r, "CODE-A", "", "").is_err(), "root {e:?}");
            }
            let mut r = req();
            r.client = e.to_owned();
            assert!(render(&r, "CODE-A", "", "").is_err(), "client {e:?}");
            assert!(
                render(&req(), &format!("CODE-{e}"), "", "").is_err() || e.is_empty(),
                "code {e:?}"
            );
        }
        // Règles métier.
        let mut r = req();
        r.root = "/srv/../etc".into();
        assert!(render(&r, "CODE-A", "", "").is_err(), "traversée");
        r.root = "relatif".into();
        assert!(render(&r, "CODE-A", "", "").is_err(), "chemin relatif");
        let mut r = req();
        r.client_url = "http://192.168.1.10:9091".into();
        assert!(
            render(&r, "CODE-A", "", "").is_err(),
            "l'API du client reste sur la boucle locale"
        );
        r.client_url = "http://127.0.0.1.evil.example:9091".into();
        assert!(render(&r, "CODE-A", "", "").is_err());
        let mut r = req();
        r.master_addr = "master.exemple.org".into();
        assert!(render(&r, "CODE-A", "", "").is_err(), "port obligatoire");
        assert!(valid_master_addr("[::1]:7443") && valid_master_addr("10.0.0.5:7443"));
    }

    #[test]
    fn binary_download_requires_https_and_a_sha256() {
        let sha = "a".repeat(64);
        assert!(render(&req(), "CODE-A", "https://exemple.org/nxt-agent", &sha).is_ok());
        for (u, s) in [
            ("http://exemple.org/a", sha.as_str()),
            ("https://exemple.org/a", ""),
            ("https://exemple.org/a", "xyz"),
            ("https://exemple.org/a'b", sha.as_str()),
            ("", sha.as_str()),
        ] {
            assert!(render(&req(), "CODE-A", u, s).is_err(), "{u} {s}");
        }
    }

    #[test]
    fn generated_script_is_valid_shell_syntax() {
        // `sh -n` analyse sans exécuter. Ignoré si aucun `sh` n'est disponible (ex. Windows sans Git Bash).
        let Ok(sh) = which_sh() else { return };
        let r = render(&req(), "CODE-ABC-DEF", "", "").unwrap();
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), r.script).unwrap();
        let out = std::process::Command::new(sh)
            .arg("-n")
            .arg(f.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn which_sh() -> Result<String, ()> {
        for c in ["sh", "C:/Program Files/Git/usr/bin/sh.exe"] {
            if std::process::Command::new(c)
                .arg("-c")
                .arg("true")
                .output()
                .is_ok_and(|o| o.status.success())
            {
                return Ok(c.to_owned());
            }
        }
        Err(())
    }
}
