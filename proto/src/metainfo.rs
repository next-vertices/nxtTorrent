//! Lecture sûre d'un fichier `.torrent` (bencode) : infohash, nom, taille, trackers.
//! Aucune exécution, aucune allocation proportionnelle à des longueurs annoncées : tout est borné.
use sha1::{Digest, Sha1};

/// Taille maximale acceptée pour un fichier `.torrent` (master, agent, API).
pub const MAX_TORRENT_FILE: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_TRACKERS: usize = 20;
/// Au-delà, la liste des fichiers n'est pas conservée (pas de clé de contenu) : borne mémoire, pas un refus du torrent.
pub const MAX_LISTED_FILES: usize = 50_000;

/// Un fichier du torrent : chemin RELATIF à la racine du torrent (composants joints par `/`) et taille.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metainfo {
    /// Infohash v1 (SHA-1 du dictionnaire `info`), 40 hexadécimaux minuscules.
    pub infohash: String,
    pub name: String,
    /// Taille totale du contenu en octets.
    pub size: u64,
    pub file_count: usize,
    /// Torrent privé : ses métadonnées ne peuvent PAS être obtenues par magnet, d'où l'envoi du fichier.
    pub private: bool,
    pub trackers: Vec<String>,
    /// Fichiers du contenu (vide si plus de `MAX_LISTED_FILES`). Torrent mono-fichier : un seul, au nom du torrent.
    pub files: Vec<FileEntry>,
    /// Torrent mono-fichier : `name` est le nom du FICHIER, pas d'un dossier racine.
    pub single_file: bool,
    /// Empreinte (SHA-1, hexadécimal) de la taille de pièce et des empreintes de pièces. Deux torrents de même empreinte
    /// ET de mêmes fichiers portent des données IDENTIQUES : la vérification ne peut pas les trouver partielles.
    pub pieces_digest: String,
}

