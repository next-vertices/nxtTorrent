//! Aides communes aux faux clients : infohash d'un lien magnet / d'un fichier .torrent, torrent de test.

pub fn hash_from_magnet(m: &str) -> Option<String> {
    let i = m.find("btih:")?;
    Some(
        m[i + 5..]
            .chars()
            .take_while(|c| *c != '&')
            .collect::<String>()
            .to_lowercase(),
    )
}

pub fn hash_from_file(bytes: &[u8]) -> Option<String> {
    proto::metainfo::parse(bytes).ok().map(|m| m.infohash)
}

pub fn hash_from_b64(b64: &str) -> Option<String> {
    let bytes = data_encoding::BASE64.decode(b64.trim().as_bytes()).ok()?;
    hash_from_file(&bytes)
}

/// Fichier .torrent v1 valide et unique (`tag` change l'infohash).
pub fn sample(name: &str, length: u32, tag: u8) -> Vec<u8> {
    let mut v = b"d8:announce23:http://tracker.test/ann4:infod".to_vec();
    v.extend(
        format!(
            "6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
            name.len()
        )
        .bytes(),
    );
    v.extend([tag; 20]);
    v.extend(b"ee");
    v
}

/// Torrent v1 mono-fichier correspondant EXACTEMENT à `data` (vraies empreintes SHA-1 par pièce) : un vrai client
/// peut le télécharger et le vérifier. `announce` = URL d'annonce du tracker.
pub fn for_data(
    name: &str,
    data: &[u8],
    piece_len: usize,
    announce: &str,
    private: bool,
) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    let pieces: Vec<u8> = data
        .chunks(piece_len)
        .flat_map(|c| Sha1::digest(c).to_vec())
        .collect();
    let mut v = format!("d8:announce{}:{announce}4:infod", announce.len()).into_bytes();
    v.extend(
        format!(
            "6:lengthi{}e4:name{}:{name}12:piece lengthi{piece_len}e6:pieces{}:",
            data.len(),
            name.len(),
            pieces.len()
        )
        .bytes(),
    );
    v.extend(pieces);
    if private {
        v.extend(b"7:privatei1e");
    }
    v.extend(b"ee");
    v
}

/// Torrent v1 MULTI-fichiers : dossier racine `name`, fichiers `(chemin relatif, contenu)` dans l'ordre donné, vraies
/// empreintes SHA-1 sur le contenu concaténé.
pub fn for_files(
    name: &str,
    files: &[(&str, &[u8])],
    piece_len: usize,
    announce: &str,
    private: bool,
) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    let all: Vec<u8> = files.iter().flat_map(|(_, d)| d.iter().copied()).collect();
    let pieces: Vec<u8> = all
        .chunks(piece_len)
        .flat_map(|c| Sha1::digest(c).to_vec())
        .collect();
    let mut v = format!("d8:announce{}:{announce}4:infod5:filesl", announce.len()).into_bytes();
    for (path, data) in files {
        v.extend(format!("d6:lengthi{}e4:pathl", data.len()).bytes());
        for c in path.split('/') {
            v.extend(format!("{}:{c}", c.len()).bytes());
        }
        v.extend(b"ee");
    }
    v.extend(
        format!(
            "e4:name{}:{name}12:piece lengthi{piece_len}e6:pieces{}:",
            name.len(),
            pieces.len()
        )
        .bytes(),
    );
    v.extend(pieces);
    if private {
        v.extend(b"7:privatei1e");
    }
    v.extend(b"ee");
    v
}
