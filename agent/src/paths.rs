//! Cloisonnement des chemins : tout chemin reçu du master est relatif à la racine locale.
use std::path::{Path, PathBuf};

/// Retire le préfixe « verbatim » `\\?\C:\…` que `canonicalize` ajoute sous Windows : les clients
/// torrent (qBittorrent, Transmission, Deluge sous Windows) ne le comprennent pas. Sans effet ailleurs.
/// Les chemins UNC (`\\?\UNC\…`) sont laissés tels quels.
pub fn simplify(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(r) = s.strip_prefix(r"\\?\") {
            let b = r.as_bytes();
            if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
                return PathBuf::from(r.to_owned());
            }
        }
    }
    p
}

pub use proto::relpath::PathError;

/// Résout `rel` sous `root`. Rejette : vide-invalide, absolu, `..`, `:` (lecteurs Windows),
/// caractères de contrôle, et tout lien symbolique (existant) menant hors de la racine.
/// Une chaîne vide désigne la racine elle-même.
pub fn resolve_under(root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    let parts = proto::relpath::split_relative(rel)?;
    let canon_root = simplify(root.canonicalize().map_err(|_| PathError::Root)?);
    let mut cur = canon_root.clone();
    for p in parts {
        let next = cur.join(p);
        if std::fs::symlink_metadata(&next).is_ok() {
            // Existe (éventuellement lien) : on suit la cible réelle et on revérifie le préfixe.
            let real = simplify(next.canonicalize().map_err(|_| PathError::Escape)?);
            if !real.starts_with(&canon_root) {
                return Err(PathError::Escape);
            }
            cur = real;
        } else {
            cur = next; // composant inexistant : simple nom, ne peut pas s'échapper
        }
    }
    Ok(cur)
}

/// Valeur de `save_subdir` rapportée quand le client stocke un torrent HORS de la racine
/// (jamais confondue avec la racine elle-même, qui est la chaîne vide).
pub const OUTSIDE_ROOT: &str = "?";

fn norm(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    if let Some(r) = s.strip_prefix("//?/") {
        s = r.to_owned(); // préfixe « verbatim » de Windows (`\\?\C:\...`)
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

/// Chemin de `abs` relatif à l'une des racines candidates (séparateurs et préfixes normalisés).
/// `None` si `abs` n'est sous aucune d'elles.
pub fn relative_str(roots: &[String], abs: &str) -> Option<String> {
    let a = norm(abs);
    let orig = abs.replace('\\', "/");
    for r in roots {
        let r = norm(r);
        if a == r {
            return Some(String::new());
        }
        if let Some(rest) = a.strip_prefix(&format!("{r}/")) {
            // On renvoie la fin du chemin d'origine (casse conservée), de même longueur.
            let tail = orig.len().saturating_sub(rest.len());
            return Some(orig.get(tail..).unwrap_or(rest).to_owned());
        }
    }
    None
}

/// Chemin relatif à la racine (jamais de chemin absolu exposé au master).
/// On essaie la racine canonique ET la racine telle que configurée (symlink, Windows).
pub fn relative_to(root: &Path, abs: &str) -> Option<String> {
    let mut roots = vec![root.to_string_lossy().into_owned()];
    if let Ok(c) = root.canonicalize() {
        roots.push(c.to_string_lossy().into_owned());
    }
    relative_str(&roots, abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_and_nested() {
        let d = tempfile::tempdir().unwrap();
        let root = simplify(d.path().canonicalize().unwrap());
        assert_eq!(resolve_under(d.path(), "").unwrap(), root);
        assert_eq!(
            resolve_under(d.path(), "movies/2024").unwrap(),
            root.join("movies").join("2024")
        );
        assert_eq!(
            resolve_under(d.path(), "./a//b/").unwrap(),
            root.join("a").join("b")
        );
    }

    #[test]
    fn rejects_malicious() {
        let d = tempfile::tempdir().unwrap();
        for (p, e) in [
            ("/etc/passwd", PathError::Absolute),
            ("\\windows", PathError::Absolute),
            ("..", PathError::Traversal),
            ("a/../../b", PathError::Traversal),
            ("a\\..\\b", PathError::Traversal),
            ("C:\\x", PathError::Invalid),
            ("a\0b", PathError::Invalid),
        ] {
            assert_eq!(resolve_under(d.path(), p), Err(e), "{p:?}");
        }
    }

    #[test]
    fn rejects_missing_root() {
        assert_eq!(
            resolve_under(Path::new("/nonexistent-root-xyz"), "a"),
            Err(PathError::Root)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        assert_eq!(resolve_under(d.path(), "link"), Err(PathError::Escape));
        assert_eq!(resolve_under(d.path(), "link/sub"), Err(PathError::Escape));
        std::os::unix::fs::symlink("/nonexistent-target", d.path().join("dangling")).unwrap();
        assert_eq!(resolve_under(d.path(), "dangling"), Err(PathError::Escape));
        // Lien interne à la racine : autorisé.
        std::fs::create_dir(d.path().join("real")).unwrap();
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("ok")).unwrap();
        assert!(resolve_under(d.path(), "ok").is_ok());
    }

    #[test]
    fn relative_hides_absolute() {
        let d = tempfile::tempdir().unwrap();
        let root = simplify(d.path().canonicalize().unwrap());
        let abs = root.join("tv").to_string_lossy().into_owned();
        assert_eq!(relative_to(d.path(), &abs).as_deref(), Some("tv"));
        assert_eq!(
            relative_to(d.path(), &root.to_string_lossy()).as_deref(),
            Some("")
        );
        assert_eq!(relative_to(d.path(), "/elsewhere"), None);
    }

    #[test]
    fn relative_handles_windows_and_verbatim_prefixes() {
        let roots = vec![r"\\?\C:\Downloads".to_owned(), "C:/Downloads".to_owned()];
        assert_eq!(
            relative_str(&roots, "C:/Downloads/tv/hd").as_deref(),
            Some("tv/hd")
        );
        assert_eq!(
            relative_str(&roots, r"C:\Downloads\tv").as_deref(),
            Some("tv")
        );
        assert_eq!(relative_str(&roots, "C:/Downloads/").as_deref(), Some(""));
        assert_eq!(
            relative_str(&roots, "C:/Downloads-evil/x"),
            None,
            "préfixe de nom, pas de dossier"
        );
        assert_eq!(relative_str(&roots, "D:/Other"), None);
    }
}
