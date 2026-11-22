//! Création d'un fichier `.torrent` (v1) à partir de fichiers présents sur disque : parcours, hachage des pièces (SHA-1),
//! écriture bencode. On ne réimplémente PAS BitTorrent : on fabrique seulement les métadonnées qu'un client existant lira.
//!
//! Sortie : `d4:infod…ee` (le dictionnaire `info` seul, sans adresse de tracker). Les adresses, le commentaire, etc. sont
//! des clés de premier niveau HORS de `info` : elles s'ajoutent ensuite (`metainfo::compose`) sans changer l'infohash. Seuls
//! `private` et `source` vivent dans `info` (ils changent l'infohash, c'est voulu : c'est ce qui distingue un torrent
//! d'un autre tracker pour les mêmes fichiers).
//!
//! Sûreté : aucun lien symbolique n'est suivi (refusé, jamais ignoré en silence : le torrent serait incomplet), les noms non
//! UTF-8 sont refusés, un fichier qui change pendant la lecture fait échouer l'opération, les dossiers internes
//! `.nxt-xseed` (liens du cross-seed) sont ignorés, la taille du résultat est bornée.
use crate::metainfo::{MAX_LISTED_FILES, MAX_TORRENT_FILE};
use sha1::{Digest, Sha1};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const MIN_PIECE: u32 = 16 * 1024;
pub const MAX_PIECE: u32 = 16 * 1024 * 1024;
/// Nombre de pièces visé par le choix automatique de leur taille.
const TARGET_PIECES: u64 = 2048;
/// Dossier interne du cross-seed (liens physiques) : jamais inclus dans un torrent.
pub const INTERNAL_DIR: &str = ".nxt-xseed";

#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    /// Taille d'une pièce (puissance de deux, 16 Kio à 16 Mio) ; `None` = choisie selon la taille du contenu.
    pub piece_length: Option<u32>,
    /// Torrent privé (pas de DHT ni d'échange de pairs).
    pub private: bool,
    /// Étiquette `source` (trackers privés : distingue l'infohash d'un tracker à l'autre). 64 caractères au plus.
    pub source: Option<String>,
    /// Nom du torrent (fichier ou dossier racine) ; par défaut, celui du chemin.
    pub name: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("chemin introuvable ou illisible")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("lien symbolique refusé : {0}")]
    Symlink(String),
    #[error("le contenu a changé pendant la lecture : {0}")]
    Changed(String),
    #[error("contenu trop volumineux : le fichier .torrent dépasserait 4 Mio")]
    TooLarge,
    #[error("opération annulée")]
    Cancelled,
    #[error("lecture impossible : {0}")]
    Io(String),
}

struct Entry {
    /// Composants relatifs au dossier racine du torrent.
    parts: Vec<String>,
    path: PathBuf,
    len: u64,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s != "."
        && s != ".."
        && !s.contains(['/', '\\'])
        && !s.chars().any(char::is_control)
}

fn walk(dir: &Path, parts: &mut Vec<String>, out: &mut Vec<Entry>) -> Result<(), CreateError> {
    let rd = std::fs::read_dir(dir).map_err(|_| CreateError::NotFound)?;
    let mut items: Vec<_> = rd.filter_map(Result::ok).collect();
    items.sort_by_key(std::fs::DirEntry::file_name);
    for it in items {
        let raw = it.file_name();
        let Some(name) = raw.to_str().map(str::to_owned) else {
            return Err(CreateError::Invalid(
                "un nom de fichier n'est pas de l'UTF-8 valide".into(),
            ));
        };
        if name == INTERNAL_DIR {
            continue;
        }
        parts.push(name.clone());
        let rel = parts.join("/");
        if !name_ok(&name) {
            return Err(CreateError::Invalid(format!(
                "nom de fichier non pris en charge : {rel}"
            )));
        }
        let meta =
            std::fs::symlink_metadata(it.path()).map_err(|e| CreateError::Io(e.to_string()))?;
        if meta.file_type().is_symlink() {
            return Err(CreateError::Symlink(rel));
        }
        if meta.is_dir() {
            walk(&it.path(), parts, out)?;
        } else if meta.is_file() {
            if out.len() >= MAX_LISTED_FILES {
                return Err(CreateError::Invalid(format!(
                    "plus de {MAX_LISTED_FILES} fichiers"
                )));
            }
            out.push(Entry {
                parts: parts.clone(),
                path: it.path(),
                len: meta.len(),
            });
        } else {
            return Err(CreateError::Invalid(format!(
                "fichier spécial non pris en charge : {rel}"
            )));
        }
        parts.pop();
    }
    Ok(())
}

