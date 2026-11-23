//! Vérification (facultative) des nouvelles versions de nxtTorrent : le master interroge l'API publique des releases
//! GitHub, au plus toutes les 12 heures, et l'interface affiche un bandeau s'il y a plus récent.
//!
//! Il ne télécharge ni n'exécute rien. Garde-fous : adresse FIXE dans le code (aucune option, aucune variable
//! d'environnement : pas de SSRF), jamais activée sans accord de l'administrateur, délai court, redirections
//! limitées, réponse bornée en taille, étiquettes validées caractère par caractère, lien de la release RECONSTRUIT par
//! nous à partir de l'étiquette (jamais repris de la réponse), requête sans aucune donnée sur l'installation
//! (User-Agent générique, sans version). Une panne (réseau fermé, limite GitHub) n'a aucune autre conséquence qu'un
//! message discret : le master fonctionne exactement pareil.
use crate::store::unix_now;
use crate::Core;
use std::cmp::Ordering;
use std::time::Duration;

/// Adresse des releases (liste, les plus récentes d'abord). Constante : voir l'en-tête.
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/next-vertices/nxtTorrent/releases?per_page=30";
/// Page d'une release : le lien affiché est toujours construit ici, jamais lu dans la réponse.
const RELEASE_PAGE: &str = "https://github.com/next-vertices/nxtTorrent/releases/tag/";

/// Intervalle entre deux vérifications réussies.
pub const CHECK_EVERY_SECS: i64 = 12 * 3600;
/// Intervalle après un échec (réseau fermé, limite GitHub…).
pub const RETRY_AFTER_ERROR_SECS: i64 = 3600;
/// Écart minimal entre deux vérifications demandées à la main.
pub const MANUAL_MIN_GAP_SECS: i64 = 60;

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BYTES: usize = 1024 * 1024;
const MAX_RELEASES: usize = 50;
const MAX_TAG: usize = 64;

/// Identifiant de pré-version (`rc`, `3`). Les numériques précèdent les alphanumériques (semver).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Alpha(String),
}

/// Version sémantique (`0.1.0-rc.3`) : suffisant pour ordonner nos propres étiquettes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: (u64, u64, u64),
    pre: Vec<Ident>,
}

impl Version {
    /// `v0.1.0-rc.3` ou `0.1.0-rc.3` ; les métadonnées de build (`+…`) sont ignorées.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => p
                .split('.')
                .map(|x| {
                    if x.is_empty() {
                        None
                    } else if x.bytes().all(|b| b.is_ascii_digit()) {
                        x.parse().ok().map(Ident::Num)
                    } else if x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                        Some(Ident::Alpha(x.to_owned()))
                    } else {
                        None
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            core: (major, minor, patch),
            pre,
        })
    }

    /// Pré-version (`-rc.N`…) ?
    pub fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.core.0, self.core.1, self.core.2)?;
        if self.is_pre() {
            let pre: Vec<String> = self
                .pre
                .iter()
                .map(|i| match i {
                    Ident::Num(n) => n.to_string(),
                    Ident::Alpha(a) => a.clone(),
                })
                .collect();
            write!(f, "-{}", pre.join("."))?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            // Une version finale est plus récente que ses pré-versions (0.1.0 > 0.1.0-rc.9).
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

/// Version de CE master.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version {
        core: (0, 0, 0),
        pre: Vec::new(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub published_at: Option<String>,
    /// Marquée « pre-release » sur GitHub (indépendamment de l'étiquette).
    pub flagged_pre: bool,
}

impl Release {
    pub fn url(&self) -> String {
        format!("{RELEASE_PAGE}{}", self.tag)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("serveur des versions injoignable")]
    Unreachable,
    #[error("limite de requêtes atteinte")]
    RateLimited,
    #[error("réponse HTTP {0}")]
    Http(u16),
    #[error("réponse illisible : {0}")]
    BadResponse(&'static str),
}

impl UpdateError {
    /// Code stable affiché par l'interface (qui le traduit) : jamais de détail réseau.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::RateLimited => "rate_limited",
            Self::Http(_) => "http_error",
            Self::BadResponse(_) => "bad_response",
        }
    }
}

