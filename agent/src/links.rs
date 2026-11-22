//! Commande `LinkTree` : crée, SOUS la racine locale de l'agent, des liens physiques (hardlinks) vers des fichiers déjà
//! présents, pour qu'un autre torrent (autre tracker, autre dossier racine ou autres noms) les retrouve sous SA
//! disposition. Aucune donnée n'est copiée ni déplacée, aucun fichier n'est jamais écrasé ni supprimé.
//!
//! Le master n'est pas cru sur parole : chaque chemin est relatif et validé (ni `..`, ni absolu, ni lecteur Windows),
//! la source doit être un fichier ordinaire déjà présent sous la racine (jamais un lien symbolique), la destination ne
//! traverse aucun lien symbolique, et rien n'existant déjà n'est remplacé. En cas d'échec, les liens créés par CET appel
//! sont retirés : l'opération est tout ou rien.
use crate::paths::{resolve_under, simplify};
use proto::pb::LinkTreeCmd;
use std::path::{Path, PathBuf};

/// Nombre maximal de liens par commande (le même plafond que le master).
const MAX_PAIRS: usize = proto::metainfo::MAX_LINKED_FILES;

/// Nom d'un dossier racine : un seul composant, ou vide (torrent mono-fichier : le fichier est à la racine).
fn root_name(s: &str) -> Result<Option<&str>, String> {
    if s.is_empty() {
        return Ok(None);
    }
    match proto::relpath::split_relative(s) {
        Ok(parts) if parts.len() == 1 && !s.contains(['/', '\\']) => Ok(Some(s)),
        _ => Err("nom de dossier invalide".to_owned()),
    }
}

fn rel_parts(s: &str) -> Result<Vec<&str>, String> {
    let parts =
        proto::relpath::split_relative(s).map_err(|_| "chemin de fichier invalide".to_owned())?;
    if parts.is_empty() || parts.len() > 32 {
        return Err("chemin de fichier invalide".to_owned());
    }
    Ok(parts)
}

fn link_error(e: &std::io::Error) -> String {
    // 18 = EXDEV (Linux/macOS), 17 = ERROR_NOT_SAME_DEVICE (Windows).
    if matches!(e.raw_os_error(), Some(17 | 18)) {
        return "liens physiques impossibles : les fichiers sont sur un autre disque que la destination".to_owned();
    }
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return "droits insuffisants pour créer le lien : l'agent doit s'exécuter avec le même utilisateur que le client torrent et avoir le dossier en écriture".to_owned();
    }
    "création du lien impossible".to_owned()
}