/// Taille de pièce automatique : la plus petite puissance de deux qui donne au plus ~2 048 pièces (bornes 16 Kio, 16 Mio).
pub fn auto_piece_length(total: u64) -> u32 {
    let mut p = MIN_PIECE;
    while p < MAX_PIECE && total.div_ceil(u64::from(p)) > TARGET_PIECES {
        p *= 2;
    }
    p
}

fn bstr(out: &mut Vec<u8>, s: &[u8]) {
    out.extend(s.len().to_string().bytes());
    out.push(b':');
    out.extend(s);
}

/// Crée le `.torrent` de `path` (un fichier, ou un dossier et tout ce qu'il contient). `progress(fait, total)` est appelé
/// régulièrement ; `cancel` interrompt l'opération entre deux pièces.
pub fn create(
    path: &Path,
    opts: &CreateOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, CreateError> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| CreateError::NotFound)?;
    if meta.file_type().is_symlink() {
        return Err(CreateError::Symlink(
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        ));
    }
    let default_name = path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    let name = match (&opts.name, default_name) {
        (Some(n), _) => n.clone(),
        (None, Some(n)) => n,
        (None, None) => {
            return Err(CreateError::Invalid(
                "indiquez un nom : ce chemin n'en a pas".into(),
            ))
        }
    };
    if !name_ok(&name) {
        return Err(CreateError::Invalid("nom du torrent invalide".into()));
    }
    if let Some(s) = &opts.source {
        if s.is_empty() || s.len() > 64 || s.chars().any(char::is_control) {
            return Err(CreateError::Invalid(
                "étiquette « source » invalide (1 à 64 caractères)".into(),
            ));
        }
    }

    let single = meta.is_file();
    let mut files: Vec<Entry> = Vec::new();
    if single {
        files.push(Entry {
            parts: vec![name.clone()],
            path: path.to_owned(),
            len: meta.len(),
        });
    } else if meta.is_dir() {
        walk(path, &mut Vec::new(), &mut files)?;
        files.sort_by(|a, b| a.parts.cmp(&b.parts));
    } else {
        return Err(CreateError::Invalid("ni un fichier ni un dossier".into()));
    }
    let total: u64 = files.iter().map(|f| f.len).sum();
    if files.is_empty() || total == 0 {
        return Err(CreateError::Invalid(
            "aucun contenu à partager (dossier vide ou fichiers vides)".into(),
        ));
    }
    let piece_len = match opts.piece_length {
        Some(p) if (MIN_PIECE..=MAX_PIECE).contains(&p) && p.is_power_of_two() => p,
        Some(_) => {
            return Err(CreateError::Invalid(
                "taille de pièce : puissance de deux de 16 Kio à 16 Mio".into(),
            ))
        }
        None => auto_piece_length(total),
    };
    // Borne le fichier final AVANT de lire des téraoctets : 20 octets par pièce + ~100 par fichier.
    let pieces_count = total.div_ceil(u64::from(piece_len));
    if pieces_count * 20 + files.len() as u64 * 120 + 1024 > MAX_TORRENT_FILE as u64 {
        return Err(CreateError::TooLarge);
    }

    // ---- Hachage : pièces consécutives sur le flux concaténé des fichiers, dans l'ordre.
    let mut pieces: Vec<u8> = Vec::with_capacity(usize::try_from(pieces_count).unwrap_or(0) * 20);
    let mut buf = vec![0u8; 256 * 1024];
    let mut hasher = Sha1::new();
    let mut in_piece: u64 = 0;
    let mut done: u64 = 0;
    progress(0, total);
    for f in &files {
        let rel = f.parts.join("/");
        let mut fh = File::open(&f.path).map_err(|e| CreateError::Io(format!("{rel} : {e}")))?;
        let mut read_total: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(CreateError::Cancelled);
            }
            let n = fh
                .read(&mut buf)
                .map_err(|e| CreateError::Io(format!("{rel} : {e}")))?;
            if n == 0 {
                break;
            }
            read_total += n as u64;
            if read_total > f.len {
                return Err(CreateError::Changed(rel));
            }
            let mut chunk = &buf[..n];
            while !chunk.is_empty() {
                let room = u64::from(piece_len) - in_piece;
                let take = usize::try_from(room.min(chunk.len() as u64)).unwrap_or(chunk.len());
                hasher.update(&chunk[..take]);
                in_piece += take as u64;
                chunk = &chunk[take..];
                if in_piece == u64::from(piece_len) {
                    pieces.extend(hasher.finalize_reset());
                    in_piece = 0;
                }
            }
            done += n as u64;
            progress(done, total);
        }
        if read_total != f.len {
            return Err(CreateError::Changed(rel));
        }
    }
    if in_piece > 0 {
        pieces.extend(hasher.finalize());
    }

    // ---- Bencode : clés du dictionnaire `info` triées (files, length, name, piece length, pieces, private, source).
    let mut info = Vec::with_capacity(pieces.len() + files.len() * 100 + 256);
    info.push(b'd');
    if single {
        info.extend(format!("6:lengthi{total}e").bytes());
    } else {
        bstr(&mut info, b"files");
        info.push(b'l');
        for f in &files {
            info.extend(format!("d6:lengthi{}e4:pathl", f.len).bytes());
            for c in &f.parts {
                bstr(&mut info, c.as_bytes());
            }
            info.extend(b"ee");
        }
        info.push(b'e');
    }
    bstr(&mut info, b"name");
    bstr(&mut info, name.as_bytes());
    info.extend(format!("12:piece lengthi{piece_len}e").bytes());
    bstr(&mut info, b"pieces");
    bstr(&mut info, &pieces);
    if opts.private {
        info.extend(b"7:privatei1e");
    }
    if let Some(s) = &opts.source {
        bstr(&mut info, b"source");
        bstr(&mut info, s.as_bytes());
    }
    info.push(b'e');

    let mut out = Vec::with_capacity(info.len() + 16);
    out.extend(b"d4:info");
    out.extend(info);
    out.push(b'e');
    if out.len() > MAX_TORRENT_FILE {
        return Err(CreateError::TooLarge);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metainfo::parse;

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }
    fn data(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }
    fn sha1s(d: &[u8], piece: usize) -> Vec<u8> {
        d.chunks(piece)
            .flat_map(|c| Sha1::digest(c).to_vec())
            .collect()
    }
    fn make(path: &Path, o: &CreateOptions) -> Result<Vec<u8>, CreateError> {
        create(path, o, &no_cancel(), &mut |_, _| {})
    }

    #[test]
    fn a_single_file_torrent_has_the_right_pieces_and_parses_back() {
        let t = tempfile::tempdir().unwrap();
        let d = data(1, 100_000);
        std::fs::write(t.path().join("film.mkv"), &d).unwrap();
        let o = CreateOptions {
            piece_length: Some(32768),
            ..CreateOptions::default()
        };
        let b = make(&t.path().join("film.mkv"), &o).unwrap();
        let m = parse(&b).unwrap();
        assert_eq!(
            (m.name.as_str(), m.size, m.single_file),
            ("film.mkv", 100_000, true)
        );
        assert!(!m.private);
        // Les empreintes de pièces sont EXACTEMENT celles du contenu (recalculées ici indépendamment).
        let expect = sha1s(&d, 32768);
        assert!(
            b.windows(expect.len()).any(|w| w == expect.as_slice()),
            "empreintes de pièces attendues"
        );
    }

    #[test]
    fn a_folder_becomes_a_multi_file_torrent_in_sorted_order_with_pieces_spanning_files() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Film");
        std::fs::create_dir_all(root.join("Subs")).unwrap();
        let (a, b, c) = (data(2, 40_000), data(3, 1_000), Vec::new());
        std::fs::write(root.join("video.mkv"), &a).unwrap();
        std::fs::write(root.join("Subs/fr.srt"), &b).unwrap();
        std::fs::write(root.join("vide.txt"), &c).unwrap(); // un fichier vide est un fichier du torrent
        let o = CreateOptions {
            piece_length: Some(16384),
            ..CreateOptions::default()
        };
        let bytes = make(&root, &o).unwrap();
        let m = parse(&bytes).unwrap();
        assert_eq!(
            (m.name.as_str(), m.size, m.single_file),
            ("Film", 41_000, false)
        );
        let paths: Vec<&str> = m.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["Subs/fr.srt", "vide.txt", "video.mkv"],
            "ordre trié et déterministe"
        );
        // Les pièces portent sur le flux CONCATÉNÉ (dans l'ordre trié), pas fichier par fichier.
        let all: Vec<u8> = [b.clone(), c.clone(), a.clone()].concat();
        let expect = sha1s(&all, 16384);
        assert!(bytes.windows(expect.len()).any(|w| w == expect.as_slice()));
        // Déterminisme : mêmes entrées, mêmes octets.
        assert_eq!(bytes, make(&root, &o).unwrap());
    }

    #[test]
    fn private_and_source_change_the_infohash_not_the_content() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a.bin"), data(4, 70_000)).unwrap();
        let p = t.path().join("a.bin");
        let plain = parse(&make(&p, &CreateOptions::default()).unwrap()).unwrap();
        let private = parse(
            &make(
                &p,
                &CreateOptions {
                    private: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
        let src = parse(
            &make(
                &p,
                &CreateOptions {
                    private: true,
                    source: Some("MonTracker".into()),
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert!(private.private && src.private && !plain.private);
        assert_ne!(plain.infohash, private.infohash);
        assert_ne!(private.infohash, src.infohash);
        assert_eq!(
            plain.content_key(),
            src.content_key(),
            "mêmes fichiers : c'est un cross-seed potentiel"
        );
        assert_eq!(
            plain.pieces_digest, private.pieces_digest,
            "mêmes données donc mêmes pièces"
        );
    }

    #[test]
    fn the_piece_length_is_validated_and_chosen_automatically() {
        assert_eq!(auto_piece_length(1), MIN_PIECE);
        assert_eq!(auto_piece_length(2048 * 16384), 16384);
        assert_eq!(auto_piece_length(2048 * 16384 + 1), 32768);
        assert_eq!(auto_piece_length(u64::MAX), MAX_PIECE);
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a"), data(5, 5000)).unwrap();
        for bad in [0, 1000, 16384 + 1, MIN_PIECE / 2, MAX_PIECE * 2, 3 * 16384] {
            let o = CreateOptions {
                piece_length: Some(bad),
                ..Default::default()
            };
            assert!(make(&t.path().join("a"), &o).is_err(), "{bad}");
        }
    }

    #[test]
    fn hostile_or_unusable_inputs_are_refused() {
        let t = tempfile::tempdir().unwrap();
        assert!(matches!(
            make(&t.path().join("absent"), &Default::default()),
            Err(CreateError::NotFound)
        ));
        std::fs::create_dir(t.path().join("vide")).unwrap();
        assert!(
            make(&t.path().join("vide"), &Default::default()).is_err(),
            "dossier vide"
        );
        std::fs::write(t.path().join("zero"), b"").unwrap();
        assert!(
            make(&t.path().join("zero"), &Default::default()).is_err(),
            "contenu de taille nulle"
        );
        std::fs::write(t.path().join("ok"), b"x").unwrap();
        for name in ["", "a/b", "..", ".", "x\ny", &"n".repeat(300)] {
            let o = CreateOptions {
                name: Some(name.into()),
                ..Default::default()
            };
            assert!(make(&t.path().join("ok"), &o).is_err(), "nom {name:?}");
        }
        for src in ["", "x\0y", &"s".repeat(65)] {
            let o = CreateOptions {
                source: Some(src.into()),
                ..Default::default()
            };
            assert!(make(&t.path().join("ok"), &o).is_err(), "source {src:?}");
        }
    }

    #[test]
    fn the_internal_cross_seed_folder_is_never_included() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("D");
        std::fs::create_dir_all(root.join(INTERNAL_DIR).join("abc")).unwrap();
        std::fs::write(root.join("a.bin"), data(6, 3000)).unwrap();
        std::fs::write(root.join(INTERNAL_DIR).join("abc/copie.bin"), data(6, 3000)).unwrap();
        let m = parse(&make(&root, &Default::default()).unwrap()).unwrap();
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path, "a.bin");
    }

    #[test]
    fn symbolic_links_are_refused_not_silently_skipped() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("D");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.bin"), data(7, 3000)).unwrap();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(root.join("a.bin"), root.join("lien")).is_ok();
        #[cfg(windows)]
        let made =
            std::os::windows::fs::symlink_file(root.join("a.bin"), root.join("lien")).is_ok();
        if !made {
            eprintln!("SYMLINKS INDISPONIBLES : test non exécuté");
            return;
        }
        assert!(
            matches!(make(&root, &Default::default()), Err(CreateError::Symlink(n)) if n == "lien")
        );
        assert!(matches!(
            make(&root.join("lien"), &Default::default()),
            Err(CreateError::Symlink(_))
        ));
    }

    #[test]
    fn progress_is_monotonic_reaches_the_total_and_cancel_stops_the_work() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("big"), data(8, 3_000_000)).unwrap();
        let mut seen: Vec<u64> = Vec::new();
        create(
            &t.path().join("big"),
            &Default::default(),
            &no_cancel(),
            &mut |d, tot| {
                assert_eq!(tot, 3_000_000);
                seen.push(d);
            },
        )
        .unwrap();
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1])
                && *seen.last().unwrap() == 3_000_000
                && seen[0] == 0
        );
        // Annulation demandée pendant la lecture : arrêt net, aucun résultat.
        let cancel = AtomicBool::new(false);
        let r = create(
            &t.path().join("big"),
            &Default::default(),
            &cancel,
            &mut |d, _| {
                if d > 500_000 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(matches!(r, Err(CreateError::Cancelled)));
    }

    #[test]
    fn a_torrent_that_would_exceed_the_size_limit_is_refused_before_reading_anything() {
        // 4 Mio de torrent = ~200 000 pièces de 16 Mio ... on simule par la borne : une taille énorme, sans la lire.
        let n = (MAX_TORRENT_FILE as u64) / 20 * u64::from(MAX_PIECE) + u64::from(MAX_PIECE);
        assert!(n.div_ceil(u64::from(MAX_PIECE)) * 20 + 1024 > MAX_TORRENT_FILE as u64);
        assert_eq!(auto_piece_length(n), MAX_PIECE);
    }
}