/// Étiquette admissible : courte, caractères sûrs (elle finit dans une adresse affichée).
fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= MAX_TAG
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// Lit la liste des releases. Tout élément douteux est ignoré (jamais d'erreur pour une seule entrée).
pub fn parse_releases(body: &[u8]) -> Result<Vec<Release>, UpdateError> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| UpdateError::BadResponse("JSON invalide"))?;
    let arr = v
        .as_array()
        .ok_or(UpdateError::BadResponse("liste attendue"))?;
    let mut out = Vec::new();
    for r in arr.iter().take(MAX_RELEASES) {
        if r.get("draft").and_then(|d| d.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(tag) = r.get("tag_name").and_then(|t| t.as_str()) else {
            continue;
        };
        if !valid_tag(tag) {
            continue;
        }
        let Some(version) = Version::parse(tag) else {
            continue;
        };
        let published_at = r
            .get("published_at")
            .and_then(|p| p.as_str())
            .filter(|p| p.len() <= 40 && p.bytes().all(|b| b.is_ascii_graphic()))
            .map(str::to_owned);
        out.push(Release {
            version,
            tag: tag.to_owned(),
            published_at,
            flagged_pre: r
                .get("prerelease")
                .and_then(|p| p.as_bool())
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// La plus récente des versions proposées à l'installation `current` : une pré-version (`rc`) voit les pré-versions et
/// les finales ; une finale ne voit que les finales.
pub fn newest<'a>(releases: &'a [Release], current: &Version) -> Option<&'a Release> {
    let with_pre = current.is_pre();
    releases
        .iter()
        .filter(|r| with_pre || !(r.version.is_pre() || r.flagged_pre))
        .max_by(|a, b| a.version.cmp(&b.version))
}

async fn read_limited(mut r: reqwest::Response) -> Result<Vec<u8>, UpdateError> {
    if r.content_length().is_some_and(|l| l > MAX_BYTES as u64) {
        return Err(UpdateError::BadResponse("réponse trop volumineuse"));
    }
    let mut out = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| UpdateError::Unreachable)? {
        if out.len() + chunk.len() > MAX_BYTES {
            return Err(UpdateError::BadResponse("réponse trop volumineuse"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Interroge `url` (en production : `RELEASES_URL`) et lit la liste des releases.
pub async fn fetch(url: &str) -> Result<Vec<Release>, UpdateError> {
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        // Générique, sans version ni identifiant : rien sur cette installation ne sort.
        .user_agent("nxtTorrent-update-check")
        .build()
        .map_err(|_| UpdateError::Unreachable)?;
    let r = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| UpdateError::Unreachable)?;
    match r.status().as_u16() {
        200 => {}
        403 | 429 => return Err(UpdateError::RateLimited),
        s => return Err(UpdateError::Http(s)),
    }
    parse_releases(&read_limited(r).await?)
}

// ---------- État dans le master ----------

const K_ENABLED: &str = "update_check";
const K_LATEST: &str = "update_latest";
const K_CHECKED: &str = "update_checked_at";
const K_ERROR: &str = "update_error";

impl Core {
    /// `None` : l'administrateur n'a pas encore choisi (l'interface le lui propose une fois).
    pub fn update_enabled(&self) -> Option<bool> {
        match self.store.setting_get(K_ENABLED).ok().flatten()?.as_str() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        }
    }

    pub fn set_update_enabled(&self, on: bool) -> Result<(), crate::store::StoreError> {
        self.store
            .setting_set(K_ENABLED, if on { "1" } else { "0" })?;
        self.bump_live();
        Ok(())
    }

    /// Adresse interrogée. Réservé aux tests (un faux serveur) : aucune API ni variable d'environnement ne l'atteint.
    pub fn set_update_source(&self, url: &str) {
        *self
            .update_source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = url.to_owned();
    }

    fn update_source(&self) -> String {
        self.update_source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn setting_i64(&self, k: &str) -> Option<i64> {
        self.store.setting_get(k).ok().flatten()?.parse().ok()
    }

    /// Vérifie maintenant et mémorise le résultat (ou le code d'erreur). N'échoue jamais.
    pub async fn update_check(&self) {
        let res = fetch(&self.update_source()).await;
        let _ = self.store.setting_set(K_CHECKED, &unix_now().to_string());
        match res {
            Ok(list) => {
                let cur = current_version();
                let latest = newest(&list, &cur).map(|r| {
                    serde_json::json!({
                        "version": r.version.to_string(),
                        "tag": r.tag,
                        "published_at": r.published_at,
                        "prerelease": r.version.is_pre() || r.flagged_pre,
                    })
                });
                if let Some(l) = &latest {
                    if Version::parse(l["version"].as_str().unwrap_or("")).is_some_and(|v| v > cur)
                    {
                        tracing::info!(version = %l["version"], "nouvelle version disponible");
                    }
                }
                let _ = self
                    .store
                    .setting_set(K_LATEST, &latest.map(|l| l.to_string()).unwrap_or_default());
                let _ = self.store.setting_set(K_ERROR, "");
            }
            Err(e) => {
                tracing::warn!(code = e.code(), "vérification des versions impossible");
                let _ = self.store.setting_set(K_ERROR, e.code());
            }
        }
        self.bump_live();
    }

    /// Vérification demandée à la main : ignorée si la précédente date de moins d'une minute. `true` si elle a eu lieu.
    pub async fn update_check_manual(&self) -> bool {
        let now = unix_now();
        if self
            .setting_i64(K_CHECKED)
            .is_some_and(|t| now - t < MANUAL_MIN_GAP_SECS)
        {
            return false;
        }
        self.update_check().await;
        true
    }

    /// Passage périodique : ne fait rien tant que l'administrateur n'a pas activé la vérification.
    pub async fn update_tick(&self) {
        if self.update_enabled() != Some(true) {
            return;
        }
        let failed = self
            .store
            .setting_get(K_ERROR)
            .ok()
            .flatten()
            .is_some_and(|e| !e.is_empty());
        let gap = if failed {
            RETRY_AFTER_ERROR_SECS
        } else {
            CHECK_EVERY_SECS
        };
        // (écrit sans `is_none_or` : stabilisé après notre version minimale de Rust)
        let due = match self.setting_i64(K_CHECKED) {
            None => true,
            Some(t) => unix_now() - t >= gap,
        };
        if due {
            self.update_check().await;
        }
    }

    /// État pour l'interface. `available` se recalcule à chaque lecture : après une mise à jour du master, le bandeau
    /// disparaît sans attendre la prochaine vérification.
    pub fn update_status(&self) -> serde_json::Value {
        let cur = current_version();
        let latest: Option<serde_json::Value> = self
            .store
            .setting_get(K_LATEST)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok());
        let latest_ver = latest
            .as_ref()
            .and_then(|l| l["version"].as_str())
            .and_then(Version::parse);
        let available = latest_ver.as_ref().is_some_and(|v| *v > cur);
        let latest = latest.map(|mut l| {
            let url = l["tag"]
                .as_str()
                .filter(|t| valid_tag(t))
                .map(|t| format!("{RELEASE_PAGE}{t}"));
            l["url"] = url.into();
            l
        });
        let error = self
            .store
            .setting_get(K_ERROR)
            .ok()
            .flatten()
            .filter(|e| !e.is_empty());
        serde_json::json!({
            "enabled": self.update_enabled(),
            "current": cur.to_string(),
            "channel": if cur.is_pre() { "rc" } else { "stable" },
            "latest": latest,
            "available": available,
            "checked_at": self.setting_i64(K_CHECKED),
            "error": error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("version invalide : {s}"))
    }

    #[test]
    fn versions_are_ordered_like_semver() {
        // Les pré-versions s'ordonnent numériquement (rc.10 après rc.3), la finale passe après ses pré-versions.
        let order = [
            "0.1.0-rc.1",
            "0.1.0-rc.2",
            "0.1.0-rc.3",
            "0.1.0-rc.10",
            "0.1.0",
            "0.1.1-rc.1",
            "0.1.1",
            "0.2.0",
            "1.0.0",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("v0.1.0-rc.3"), v("0.1.0-rc.3"), "préfixe v ignoré");
        assert_eq!(
            v("0.1.0+build.7"),
            v("0.1.0"),
            "métadonnées de build ignorées"
        );
        assert!(v("0.1.0-alpha") < v("0.1.0-alpha.1"), "plus court d'abord");
        assert!(
            v("0.1.0-1") < v("0.1.0-alpha"),
            "numérique avant alphanumérique"
        );
        assert_eq!(v("0.1.0-rc.3").to_string(), "0.1.0-rc.3");
    }

    #[test]
    fn malformed_versions_are_refused() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "1.2.3-",
            "1.2.3-rc..1",
            "1.2.3-r_c",
            "v",
            "-1.2.3",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }

    fn release(tag: &str, flagged: bool) -> Release {
        Release {
            version: v(tag),
            tag: tag.to_owned(),
            published_at: None,
            flagged_pre: flagged,
        }
    }

    #[test]
    fn a_release_candidate_sees_both_channels_and_a_stable_install_only_stable() {
        let all = vec![
            release("v0.1.0-rc.4", true),
            release("v0.1.0-rc.3", true),
            release("v0.0.9", false),
        ];
        // rc installée : voit la rc.4.
        assert_eq!(
            newest(&all, &v("0.1.0-rc.3")).map(|r| r.tag.as_str()),
            Some("v0.1.0-rc.4")
        );
        // finale installée : ne voit JAMAIS une pré-version.
        assert_eq!(
            newest(&all, &v("0.0.8")).map(|r| r.tag.as_str()),
            Some("v0.0.9")
        );
        // La pastille « pre-release » de GitHub suffit, même si l'étiquette ressemble à une finale.
        let odd = vec![release("v0.2.0", true)];
        assert!(newest(&odd, &v("0.1.0")).is_none());
        // Une finale arrivée remplace les rc pour une rc installée.
        let with_final = vec![release("v0.1.0", false), release("v0.1.0-rc.9", true)];
        assert_eq!(
            newest(&with_final, &v("0.1.0-rc.3")).map(|r| r.tag.as_str()),
            Some("v0.1.0")
        );
    }

    #[test]
    fn the_releases_list_is_read_defensively() {
        let body = br#"[
          {"tag_name":"v0.1.0-rc.4","prerelease":true,"draft":false,"published_at":"2026-11-23T20:00:00Z","html_url":"https://evil.example/x"},
          {"tag_name":"v9.9.9","draft":true},
          {"tag_name":"../../etc/passwd"},
          {"tag_name":"pas-une-version"},
          {"tag_name":"v0.1.0-rc.3<script>"},
          {"name":"sans etiquette"},
          42
        ]"#;
        let r = parse_releases(body).unwrap();
        assert_eq!(
            r.len(),
            1,
            "brouillon, étiquettes invalides et éléments étrangers ignorés"
        );
        assert_eq!(r[0].tag, "v0.1.0-rc.4");
        // Le lien est reconstruit : l'`html_url` de la réponse n'est jamais utilisé.
        assert_eq!(
            r[0].url(),
            "https://github.com/next-vertices/nxtTorrent/releases/tag/v0.1.0-rc.4"
        );
        assert!(
            parse_releases(b"{}").is_err(),
            "un objet n'est pas une liste"
        );
        assert!(parse_releases(b"pas du json").is_err());
        // Au plus 50 éléments lus.
        let many: Vec<String> = (0..200)
            .map(|i| format!(r#"{{"tag_name":"v0.0.{i}"}}"#))
            .collect();
        let r = parse_releases(format!("[{}]", many.join(",")).as_bytes()).unwrap();
        assert_eq!(r.len(), MAX_RELEASES);
    }

    #[test]
    fn this_build_has_a_valid_version() {
        assert_ne!(current_version(), v("0.0.0"), "CARGO_PKG_VERSION lisible");
    }
}