impl Metainfo {
    /// Clé de contenu : SHA-256 des couples (chemin relatif, taille) triés. Deux torrents de même clé portent les
    /// MÊMES fichiers aux mêmes chemins (le dossier racine, lui, peut différer : voir `name`). `None` si la liste est
    /// vide ou trop longue pour avoir été conservée.
    pub fn content_key(&self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let mut sorted: Vec<&FileEntry> = self.files.iter().collect();
        sorted.sort();
        let mut h = sha2::Sha256::new();
        for f in sorted {
            h.update((f.path.len() as u64).to_be_bytes());
            h.update(f.path.as_bytes());
            h.update(f.size.to_be_bytes());
        }
        Some(data_encoding::HEXLOWER.encode(&h.finalize()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetainfoError {
    #[error("fichier trop gros (4 Mio maximum)")]
    TooLarge,
    #[error("fichier .torrent invalide : {0}")]
    Invalid(&'static str),
    #[error("torrent BitTorrent v2 pur non pris en charge (il faut un torrent v1 ou hybride)")]
    V2Only,
}

/// Nœud bencode : on ne garde que les positions dans le tampon d'origine.
#[derive(Debug)]
enum Node {
    Int(i64),
    Bytes(std::ops::Range<usize>),
    List(Vec<Item>),
    Dict(Vec<(std::ops::Range<usize>, Item)>),
}

#[derive(Debug)]
struct Item {
    node: Node,
    span: std::ops::Range<usize>,
}

fn parse_item(b: &[u8], pos: usize, depth: usize) -> Result<(Item, usize), MetainfoError> {
    if depth > MAX_DEPTH {
        return Err(MetainfoError::Invalid("imbrication excessive"));
    }
    let bad = MetainfoError::Invalid;
    let c = *b.get(pos).ok_or(bad("fin de fichier inattendue"))?;
    match c {
        b'i' => {
            let end = b[pos..]
                .iter()
                .position(|x| *x == b'e')
                .ok_or(bad("entier non terminé"))?
                + pos;
            let txt = std::str::from_utf8(&b[pos + 1..end]).map_err(|_| bad("entier invalide"))?;
            let v: i64 = txt.parse().map_err(|_| bad("entier invalide"))?;
            Ok((
                Item {
                    node: Node::Int(v),
                    span: pos..end + 1,
                },
                end + 1,
            ))
        }
        b'l' | b'd' => {
            let dict = c == b'd';
            let mut p = pos + 1;
            let (mut list, mut map) = (Vec::new(), Vec::new());
            loop {
                if *b.get(p).ok_or(bad("liste ou dictionnaire non terminé"))? == b'e' {
                    p += 1;
                    break;
                }
                if dict {
                    let (k, np) = parse_item(b, p, depth + 1)?;
                    let Node::Bytes(kr) = k.node else {
                        return Err(bad("clé de dictionnaire non textuelle"));
                    };
                    let (v, np) = parse_item(b, np, depth + 1)?;
                    map.push((kr, v));
                    p = np;
                } else {
                    let (v, np) = parse_item(b, p, depth + 1)?;
                    list.push(v);
                    p = np;
                }
            }
            let node = if dict {
                Node::Dict(map)
            } else {
                Node::List(list)
            };
            Ok((Item { node, span: pos..p }, p))
        }
        b'0'..=b'9' => {
            let colon = b[pos..]
                .iter()
                .position(|x| *x == b':')
                .ok_or(bad("longueur sans ':'"))?
                + pos;
            let len: usize = std::str::from_utf8(&b[pos..colon])
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or(bad("longueur invalide"))?;
            let start = colon + 1;
            let end = start
                .checked_add(len)
                .filter(|e| *e <= b.len())
                .ok_or(bad("chaîne tronquée"))?;
            Ok((
                Item {
                    node: Node::Bytes(start..end),
                    span: pos..end,
                },
                end,
            ))
        }
        _ => Err(bad("caractère inattendu")),
    }
}

fn get<'a>(dict: &'a [(std::ops::Range<usize>, Item)], b: &[u8], key: &str) -> Option<&'a Item> {
    dict.iter()
        .find(|(k, _)| &b[k.clone()] == key.as_bytes())
        .map(|(_, v)| v)
}

fn text(b: &[u8], r: &std::ops::Range<usize>, max: usize) -> String {
    String::from_utf8_lossy(&b[r.clone()])
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// Analyse un `.torrent` et calcule son infohash.
pub fn parse(b: &[u8]) -> Result<Metainfo, MetainfoError> {
    if b.len() > MAX_TORRENT_FILE {
        return Err(MetainfoError::TooLarge);
    }
    let (root, end) = parse_item(b, 0, 0)?;
    if end != b.len() {
        return Err(MetainfoError::Invalid("données après la fin du torrent"));
    }
    let Node::Dict(top) = &root.node else {
        return Err(MetainfoError::Invalid(
            "la racine doit être un dictionnaire",
        ));
    };
    let info = get(top, b, "info").ok_or(MetainfoError::Invalid("dictionnaire `info` absent"))?;
    let Node::Dict(i) = &info.node else {
        return Err(MetainfoError::Invalid("`info` n'est pas un dictionnaire"));
    };
    if get(i, b, "pieces").is_none() {
        return Err(MetainfoError::V2Only);
    }
    let name = match get(i, b, "name").map(|x| &x.node) {
        Some(Node::Bytes(r)) => text(b, r, 255),
        _ => return Err(MetainfoError::Invalid("`info.name` absent")),
    };
    let pieces_digest = {
        let mut h = Sha1::new();
        if let Some(Node::Int(pl)) = get(i, b, "piece length").map(|x| &x.node) {
            h.update(pl.to_be_bytes());
        }
        if let Some(Node::Bytes(r)) = get(i, b, "pieces").map(|x| &x.node) {
            h.update(&b[r.clone()]);
        }
        data_encoding::HEXLOWER.encode(&h.finalize())
    };
    let mut files: Vec<FileEntry> = Vec::new();
    let single_file = matches!(get(i, b, "length").map(|x| &x.node), Some(Node::Int(_)));
    let (size, file_count) = if let Some(Node::Int(l)) = get(i, b, "length").map(|x| &x.node) {
        let size = u64::try_from(*l).map_err(|_| MetainfoError::Invalid("taille négative"))?;
        files.push(FileEntry {
            path: name.clone(),
            size,
        });
        (size, 1)
    } else if let Some(Node::List(list)) = get(i, b, "files").map(|x| &x.node) {
        let mut total = 0u64;
        for f in list {
            let Node::Dict(fd) = &f.node else {
                return Err(MetainfoError::Invalid("entrée de `files` invalide"));
            };
            let Some(Node::Int(l)) = get(fd, b, "length").map(|x| &x.node) else {
                return Err(MetainfoError::Invalid("taille de fichier absente"));
            };
            let size = u64::try_from(*l).map_err(|_| MetainfoError::Invalid("taille négative"))?;
            total = total
                .checked_add(size)
                .ok_or(MetainfoError::Invalid("taille totale démesurée"))?;
            if list.len() <= MAX_LISTED_FILES {
                // Chemin : composants non vides, sans `.`/`..` ni séparateur (jamais de sortie de la racine).
                let Some(Node::List(parts)) = get(fd, b, "path").map(|x| &x.node) else {
                    return Err(MetainfoError::Invalid("chemin de fichier absent"));
                };
                let mut comps = Vec::with_capacity(parts.len());
                for p in parts {
                    let Node::Bytes(r) = &p.node else {
                        return Err(MetainfoError::Invalid("chemin de fichier invalide"));
                    };
                    let c = text(b, r, 255);
                    if c.is_empty() || c == "." || c == ".." || c.contains(['/', '\\']) {
                        return Err(MetainfoError::Invalid("chemin de fichier invalide"));
                    }
                    comps.push(c);
                }
                if comps.is_empty() {
                    return Err(MetainfoError::Invalid("chemin de fichier vide"));
                }
                files.push(FileEntry {
                    path: comps.join("/"),
                    size,
                });
            }
        }
        if list.len() > MAX_LISTED_FILES {
            files.clear();
        }
        (total, list.len())
    } else {
        return Err(MetainfoError::Invalid("ni `length` ni `files`"));
    };
    let private = matches!(get(i, b, "private").map(|x| &x.node), Some(Node::Int(1)));

    let mut trackers = Vec::new();
    let mut add = |r: &std::ops::Range<usize>| {
        let t = text(b, r, 300);
        let ok = ["http://", "https://", "udp://"]
            .iter()
            .any(|p| t.starts_with(p));
        if ok && trackers.len() < MAX_TRACKERS && !trackers.contains(&t) {
            trackers.push(t);
        }
    };
    if let Some(Node::Bytes(r)) = get(top, b, "announce").map(|x| &x.node) {
        add(r);
    }
    if let Some(Node::List(tiers)) = get(top, b, "announce-list").map(|x| &x.node) {
        for tier in tiers {
            if let Node::List(urls) = &tier.node {
                for u in urls {
                    if let Node::Bytes(r) = &u.node {
                        add(r);
                    }
                }
            }
        }
    }

    let infohash = data_encoding::HEXLOWER.encode(&Sha1::digest(&b[info.span.clone()]));
    Ok(Metainfo {
        infohash,
        name,
        size,
        file_count,
        private,
        trackers,
        files,
        single_file,
        pieces_digest,
    })
}

/// Nombre maximal de fichiers pour lesquels on crée des liens (un message de commande reste petit).
pub const MAX_LINKED_FILES: usize = 5_000;

/// Un lien à créer : fichier existant du torrent d'origine -> fichier du candidat (chemins relatifs à leur racine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPair {
    pub src: String,
    pub dst: String,
}

/// Disposition des liens à créer pour qu'un torrent `cand` retrouve les fichiers de `parent` sous SA disposition
/// (dossier racine et/ou noms de fichiers différents). `None` si les contenus ne correspondent pas fichier par
/// fichier. Règles, volontairement strictes (la revérification du client tranche ensuite sur le contenu réel) :
/// - chaque fichier du candidat reçoit EXACTEMENT un fichier du parent de même taille, et inversement ;
/// - un chemin identique est apparié d'office ; sinon, une taille portée par un seul fichier de chaque côté ;
///   sinon, un nom de fichier (dernier composant) unique ; au-delà, ambigu : refus.
pub fn plan_links(parent: &Metainfo, cand: &Metainfo) -> Option<Vec<LinkPair>> {
    if parent.files.is_empty()
        || parent.files.len() != cand.files.len()
        || parent.size != cand.size
        || cand.files.len() > MAX_LINKED_FILES
    {
        return None;
    }
    let mut free: Vec<Option<&FileEntry>> = parent.files.iter().map(Some).collect();
    let mut pairs: Vec<Option<LinkPair>> = vec![None; cand.files.len()];
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let take = |free: &mut Vec<Option<&FileEntry>>, k: usize, dst: &str| {
        free[k].take().map(|f| LinkPair {
            src: f.path.clone(),
            dst: dst.to_owned(),
        })
    };
    // 1. Même chemin et même taille.
    for (i, c) in cand.files.iter().enumerate() {
        if let Some(k) = free
            .iter()
            .position(|f| f.is_some_and(|f| f.path == c.path && f.size == c.size))
        {
            pairs[i] = take(&mut free, k, &c.path);
        }
    }
    // 2. Taille portée par un seul fichier restant de chaque côté.
    for i in 0..cand.files.len() {
        if pairs[i].is_some() {
            continue;
        }
        let size = cand.files[i].size;
        let rivals = (0..cand.files.len())
            .filter(|&j| pairs[j].is_none() && cand.files[j].size == size)
            .count();
        let options: Vec<usize> = (0..free.len())
            .filter(|&k| free[k].is_some_and(|f| f.size == size))
            .collect();
        if rivals == 1 && options.len() == 1 {
            pairs[i] = take(&mut free, options[0], &cand.files[i].path);
        }
    }
    // 3. Même taille, plusieurs candidats : le nom de fichier doit être unique parmi eux.
    for i in 0..cand.files.len() {
        if pairs[i].is_some() {
            continue;
        }
        let (size, name) = (cand.files[i].size, base(&cand.files[i].path));
        let options: Vec<usize> = (0..free.len())
            .filter(|&k| free[k].is_some_and(|f| f.size == size && base(&f.path) == name))
            .collect();
        let rivals = (0..cand.files.len())
            .filter(|&j| {
                pairs[j].is_none()
                    && cand.files[j].size == size
                    && base(&cand.files[j].path) == name
            })
            .count();
        if options.len() == 1 && rivals == 1 {
            pairs[i] = take(&mut free, options[0], &cand.files[i].path);
        }
    }
    pairs.into_iter().collect()
}

/// Faut-il un dossier de LIENS isolé ? Non seulement si le candidat se pose exactement sur les fichiers de l'original ET
/// que les données sont PROUVÉES identiques : mêmes chemins, même dossier racine, mêmes empreintes de pièces. Sinon le
/// candidat a son propre arbre de liens : si le client le juge incomplet (contenu différent), il peut renommer SES
/// fichiers en « partiels » (Transmission le fait, constaté en test réel) ; avec des liens, seuls ses noms bougent et
/// l'original reste intact.
pub fn needs_links(parent: &Metainfo, cand: &Metainfo) -> bool {
    !(parent.name == cand.name
        && parent.content_key().is_some()
        && parent.content_key() == cand.content_key()
        && parent.pieces_digest == cand.pieces_digest)
}

/// Nombre maximal d'adresses de trackers dans un torrent créé.
pub const MAX_TRACKERS_CREATED: usize = MAX_TRACKERS;

/// Adresse de tracker acceptable : http, https ou udp ; ASCII imprimable sans espace, 500 caractères au plus.
pub fn valid_tracker_url(u: &str) -> bool {
    ["http://", "https://", "udp://"]
        .iter()
        .any(|p| u.starts_with(p))
        && u.len() <= 500
        && u.len() > 8
        && u.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

/// Assemble un `.torrent` complet autour du dictionnaire `info` de `b` : adresses de trackers (par paliers), commentaire,
/// « created by » et date de création. Le dictionnaire `info` est recopié OCTET POUR OCTET : l'infohash ne change pas.
/// Tout le reste de `b` est ignoré (rien d'autre que `info` n'est conservé). Un torrent privé exige au moins un tracker.
pub fn compose(
    b: &[u8],
    tiers: &[Vec<String>],
    comment: Option<&str>,
    created_by: &str,
    creation_date: Option<i64>,
) -> Result<Vec<u8>, MetainfoError> {
    let meta = parse(b)?;
    let (root, _) = parse_item(b, 0, 0)?;
    let Node::Dict(top) = &root.node else {
        return Err(MetainfoError::Invalid(
            "la racine doit être un dictionnaire",
        ));
    };
    let info = get(top, b, "info").ok_or(MetainfoError::Invalid("dictionnaire `info` absent"))?;
    let all: Vec<&String> = tiers.iter().flatten().collect();
    if tiers.iter().any(Vec::is_empty)
        || all.len() > MAX_TRACKERS_CREATED
        || !all.iter().all(|u| valid_tracker_url(u))
    {
        return Err(MetainfoError::Invalid(
            "adresse de tracker invalide (http, https ou udp ; 20 au plus)",
        ));
    }
    if meta.private && all.is_empty() {
        return Err(MetainfoError::Invalid(
            "un torrent privé exige au moins un tracker",
        ));
    }
    if comment.is_some_and(|c| c.len() > 500 || c.chars().any(char::is_control))
        || created_by.len() > 64
        || created_by.chars().any(char::is_control)
    {
        return Err(MetainfoError::Invalid(
            "commentaire ou « created by » invalide",
        ));
    }
    let bstr = |out: &mut Vec<u8>, x: &[u8]| {
        out.extend(x.len().to_string().bytes());
        out.push(b':');
        out.extend(x);
    };
    // Clés de premier niveau triées : announce, announce-list, comment, created by, creation date, info.
    let mut out = vec![b'd'];
    if let Some(first) = all.first() {
        bstr(&mut out, b"announce");
        bstr(&mut out, first.as_bytes());
        if tiers.len() > 1 || tiers[0].len() > 1 {
            bstr(&mut out, b"announce-list");
            out.push(b'l');
            for t in tiers {
                out.push(b'l');
                for u in t {
                    bstr(&mut out, u.as_bytes());
                }
                out.push(b'e');
            }
            out.push(b'e');
        }
    }
    if let Some(c) = comment.filter(|c| !c.is_empty()) {
        bstr(&mut out, b"comment");
        bstr(&mut out, c.as_bytes());
    }
    if !created_by.is_empty() {
        bstr(&mut out, b"created by");
        bstr(&mut out, created_by.as_bytes());
    }
    if let Some(d) = creation_date {
        out.extend(format!("13:creation datei{d}e").bytes());
    }
    bstr(&mut out, b"info");
    out.extend(&b[info.span.clone()]);
    out.push(b'e');
    if out.len() > MAX_TORRENT_FILE {
        return Err(MetainfoError::TooLarge);
    }
    Ok(out)
}

/// Réécrit un `.torrent` pour ne l'annoncer QU'À `tracker_url` : supprime `announce`, `announce-list`, `nodes`,
/// `url-list` et `httpseeds` (aucune autre source ne doit être contactée), garde tout le reste octet pour octet.
/// Le dictionnaire `info` est recopié tel quel : l'**infohash est strictement identique** (le client reconnaît le même
/// torrent, les données déjà présentes restent valides, un torrent privé reste privé).
pub fn rewrite_announce(b: &[u8], tracker_url: &str) -> Result<Vec<u8>, MetainfoError> {
    if tracker_url.is_empty()
        || tracker_url.len() > 500
        || tracker_url.bytes().any(|c| !(0x21..=0x7e).contains(&c))
    {
        return Err(MetainfoError::Invalid("URL de tracker invalide"));
    }
    parse(b)?; // valide d'abord (taille, structure, v1)
    let (root, _) = parse_item(b, 0, 0)?;
    let Node::Dict(top) = &root.node else {
        return Err(MetainfoError::Invalid(
            "la racine doit être un dictionnaire",
        ));
    };
    const DROP: [&[u8]; 5] = [
        b"announce",
        b"announce-list",
        b"nodes",
        b"url-list",
        b"httpseeds",
    ];
    let mut entries: Vec<(Vec<u8>, &[u8])> = top
        .iter()
        .filter(|(k, _)| !DROP.contains(&&b[k.clone()]))
        .map(|(k, v)| (b[k.clone()].to_vec(), &b[v.span.clone()]))
        .collect();
    let announce = format!("{}:{}", tracker_url.len(), tracker_url).into_bytes();
    entries.push((b"announce".to_vec(), &announce));
    // Les clés d'un dictionnaire bencode sont triées (ordre des octets).
    entries.sort_by(|x, y| x.0.cmp(&y.0));
    let mut out = vec![b'd'];
    for (k, v) in entries {
        out.extend(format!("{}:", k.len()).as_bytes());
        out.extend(&k);
        out.extend(v);
    }
    out.push(b'e');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un petit .torrent v1 valide.
    pub fn sample(name: &str, length: u32, private: bool) -> Vec<u8> {
        let mut v = b"d8:announce23:http://tracker.test/ann4:infod".to_vec();
        v.extend(
            format!(
                "6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
                name.len()
            )
            .bytes(),
        );
        v.extend([7u8; 20]);
        if private {
            v.extend(b"7:privatei1e");
        }
        v.extend(b"ee");
        v
    }

    #[test]
    fn parses_name_size_trackers_and_computes_the_infohash() {
        let t = sample("film.mkv", 123_456, true);
        let m = parse(&t).unwrap();
        assert_eq!(
            (m.name.as_str(), m.size, m.file_count, m.private),
            ("film.mkv", 123_456, 1, true)
        );
        assert_eq!(m.trackers, vec!["http://tracker.test/ann"]);
        // L'infohash est le SHA-1 du dictionnaire `info` tel qu'il figure dans le fichier.
        let start = t.windows(7).position(|w| w == b"4:infod").unwrap() + 6;
        let info = &t[start..t.len() - 1];
        assert_eq!(
            m.infohash,
            data_encoding::HEXLOWER.encode(&Sha1::digest(info))
        );
        assert_eq!(m.infohash.len(), 40);
        // Public : `private` absent.
        assert!(!parse(&sample("a", 1, false)).unwrap().private);
    }

    #[test]
    fn multi_file_torrents_sum_their_lengths() {
        let mut t = b"d4:infod5:filesld6:lengthi10e4:pathl1:aeed6:lengthi32e4:pathl1:beee4:name3:dir12:piece lengthi16384e6:pieces20:".to_vec();
        t.extend([0u8; 20]);
        t.extend(b"ee");
        let m = parse(&t).unwrap();
        assert_eq!((m.size, m.file_count, m.name.as_str()), (42, 2, "dir"));
    }

    #[test]
    fn rejects_malformed_oversized_and_unsupported_input() {
        for (bad, why) in [
            (b"".to_vec(), "vide"),
            (b"hello".to_vec(), "pas du bencode"),
            (b"i42e".to_vec(), "racine non dictionnaire"),
            (b"d4:spami1ee".to_vec(), "pas d'info"),
            (
                b"d4:infod4:name1:aee".to_vec(),
                "ni length ni files, pas de pieces",
            ),
            (
                b"d4:infod6:lengthi5e4:name1:a6:pieces0:ee".to_vec(),
                "ok mais trailing ci-dessous",
            ),
        ] {
            if why.contains("trailing") {
                let mut t = bad.clone();
                t.extend(b"x");
                assert!(parse(&t).is_err(), "{why}");
            } else {
                assert!(parse(&bad).is_err(), "{why}");
            }
        }
        assert!(parse(&[b'l'; 100]).is_err(), "imbrication excessive");
        assert_eq!(
            parse(&vec![0u8; MAX_TORRENT_FILE + 1]),
            Err(MetainfoError::TooLarge)
        );
        assert!(
            parse(b"d4:infod6:lengthi-5e4:name1:a6:pieces0:ee").is_err(),
            "taille négative"
        );
        assert!(
            parse(b"999999999999:abc").is_err(),
            "longueur annoncée démesurée sans allocation"
        );
        // v2 pur : pas de `pieces`.
        assert_eq!(
            parse(b"d4:infod6:lengthi5e4:name1:aee"),
            Err(MetainfoError::V2Only)
        );
    }

    #[test]
    fn control_characters_and_odd_trackers_are_filtered() {
        let mut t = b"d8:announce16:file:///etc/pass4:infod6:lengthi5e4:name4:a\x00\x07b12:piece lengthi1e6:pieces20:".to_vec();
        t.extend([1u8; 20]);
        t.extend(b"ee");
        let m = parse(&t).unwrap();
        assert_eq!(m.name, "ab");
        assert!(m.trackers.is_empty(), "seuls http(s)/udp sont conservés");
    }

    #[test]
    fn rewriting_the_announce_keeps_the_infohash_and_drops_every_other_source() {
        // Torrent avec plusieurs sources à retirer : announce-list, nodes, url-list, httpseeds.
        let mut v = b"d8:announce23:http://tracker.test/ann13:announce-listll19:http://other.test/aee5:nodesll7:1.2.3.4i1eee8:url-listl18:http://web.seed/x/e9:httpseedsl16:http://hs.test/ae4:infod".to_vec();
        v.extend(b"6:lengthi16384e4:name4:test12:piece lengthi16384e6:pieces20:");
        v.extend([7u8; 20]);
        v.extend(b"7:privatei1eee");
        let before = parse(&v).unwrap();
        assert_eq!(before.trackers.len(), 2);
        let out = rewrite_announce(&v, "http://master.test:7444/announce/n1/tok").unwrap();
        let after = parse(&out).unwrap();
        assert_eq!(after.infohash, before.infohash, "même infohash");
        assert_eq!(
            after.trackers,
            vec!["http://master.test:7444/announce/n1/tok".to_owned()]
        );
        assert!(after.private && after.name == "test" && after.size == 16384);
        for gone in [
            &b"other.test"[..],
            b"1.2.3.4",
            b"web.seed",
            b"hs.test",
            b"tracker.test",
        ] {
            assert!(
                !out.windows(gone.len()).any(|w| w == gone),
                "{:?} subsiste",
                String::from_utf8_lossy(gone)
            );
        }
        // Idempotent et entrées invalides refusées.
        let twice = rewrite_announce(&out, "http://master.test:7444/announce/n1/tok").unwrap();
        assert_eq!(twice, out);
        assert!(rewrite_announce(&v, "").is_err());
        assert!(rewrite_announce(&v, "http://a b").is_err());
        assert!(rewrite_announce(b"pas un torrent", "http://x").is_err());
    }

    /// Torrent multi-fichiers : `name` = dossier racine, `entries` = (composants du chemin, taille).
    fn multi(name: &str, entries: &[(&[&str], u64)]) -> Vec<u8> {
        let mut v = b"d4:infod5:filesl".to_vec();
        for (path, len) in entries {
            v.extend(format!("d6:lengthi{len}e4:pathl").bytes());
            for c in *path {
                v.extend(format!("{}:{c}", c.len()).bytes());
            }
            v.extend(b"ee");
        }
        v.extend(
            format!(
                "e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
                name.len()
            )
            .bytes(),
        );
        v.extend([1u8; 20]);
        v.extend(b"ee");
        v
    }

    #[test]
    fn files_are_listed_and_the_content_key_ignores_order_and_root_folder() {
        let a = parse(&multi(
            "Film.2024.1080p",
            &[(&["video.mkv"], 1000), (&["Subs", "fr.srt"], 5)],
        ))
        .unwrap();
        assert_eq!(
            a.files,
            vec![
                FileEntry {
                    path: "video.mkv".into(),
                    size: 1000
                },
                FileEntry {
                    path: "Subs/fr.srt".into(),
                    size: 5
                },
            ]
        );
        // Autre tracker : autre dossier racine, autre ordre, mêmes fichiers -> même contenu.
        let b = parse(&multi(
            "Film.2024.1080p-GRP",
            &[(&["Subs", "fr.srt"], 5), (&["video.mkv"], 1000)],
        ))
        .unwrap();
        assert_ne!(a.name, b.name);
        assert_eq!(a.content_key(), b.content_key());
        assert!(a.content_key().is_some());
        // Un octet de plus, ou un autre chemin : autre contenu.
        let c = parse(&multi(
            "x",
            &[(&["video.mkv"], 1001), (&["Subs", "fr.srt"], 5)],
        ))
        .unwrap();
        let d = parse(&multi(
            "x",
            &[(&["video.mkv"], 1000), (&["Subs", "en.srt"], 5)],
        ))
        .unwrap();
        assert_ne!(a.content_key(), c.content_key());
        assert_ne!(a.content_key(), d.content_key());
        // Séparation non ambiguë : ("ab", 1) n'égale pas ("a", ...) + octets de taille voisins.
        let e = parse(&multi("x", &[(&["a", "b"], 1)])).unwrap();
        let f = parse(&multi("x", &[(&["ab"], 1)])).unwrap();
        assert_ne!(e.content_key(), f.content_key());
    }

    #[test]
    fn a_single_file_torrent_lists_itself_under_its_name() {
        let m = parse(&sample("film.mkv", 4096, false)).unwrap();
        assert_eq!(
            m.files,
            vec![FileEntry {
                path: "film.mkv".into(),
                size: 4096
            }]
        );
        assert!(m.content_key().is_some());
    }

    #[test]
    fn unsafe_file_paths_are_refused() {
        for bad in [
            multi("x", &[(&["..", "etc"], 1)]),
            multi("x", &[(&["a/b"], 1)]),
            multi("x", &[(&[r"a\b"], 1)]),
            multi("x", &[(&["."], 1)]),
            multi("x", &[(&[""], 1)]),
            multi("x", &[(&[], 1)]),
        ] {
            assert!(parse(&bad).is_err(), "{:?}", String::from_utf8_lossy(&bad));
        }
    }

    fn mi(name: &str, files: &[(&str, u64)]) -> Metainfo {
        let entries: Vec<(Vec<&str>, u64)> = files
            .iter()
            .map(|(p, l)| (p.split('/').collect(), *l))
            .collect();
        let refs: Vec<(&[&str], u64)> = entries.iter().map(|(p, l)| (p.as_slice(), *l)).collect();
        parse(&multi(name, &refs)).unwrap()
    }

    #[test]
    fn a_different_root_folder_links_every_file_one_to_one() {
        let a = mi(
            "Film.2024.1080p",
            &[("video.mkv", 1000), ("Subs/fr.srt", 5)],
        );
        let b = mi(
            "Film.2024.1080p-GRP",
            &[("video.mkv", 1000), ("Subs/fr.srt", 5)],
        );
        assert!(
            needs_links(&a, &b),
            "même contenu mais autre racine : liens nécessaires"
        );
        let mut v = plan_links(&a, &b).unwrap();
        v.sort_by(|x, y| x.src.cmp(&y.src));
        assert_eq!(
            v,
            vec![
                LinkPair {
                    src: "Subs/fr.srt".into(),
                    dst: "Subs/fr.srt".into()
                },
                LinkPair {
                    src: "video.mkv".into(),
                    dst: "video.mkv".into()
                },
            ]
        );
        // Même disposition ET mêmes empreintes de pièces : pas de liens, les deux torrents partagent les fichiers.
        let c = mi(
            "Film.2024.1080p",
            &[("Subs/fr.srt", 5), ("video.mkv", 1000)],
        );
        assert!(!needs_links(&a, &c));
        // Même disposition mais empreintes différentes : contenu non PROUVÉ identique -> arbre de liens isolé.
        let mut d = c.clone();
        d.pieces_digest = "autre".into();
        assert!(needs_links(&a, &d));
    }

    #[test]
    fn renamed_files_are_matched_by_unique_size_then_by_unique_name() {
        let a = mi(
            "D",
            &[
                ("a/video.mkv", 1000),
                ("a/sous-titres.srt", 5),
                ("extras/b.nfo", 7),
            ],
        );
        // Fichiers renommés, tailles toutes distinctes : appariés par taille.
        let b = mi(
            "D2",
            &[("Film.mkv", 1000), ("Film.srt", 5), ("Film.nfo", 7)],
        );
        let v = plan_links(&a, &b).unwrap();
        assert!(v.contains(&LinkPair {
            src: "a/video.mkv".into(),
            dst: "Film.mkv".into()
        }));
        assert!(v.contains(&LinkPair {
            src: "extras/b.nfo".into(),
            dst: "Film.nfo".into()
        }));
        // Deux fichiers de même taille : départagés par le nom s'il est unique...
        let c = mi("X", &[("x/ep1.mkv", 500), ("x/ep2.mkv", 500)]);
        let d = mi("Y", &[("y/ep2.mkv", 500), ("y/ep1.mkv", 500)]);
        let v = plan_links(&c, &d).unwrap();
        assert!(
            v.contains(&LinkPair {
                src: "x/ep1.mkv".into(),
                dst: "y/ep1.mkv".into()
            }),
            "{v:?}"
        );
        // ...sinon c'est ambigu : refus (on ne devine pas quel fichier est lequel).
        let e = mi("Z", &[("z/aaa.mkv", 500), ("z/bbb.mkv", 500)]);
        assert_eq!(plan_links(&c, &e), None);
    }

    #[test]
    fn contents_that_do_not_match_file_by_file_are_never_linked() {
        let a = mi("D", &[("video.mkv", 1000), ("fr.srt", 5)]);
        for other in [
            mi("D", &[("video.mkv", 1001), ("fr.srt", 5)]), // une taille diffère
            mi("D", &[("video.mkv", 1000)]),                // un fichier de moins
            mi("D", &[("video.mkv", 1000), ("fr.srt", 5), ("x", 1)]), // un fichier de plus
            mi("D", &[("video.mkv", 1000), ("en.srt", 6)]), // autre fichier
        ] {
            assert_eq!(plan_links(&a, &other), None);
        }
        // Mono-fichier : renommé, même taille -> un lien ; taille différente -> rien.
        let s1 = parse(&sample("film.mkv", 4096, false)).unwrap();
        let s2 = parse(&sample("Film.2024.mkv", 4096, false)).unwrap();
        assert_eq!(
            plan_links(&s1, &s2).unwrap(),
            vec![LinkPair {
                src: "film.mkv".into(),
                dst: "Film.2024.mkv".into()
            }]
        );
        assert!(plan_links(&s1, &parse(&sample("film.mkv", 4097, false)).unwrap()).is_none());
    }

    #[test]
    fn too_many_files_are_never_linked() {
        let many: Vec<(String, u64)> = (0..=MAX_LINKED_FILES)
            .map(|i| (format!("f{i}"), 1 + i as u64))
            .collect();
        let refs: Vec<(&str, u64)> = many.iter().map(|(p, l)| (p.as_str(), *l)).collect();
        let a = mi("A", &refs);
        let b = mi("B", &refs);
        assert_eq!(plan_links(&a, &b), None);
    }

    #[test]
    fn compose_adds_trackers_and_metadata_without_touching_the_infohash() {
        let base = sample("film.mkv", 4096, true);
        let m0 = parse(&base).unwrap();
        let tiers = vec![
            vec![
                "https://tracker.test/announce?pk=abc".to_owned(),
                "udp://t2.test:6969/announce".to_owned(),
            ],
            vec!["http://backup.test/ann".to_owned()],
        ];
        let out = compose(
            &base,
            &tiers,
            Some("Mon commentaire"),
            "nxtTorrent 0.1",
            Some(1_700_000_000),
        )
        .unwrap();
        let m = parse(&out).unwrap();
        assert_eq!(m.infohash, m0.infohash, "l'infohash ne change pas");
        assert_eq!(
            m.trackers,
            vec![
                "https://tracker.test/announce?pk=abc",
                "udp://t2.test:6969/announce",
                "http://backup.test/ann"
            ]
        );
        assert!(out.windows(15).any(|w| w == b"Mon commentaire"));
        assert!(out.windows(14).any(|w| w == b"nxtTorrent 0.1"));
        assert!(out.windows(21).any(|w| w == b"creation datei1700000"));
        // Un seul tracker : pas d'announce-list ; recomposer remplace tout (rien de l'ancien ne subsiste).
        let one = compose(
            &out,
            &[vec!["http://seul.test/a".to_owned()]],
            None,
            "",
            None,
        )
        .unwrap();
        assert!(!one.windows(13).any(|w| w == b"announce-list"));
        assert_eq!(parse(&one).unwrap().trackers, vec!["http://seul.test/a"]);
        assert!(!one.windows(9).any(|w| w == b"t2.test:6"));
        assert_eq!(parse(&one).unwrap().infohash, m0.infohash);
        // Sans tracker : possible pour un torrent public (DHT) seulement.
        let public = sample("p.bin", 100, false);
        assert!(compose(&public, &[], None, "", None).is_ok());
        assert!(
            compose(&base, &[], None, "", None).is_err(),
            "un torrent privé exige un tracker"
        );
    }

    #[test]
    fn compose_refuses_dangerous_or_malformed_values() {
        let base = sample("film.mkv", 4096, false);
        let t = |u: &str| vec![vec![u.to_owned()]];
        for bad in [
            "ftp://x.test/a",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "http://a b",
            "http://x\n.test",
            "",
            "http://",
            &format!("http://x.test/{}", "a".repeat(600)),
        ] {
            assert!(compose(&base, &t(bad), None, "", None).is_err(), "{bad:?}");
        }
        let many: Vec<Vec<String>> = (0..=MAX_TRACKERS_CREATED)
            .map(|i| vec![format!("http://t{i}.test/a")])
            .collect();
        assert!(
            compose(&base, &many, None, "", None).is_err(),
            "trop de trackers"
        );
        assert!(
            compose(&base, &[vec![]], None, "", None).is_err(),
            "palier vide"
        );
        assert!(
            compose(&base, &t("http://ok.test/a"), Some("a\nb"), "", None).is_err(),
            "contrôle dans le commentaire"
        );
        assert!(compose(
            &base,
            &t("http://ok.test/a"),
            Some(&"c".repeat(501)),
            "",
            None
        )
        .is_err());
        assert!(compose(&base, &t("http://ok.test/a"), None, &"c".repeat(65), None).is_err());
        assert!(compose(b"pas un torrent", &t("http://ok.test/a"), None, "", None).is_err());
    }
}
