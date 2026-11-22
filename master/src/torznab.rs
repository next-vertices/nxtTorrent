//! Client Torznab (Prowlarr, Jackett…) pour le cross-seed : recherche par nom, téléchargement des `.torrent`.
//!
//! Le master est seul à parler aux indexeurs ; l'agent n'en connaît aucun. Garde-fous : schéma http/https sans
//! identifiants dans l'URL, délais courts, redirections désactivées, réponses bornées en taille, lecteur XML minimal
//! qui REFUSE tout `<!DOCTYPE`/`<!ENTITY` (aucune expansion d'entité externe), et téléchargement de `.torrent`
//! uniquement depuis l'hôte de l'indexeur lui-même (un indexeur malveillant ne peut pas viser le réseau interne).
use std::time::Duration;

/// Taille maximale d'une réponse de recherche.
pub const MAX_SEARCH_BYTES: usize = 5 * 1024 * 1024;
const MAX_ITEMS: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum TorznabError {
    #[error("URL d'indexeur invalide : {0}")]
    BadUrl(&'static str),
    #[error("indexeur injoignable")]
    Unreachable,
    #[error("réponse de l'indexeur refusée : {0}")]
    BadResponse(&'static str),
    #[error("l'indexeur a répondu par une erreur : {0}")]
    Remote(String),
    /// Identifiants refusés, compte suspendu ou accès bloqué (HTTP 401/403, erreurs Torznab 100-199) : on cesse
    /// d'insister, un compte peut être banni pour des requêtes répétées avec une mauvaise clé.
    #[error("accès refusé par l'indexeur : {0}")]
    Auth(String),
    /// L'indexeur demande de ralentir (HTTP 429, ou 503 avec `Retry-After`) ; secondes d'attente demandées.
    #[error("l'indexeur demande de ralentir (trop de requêtes)")]
    RateLimited(Option<u64>),
    /// Quota atteint (erreurs Torznab 500 « limite de requêtes », 501 « limite de téléchargements », 910).
    #[error("quota de l'indexeur atteint : {0}")]
    Quota(String),
    /// Problème temporaire de l'indexeur (HTTP 5xx).
    #[error("l'indexeur a un problème temporaire : {0}")]
    Transient(&'static str),
}

impl TorznabError {
    /// Faute à ne PAS retenter tout de suite : l'indexeur nous a demandé d'attendre ou a refusé l'accès.
    pub fn is_hard_stop(&self) -> bool {
        matches!(
            self,
            Self::Auth(_) | Self::RateLimited(_) | Self::Quota(_) | Self::BadUrl(_)
        )
    }
}

/// Traduit un élément `<error code=… description=…/>` de Torznab.
fn remote_error(code: Option<&str>, description: String) -> TorznabError {
    let d: String = description.chars().take(200).collect();
    match code.and_then(|c| c.parse::<u32>().ok()) {
        // 100 identifiants incorrects, 101 compte suspendu, 102 droits insuffisants, 103 inscription refusée.
        Some(100..=199) => TorznabError::Auth(d),
        // 500 limite de requêtes, 501 limite de téléchargements, 910 API désactivée.
        Some(500 | 501 | 910) => TorznabError::Quota(d),
        _ => TorznabError::Remote(d),
    }
}

/// Valeur de `Retry-After` en secondes (la forme « date HTTP » est ignorée : l'attente par défaut s'applique).
fn retry_after(r: &reqwest::Response) -> Option<u64> {
    r.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Erreur correspondant à un statut HTTP non réussi.
fn status_error(r: &reqwest::Response) -> TorznabError {
    let s = r.status().as_u16();
    match s {
        429 => TorznabError::RateLimited(retry_after(r)),
        401 | 403 => TorznabError::Auth(format!(
            "HTTP {s} : clé invalide, compte ou adresse IP bloqué"
        )),
        503 if retry_after(r).is_some() => TorznabError::RateLimited(retry_after(r)),
        500..=599 => TorznabError::Transient("erreur du serveur"),
        _ => TorznabError::BadResponse("statut HTTP inattendu"),
    }
}

/// Un résultat de recherche.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Item {
    pub title: String,
    /// URL de téléchargement du `.torrent` (peut porter la clé d'API ou un passkey : jamais journalisée).
    pub link: String,
    pub size: u64,
    /// Infohash annoncé par l'indexeur (40 hexadécimaux minuscules), s'il le donne.
    pub infohash: Option<String>,
}

fn unescape(s: &str) -> String {
    let s = s.trim();
    // Section CDATA : contenu littéral.
    if let Some(inner) = s
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
    {
        return inner.trim().to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (rep, len) = if tail.starts_with("&amp;") {
            ('&', 5)
        } else if tail.starts_with("&lt;") {
            ('<', 4)
        } else if tail.starts_with("&gt;") {
            ('>', 4)
        } else if tail.starts_with("&quot;") {
            ('"', 6)
        } else if tail.starts_with("&apos;") {
            ('\'', 6)
        } else {
            ('&', 1) // entité inconnue : laissée telle quelle, jamais développée
        };
        out.push(rep);
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

/// Texte d'une balise simple `<name>…</name>` dans `block`.
fn tag_text(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(unescape(&block[start..end]))
}

/// Valeur de l'attribut `attr` d'une balise ouvrante `<tag …>`, avec filtre éventuel sur un autre attribut.
fn attr_of(tag_src: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let key = format!(" {attr}={quote}");
        if let Some(i) = tag_src.find(&key) {
            let v = &tag_src[i + key.len()..];
            let end = v.find(quote)?;
            return Some(unescape(&v[..end]));
        }
    }
    None
}

/// Balises ouvrantes `<tag …>` ou `<tag …/>` rencontrées dans `block`.
fn open_tags<'a>(block: &'a str, tag: &str) -> Vec<&'a str> {
    let needle = format!("<{tag} ");
    let mut v = Vec::new();
    let mut rest = block;
    while let Some(i) = rest.find(&needle) {
        let from = &rest[i..];
        let Some(end) = from.find('>') else { break };
        v.push(&from[..end]);
        rest = &from[end..];
    }
    v
}

/// Analyse une réponse RSS Torznab. Une réponse `<error …/>` devient une erreur.
pub fn parse_search(xml: &str) -> Result<Vec<Item>, TorznabError> {
    if xml.len() > MAX_SEARCH_BYTES {
        return Err(TorznabError::BadResponse("réponse trop volumineuse"));
    }
    if xml.contains("<!DOCTYPE") || xml.contains("<!ENTITY") {
        return Err(TorznabError::BadResponse("DOCTYPE/ENTITY refusés"));
    }
    if let Some(e) = open_tags(xml, "error").first() {
        let d = attr_of(e, "description").unwrap_or_default();
        return Err(remote_error(attr_of(e, "code").as_deref(), d));
    }
    let mut items = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<item>") {
        let from = &rest[i + "<item>".len()..];
        let Some(end) = from.find("</item>") else {
            break;
        };
        let block = &from[..end];
        rest = &from[end + "</item>".len()..];
        if items.len() >= MAX_ITEMS {
            break;
        }
        let title = tag_text(block, "title").unwrap_or_default();
        // Lien : <link> ; à défaut, l'URL de <enclosure>.
        let link = tag_text(block, "link")
            .filter(|l| !l.is_empty())
            .or_else(|| {
                open_tags(block, "enclosure")
                    .first()
                    .and_then(|t| attr_of(t, "url"))
            })
            .unwrap_or_default();
        let mut size = tag_text(block, "size").and_then(|s| s.parse::<u64>().ok());
        let mut infohash = None;
        for a in open_tags(block, "torznab:attr") {
            let (Some(n), Some(v)) = (attr_of(a, "name"), attr_of(a, "value")) else {
                continue;
            };
            match n.as_str() {
                "size" if size.is_none() => size = v.parse().ok(),
                "infohash" => {
                    let v = v.to_ascii_lowercase();
                    if v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
                        infohash = Some(v);
                    }
                }
                _ => {}
            }
        }
        if title.is_empty() || link.is_empty() {
            continue;
        }
        items.push(Item {
            title: title.chars().take(300).collect(),
            link,
            size: size.unwrap_or(0),
            infohash,
        });
    }
    Ok(items)
}