/// Crée les liens demandés. Idempotent : un lien déjà en place (même fichier, même taille) compte comme réussi.
/// Renvoie le nombre de fichiers liés.
pub fn link_tree(root: &Path, c: &LinkTreeCmd) -> Result<usize, String> {
    if c.pairs.is_empty() || c.pairs.len() > MAX_PAIRS {
        return Err(format!("entre 1 et {MAX_PAIRS} fichiers par commande"));
    }
    let canon_root = simplify(
        root.canonicalize()
            .map_err(|_| "racine locale illisible".to_owned())?,
    );
    let src_dir =
        resolve_under(root, &c.source_dir).map_err(|e| format!("dossier d'origine refusé: {e}"))?;
    let dst_dir = resolve_under(root, &c.dest_dir)
        .map_err(|e| format!("dossier de destination refusé: {e}"))?;
    let mut src_base = src_dir;
    if let Some(n) = root_name(&c.source_name)? {
        src_base.push(n);
    }
    let mut dst_base = dst_dir;
    if let Some(n) = root_name(&c.dest_name)? {
        dst_base.push(n);
    }

    let mut created_files: Vec<PathBuf> = Vec::new();
    let mut created_dirs: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<usize, String> {
        for p in &c.pairs {
            let mut src = src_base.clone();
            src.extend(rel_parts(&p.src)?);
            let mut dst = dst_base.clone();
            let dst_parts = rel_parts(&p.dst)?;
            dst.extend(&dst_parts);

            // Source : un fichier ORDINAIRE déjà présent, sous la racine, jamais un lien symbolique.
            let meta = std::fs::symlink_metadata(&src)
                .map_err(|_| "fichier d'origine introuvable".to_owned())?;
            if !meta.is_file() {
                return Err("le fichier d'origine n'est pas un fichier ordinaire".to_owned());
            }
            let real = simplify(
                src.canonicalize()
                    .map_err(|_| "fichier d'origine illisible".to_owned())?,
            );
            if !real.starts_with(&canon_root) {
                return Err("le fichier d'origine sort de la racine".to_owned());
            }

            // Destination : aucun composant existant ne doit être un lien symbolique ; les dossiers manquants sont créés.
            let parent = dst
                .parent()
                .ok_or_else(|| "destination invalide".to_owned())?;
            // Dossiers de la racine jusqu'au parent du fichier : chacun doit être un dossier ordinaire (jamais un lien),
            // les manquants sont créés (la destination peut être un dossier isolé tout neuf).
            let mut walk = canon_root.clone();
            if let Ok(extra) = parent.strip_prefix(&canon_root) {
                let mut chain: Vec<PathBuf> = Vec::new();
                for comp in extra.components() {
                    walk.push(comp);
                    chain.push(walk.clone());
                }
                for dir in chain {
                    match std::fs::symlink_metadata(&dir) {
                        Ok(m) if m.file_type().is_symlink() => {
                            return Err("destination refusée : lien symbolique rencontré".to_owned())
                        }
                        Ok(m) if !m.is_dir() => return Err(
                            "destination refusée : un fichier occupe l'emplacement d'un dossier"
                                .to_owned(),
                        ),
                        Ok(_) => {}
                        Err(_) => {
                            std::fs::create_dir(&dir)
                                .map_err(|_| "création d'un dossier impossible".to_owned())?;
                            created_dirs.push(dir);
                        }
                    }
                }
            }
            // Quel que soit le type de lien rencontré (symbolique, « junction » Windows…), le dossier de destination,
            // une fois résolu, doit rester sous la racine.
            let real_parent = simplify(
                parent
                    .canonicalize()
                    .map_err(|_| "destination illisible".to_owned())?,
            );
            if !real_parent.starts_with(&canon_root) {
                return Err("la destination sort de la racine".to_owned());
            }
            match std::fs::symlink_metadata(&dst) {
                // Déjà là : accepté seulement si c'est manifestement le même contenu ; jamais écrasé.
                Ok(m) if m.is_file() && m.len() == meta.len() => continue,
                Ok(_) => return Err("un fichier différent existe déjà à la destination".to_owned()),
                Err(_) => {}
            }
            std::fs::hard_link(&src, &dst).map_err(|e| link_error(&e))?;
            created_files.push(dst);
        }
        Ok(c.pairs.len())
    })();

    if result.is_err() {
        // Tout ou rien : on retire ce que CET appel a créé (jamais autre chose).
        for f in created_files.iter().rev() {
            let _ = std::fs::remove_file(f);
        }
        for d in created_dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::pb::LinkPair;

    fn cmd(
        src_dir: &str,
        src_name: &str,
        dst_dir: &str,
        dst_name: &str,
        pairs: &[(&str, &str)],
    ) -> LinkTreeCmd {
        LinkTreeCmd {
            source_dir: src_dir.into(),
            source_name: src_name.into(),
            dest_dir: dst_dir.into(),
            dest_name: dst_name.into(),
            pairs: pairs
                .iter()
                .map(|(a, b)| LinkPair {
                    src: (*a).into(),
                    dst: (*b).into(),
                })
                .collect(),
        }
    }

    fn symlink(from: &Path, to: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(from, to)
        }
        #[cfg(windows)]
        {
            if from.is_dir() {
                std::os::windows::fs::symlink_dir(from, to)
            } else {
                std::os::windows::fs::symlink_file(from, to)
            }
        }
    }

    fn write(root: &Path, rel: &str, data: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    #[test]
    fn a_renamed_tree_is_hard_linked_without_copying_anything() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "movies/Film.2024/video.mkv", b"0123456789");
        write(t.path(), "movies/Film.2024/Subs/fr.srt", b"sub");
        let c = cmd(
            "movies",
            "Film.2024",
            "movies",
            "Film.2024-GRP",
            &[
                ("video.mkv", "video.mkv"),
                ("Subs/fr.srt", "Sous-titres/fr.srt"),
            ],
        );
        assert_eq!(link_tree(t.path(), &c).unwrap(), 2);
        let (a, b) = (
            t.path().join("movies/Film.2024/video.mkv"),
            t.path().join("movies/Film.2024-GRP/video.mkv"),
        );
        assert_eq!(std::fs::read(&b).unwrap(), b"0123456789");
        assert_eq!(
            std::fs::read(t.path().join("movies/Film.2024-GRP/Sous-titres/fr.srt")).unwrap(),
            b"sub"
        );
        // C'est bien UN lien physique, pas une copie : une écriture dans l'un se voit dans l'autre.
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&a)
            .unwrap()
            .write_all(b"+")
            .unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"0123456789+");
        // Idempotent : rejouer la commande réussit sans rien changer.
        assert_eq!(link_tree(t.path(), &c).unwrap(), 2);
        // L'original est intact.
        assert!(t.path().join("movies/Film.2024/Subs/fr.srt").exists());
    }

    #[test]
    fn the_destination_folder_may_be_a_brand_new_isolated_one() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "movies/Film/video.mkv", b"data");
        let c = cmd(
            "movies",
            "Film",
            "movies/.nxt-xseed/abc123",
            "Film-GRP",
            &[("video.mkv", "video.mkv")],
        );
        link_tree(t.path(), &c).unwrap();
        assert_eq!(
            std::fs::read(t.path().join("movies/.nxt-xseed/abc123/Film-GRP/video.mkv")).unwrap(),
            b"data"
        );
        // Renommer le lien (ce que fait un client qui juge le torrent partiel) ne touche pas à l'original.
        std::fs::rename(
            t.path().join("movies/.nxt-xseed/abc123/Film-GRP/video.mkv"),
            t.path()
                .join("movies/.nxt-xseed/abc123/Film-GRP/video.mkv.part"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(t.path().join("movies/Film/video.mkv")).unwrap(),
            b"data"
        );
    }

    #[test]
    fn single_file_torrents_are_linked_under_their_own_name() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "movies/film.mkv", b"abc");
        let c = cmd("movies", "", "movies", "", &[("film.mkv", "Film.2024.mkv")]);
        link_tree(t.path(), &c).unwrap();
        assert_eq!(
            std::fs::read(t.path().join("movies/Film.2024.mkv")).unwrap(),
            b"abc"
        );
    }

    #[test]
    fn hostile_paths_are_refused_and_nothing_is_created() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "movies/D/a.bin", b"x");
        std::fs::create_dir_all(t.path().join("secret")).unwrap();
        for bad in [
            cmd("movies", "D", "movies", "E", &[("../a.bin", "a.bin")]),
            cmd("movies", "D", "movies", "E", &[("a.bin", "../a.bin")]),
            cmd("movies", "D", "movies", "E", &[("/etc/passwd", "a.bin")]),
            cmd("movies", "D", "movies", "E", &[("a.bin", "/tmp/a.bin")]),
            cmd("movies", "D", "movies", "E", &[("C:/x", "a.bin")]),
            cmd("movies", "D", "movies", "E", &[("a.bin", "")]),
            cmd("movies", "D", "movies", "E", &[("", "a.bin")]),
            cmd("movies", "../D", "movies", "E", &[("a.bin", "a.bin")]),
            cmd("movies", "D/x", "movies", "E", &[("a.bin", "a.bin")]),
            cmd("movies", "D", "movies", "..", &[("a.bin", "a.bin")]),
            cmd("../..", "D", "movies", "E", &[("a.bin", "a.bin")]),
            cmd("movies", "D", "/tmp", "E", &[("a.bin", "a.bin")]),
            cmd("movies", "D", "movies", "E", &[]),
            // Source inexistante / dossier à la place d'un fichier.
            cmd("movies", "D", "movies", "E", &[("absent.bin", "a.bin")]),
            cmd("movies", "", "movies", "E", &[("D", "a.bin")]),
        ] {
            assert!(link_tree(t.path(), &bad).is_err(), "{bad:?}");
        }
        assert!(!t.path().join("movies/E").exists(), "rien n'a été créé");
    }

    #[test]
    fn nothing_is_overwritten_and_a_failure_rolls_back_what_it_created() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "m/D/a.bin", b"aaaa");
        write(t.path(), "m/D/b.bin", b"bb");
        // Une destination occupée par un AUTRE contenu (taille différente) n'est jamais écrasée...
        write(t.path(), "m/E/b.bin", b"autre contenu");
        let c = cmd(
            "m",
            "D",
            "m",
            "E",
            &[("a.bin", "a.bin"), ("b.bin", "b.bin")],
        );
        let e = link_tree(t.path(), &c).unwrap_err();
        assert!(e.contains("différent existe déjà"), "{e}");
        assert_eq!(
            std::fs::read(t.path().join("m/E/b.bin")).unwrap(),
            b"autre contenu",
            "intact"
        );
        // ...et le lien déjà créé pour a.bin (avant l'échec) est retiré : tout ou rien.
        assert!(!t.path().join("m/E/a.bin").exists());
        // Un dossier créé par l'appel est retiré aussi.
        let c = cmd(
            "m",
            "D",
            "m",
            "F",
            &[("a.bin", "sous/dossier/a.bin"), ("absent", "x")],
        );
        assert!(link_tree(t.path(), &c).is_err());
        assert!(!t.path().join("m/F").exists(), "dossiers créés retirés");
    }

    #[test]
    fn symbolic_links_are_never_followed_or_traversed() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "m/D/a.bin", b"x");
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "cible.bin", b"hors racine");
        let sym = symlink;
        // Source = lien symbolique vers un fichier hors racine : refusé.
        if sym(
            &outside.path().join("cible.bin"),
            &t.path().join("m/D/lien.bin"),
        )
        .is_err()
        {
            eprintln!("SYMLINKS INDISPONIBLES : test des liens symboliques non exécuté");
            return; // création de liens symboliques impossible ici (droits Windows) : rien à vérifier
        }
        let c = cmd("m", "D", "m", "E", &[("lien.bin", "lien.bin")]);
        assert!(link_tree(t.path(), &c).is_err());
        assert!(!t.path().join("m/E/lien.bin").exists());
        // Destination dont un composant est un lien symbolique vers l'extérieur : refusée.
        std::fs::create_dir_all(t.path().join("m/G")).unwrap();
        sym(outside.path(), &t.path().join("m/G/sortie")).unwrap();
        let c = cmd("m", "D", "m", "G", &[("a.bin", "sortie/a.bin")]);
        assert!(link_tree(t.path(), &c).is_err());
        assert!(
            !outside.path().join("a.bin").exists(),
            "rien n'est écrit hors de la racine"
        );
    }

    #[test]
    fn at_most_the_agreed_number_of_files_are_linked() {
        let t = tempfile::tempdir().unwrap();
        let pairs: Vec<(String, String)> = (0..=MAX_PAIRS)
            .map(|i| (format!("f{i}"), format!("f{i}")))
            .collect();
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert!(link_tree(t.path(), &cmd("", "A", "", "B", &refs)).is_err());
    }
}