/// URL d'un indexeur validée : http(s), hôte présent, ni identifiants ni fragment.
pub fn check_base_url(url: &str) -> Result<reqwest::Url, TorznabError> {
    let u = reqwest::Url::parse(url).map_err(|_| TorznabError::BadUrl("syntaxe"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(TorznabError::BadUrl("http ou https uniquement"));
    }
    if u.host_str().is_none() {
        return Err(TorznabError::BadUrl("hôte absent"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(TorznabError::BadUrl("identifiants interdits dans l'URL"));
    }
    if u.fragment().is_some() || u.query().is_some() {
        return Err(TorznabError::BadUrl(
            "ni requête ni fragment (la clé d'API est un champ à part)",
        ));
    }
    Ok(u)
}

/// Même schéma, même hôte, même port : seule origine d'où un `.torrent` est téléchargé.
pub fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

pub struct Torznab {
    base: reqwest::Url,
    api_key: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Torznab {
    // La clé d'API ne sort jamais dans un journal.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Torznab")
            .field("host", &self.base.host_str())
            .finish_non_exhaustive()
    }
}

/// Lit le corps en s'arrêtant au-delà de `max` octets (pas de mémoire proportionnelle à la réponse).
async fn read_limited(mut r: reqwest::Response, max: usize) -> Result<Vec<u8>, TorznabError> {
    if r.content_length().is_some_and(|l| l > max as u64) {
        return Err(TorznabError::BadResponse("réponse trop volumineuse"));
    }
    let mut out = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| TorznabError::Unreachable)? {
        if out.len() + chunk.len() > max {
            return Err(TorznabError::BadResponse("réponse trop volumineuse"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

impl Torznab {
    /// `base_url` : adresse de l'indexeur SANS `/api` (ex. `http://prowlarr:9696/1`), `api_key` : sa clé.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, TorznabError> {
        let base = check_base_url(base_url)?;
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("nxtTorrent")
            .build()
            .map_err(|_| TorznabError::Unreachable)?;
        Ok(Self {
            base,
            api_key: api_key.to_owned(),
            http,
        })
    }

    fn api_url(&self) -> reqwest::Url {
        let mut u = self.base.clone();
        let path = format!("{}/api", u.path().trim_end_matches('/'));
        u.set_path(&path);
        u
    }

    /// Recherche par texte. Le nom du torrent et la clé d'API sont les seules données envoyées.
    pub async fn search(&self, query: &str) -> Result<Vec<Item>, TorznabError> {
        let mut u = self.api_url();
        u.query_pairs_mut()
            .append_pair("t", "search")
            .append_pair("q", query)
            .append_pair("extended", "1")
            .append_pair("limit", "100")
            .append_pair("apikey", &self.api_key);
        let r = self
            .http
            .get(u)
            .send()
            .await
            .map_err(|_| TorznabError::Unreachable)?;
        if !r.status().is_success() {
            return Err(status_error(&r));
        }
        let body = read_limited(r, MAX_SEARCH_BYTES).await?;
        parse_search(&String::from_utf8_lossy(&body))
    }

    /// Test de configuration : capacités de l'indexeur (`t=caps`). Succès = URL et clé acceptées.
    pub async fn test(&self) -> Result<(), TorznabError> {
        let mut u = self.api_url();
        u.query_pairs_mut()
            .append_pair("t", "caps")
            .append_pair("apikey", &self.api_key);
        let r = self
            .http
            .get(u)
            .send()
            .await
            .map_err(|_| TorznabError::Unreachable)?;
        if !r.status().is_success() {
            return Err(status_error(&r));
        }
        let body = read_limited(r, MAX_SEARCH_BYTES).await?;
        let text = String::from_utf8_lossy(&body);
        if let Some(e) = open_tags(&text, "error").first() {
            return Err(remote_error(
                attr_of(e, "code").as_deref(),
                attr_of(e, "description").unwrap_or_default(),
            ));
        }
        Ok(())
    }

    /// Télécharge un `.torrent` : uniquement depuis l'hôte de l'indexeur, ≤ 4 Mio, redirections refusées.
    pub async fn download(&self, link: &str) -> Result<Vec<u8>, TorznabError> {
        let u = reqwest::Url::parse(link).map_err(|_| TorznabError::BadUrl("lien invalide"))?;
        if !same_origin(&u, &self.base) {
            return Err(TorznabError::BadUrl("lien hors de l'hôte de l'indexeur"));
        }
        let r = self
            .http
            .get(u)
            .send()
            .await
            .map_err(|_| TorznabError::Unreachable)?;
        if !r.status().is_success() {
            return Err(status_error(&r));
        }
        read_limited(r, proto::metainfo::MAX_TORRENT_FILE).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
<item><title>Film.2024.1080p-GRP</title><link>http://idx.test/dl/1?apikey=k&amp;file=a</link><size>1005</size>
<torznab:attr name="infohash" value="ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD" /></item>
<item><title><![CDATA[Série & co 720p]]></title><enclosure url="http://idx.test/dl/2" length="55" type="application/x-bittorrent" />
<torznab:attr name="size" value="777"/></item>
<item><title>Sans lien</title></item>
</channel></rss>"#;

    #[test]
    fn rss_items_are_parsed_with_entities_cdata_and_attributes() {
        let v = parse_search(RSS).unwrap();
        assert_eq!(v.len(), 2, "l'item sans lien est ignoré");
        assert_eq!(v[0].title, "Film.2024.1080p-GRP");
        assert_eq!(v[0].link, "http://idx.test/dl/1?apikey=k&file=a");
        assert_eq!(v[0].size, 1005);
        assert_eq!(
            v[0].infohash.as_deref(),
            Some("abcdefabcdefabcdefabcdefabcdefabcdefabcd")
        );
        assert_eq!(v[1].title, "Série & co 720p");
        assert_eq!(v[1].link, "http://idx.test/dl/2", "repli sur <enclosure>");
        assert_eq!(v[1].size, 777, "taille lue dans torznab:attr");
        assert_eq!(v[1].infohash, None);
    }

    #[test]
    fn errors_doctype_and_oversize_are_refused() {
        let e = parse_search(r#"<error code="100" description="Incorrect user credentials"/>"#);
        assert!(matches!(e, Err(TorznabError::Auth(m)) if m.contains("credentials")));
        assert!(
            parse_search("<!DOCTYPE x [<!ENTITY a SYSTEM \"file:///etc/passwd\">]><rss/>").is_err()
        );
        assert!(parse_search(&"x".repeat(MAX_SEARCH_BYTES + 1)).is_err());
        // Entité inconnue : jamais développée.
        let v =
            parse_search("<item><title>a &foo; b</title><link>http://i/x</link></item>").unwrap();
        assert_eq!(v[0].title, "a &foo; b");
        // Beaucoup d'items : bornés.
        let many = "<item><title>t</title><link>http://i/x</link></item>".repeat(500);
        assert_eq!(parse_search(&many).unwrap().len(), MAX_ITEMS);
    }

    #[test]
    fn indexer_urls_are_validated_and_downloads_stay_on_the_indexer_host() {
        assert!(check_base_url("http://prowlarr:9696/1").is_ok());
        assert!(check_base_url("https://idx.example.org").is_ok());
        for bad in [
            "ftp://x/",
            "file:///etc/passwd",
            "http://user:pw@host/",
            "http://host/?apikey=secret",
            "http://host/#f",
            "pas une url",
            "http:///",
        ] {
            assert!(check_base_url(bad).is_err(), "{bad}");
        }
        let a = check_base_url("http://idx.test:9696/1").unwrap();
        assert!(same_origin(
            &a,
            &reqwest::Url::parse("http://idx.test:9696/dl/9").unwrap()
        ));
        for other in [
            "http://evil.test:9696/dl",
            "http://idx.test:9697/dl",
            "https://idx.test:9696/dl",
            "http://127.0.0.1:9696/dl",
        ] {
            assert!(
                !same_origin(&a, &reqwest::Url::parse(other).unwrap()),
                "{other}"
            );
        }
    }

    #[test]
    fn torznab_error_codes_map_to_the_right_reaction() {
        let e =
            |c: &str| parse_search(&format!(r#"<error code="{c}" description="x"/>"#)).unwrap_err();
        for c in ["100", "101", "102", "103"] {
            assert!(matches!(e(c), TorznabError::Auth(_)), "{c}");
        }
        for c in ["500", "501", "910"] {
            assert!(matches!(e(c), TorznabError::Quota(_)), "{c}");
        }
        for c in ["200", "201", "300", "900", "abc"] {
            assert!(matches!(e(c), TorznabError::Remote(_)), "{c}");
        }
        assert!(e("100").is_hard_stop() && e("500").is_hard_stop() && !e("900").is_hard_stop());
        assert!(TorznabError::RateLimited(Some(30)).is_hard_stop());
        assert!(!TorznabError::Unreachable.is_hard_stop());
    }

    #[test]
    fn the_api_key_never_appears_in_debug_output() {
        let t = Torznab::new("http://idx.test/1", "SECRET-KEY").unwrap();
        assert!(!format!("{t:?}").contains("SECRET"));
    }
}
