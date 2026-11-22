//! Commandes « ponctuelles » demandées par le master : lister un dossier et créer un `.torrent` à partir de fichiers du nœud.
//!
//! Tout reste SOUS la racine locale de l'agent (`resolve_under` : chemin relatif, sans `..`, liens symboliques canonicalisés
//! sous la racine). Le master n'est pas cru sur parole : chaque chemin et chaque valeur est revalidé ici.
//! La création lit et hache des fichiers (CPU et disque) : au plus deux tâches à la fois, annulables.
use crate::paths::resolve_under;
use proto::pb::{CreateTorrentCmd, DirEntry};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Entrées d'un dossier renvoyées au plus (le reste est signalé par `truncated`).
pub const MAX_ENTRIES: usize = 2000;
/// Créations de torrent simultanées au plus sur un nœud.
pub const MAX_JOBS: usize = 2;

/// Tâches en cours : identifiant -> drapeau d'annulation. Survit aux sessions du tunnel.
#[derive(Default)]
pub struct JobTable {
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl JobTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<AtomicBool>>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Réserve une place pour la tâche `id` ; renvoie son drapeau d'annulation.
    pub fn start(&self, id: &str) -> Result<Arc<AtomicBool>, &'static str> {
        let mut m = self.lock();
        if m.contains_key(id) {
            return Err("tâche déjà en cours");
        }
        if m.len() >= MAX_JOBS {
            return Err("trop de créations de torrent en cours sur ce nœud (2 au plus)");
        }
        let flag = Arc::new(AtomicBool::new(false));
        m.insert(id.to_owned(), flag.clone());
        Ok(flag)
    }

    pub fn finish(&self, id: &str) {
        self.lock().remove(id);
    }

    /// Demande l'arrêt d'une tâche ; `false` si elle est inconnue (déjà finie).
    pub fn cancel(&self, id: &str) -> bool {
        match self.lock().get(id) {
            Some(f) => {
                f.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Identifiant de tâche acceptable : court, sans caractère qui puisse servir à autre chose qu'à nommer.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Contenu d'un dossier du nœud (`path` relatif à la racine ; vide = la racine). Dossiers d'abord, puis fichiers, triés sans
/// tenir compte de la casse. Les noms non UTF-8 et le dossier interne du cross-seed sont omis ; un lien symbolique est
/// signalé (`link`) mais jamais suivi.
pub fn browse(root: &Path, path: &str) -> Result<(Vec<DirEntry>, bool), String> {
    let dir = resolve_under(root, path).map_err(|e| format!("chemin refusé : {e}"))?;
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| "dossier introuvable".to_owned())?;
    if !meta.is_dir() {
        return Err("ce n'est pas un dossier".to_owned());
    }
    let rd = std::fs::read_dir(&dir).map_err(|_| "dossier illisible".to_owned())?;
    let mut out: Vec<DirEntry> = Vec::new();
    for it in rd.filter_map(Result::ok) {
        let Some(name) = it.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name == proto::create::INTERNAL_DIR {
            continue;
        }
        let Ok(m) = std::fs::symlink_metadata(it.path()) else {
            continue;
        };
        out.push(DirEntry {
            name,
            dir: m.is_dir(),
            size: if m.is_file() { m.len() } else { 0 },
            link: m.file_type().is_symlink(),
        });
    }
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let truncated = out.len() > MAX_ENTRIES;
    out.truncate(MAX_ENTRIES);
    Ok((out, truncated))
}

/// Crée le `.torrent` demandé (dictionnaire `info` seul : les adresses de trackers sont ajoutées par le master, hors de `info`).
pub fn create(
    root: &Path,
    c: &CreateTorrentCmd,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    if !valid_id(&c.job_id) {
        return Err("identifiant de tâche invalide".to_owned());
    }
    if c.path.trim_matches(['/', '\\', '.']).is_empty() {
        return Err("choisissez un fichier ou un dossier (pas toute la racine)".to_owned());
    }
    let abs = resolve_under(root, &c.path).map_err(|e| format!("chemin refusé : {e}"))?;
    let canon_root = root
        .canonicalize()
        .map_err(|_| "racine locale illisible".to_owned())?;
    if abs.canonicalize().is_ok_and(|a| a == canon_root) {
        return Err("choisissez un fichier ou un dossier (pas toute la racine)".to_owned());
    }
    let opts = proto::create::CreateOptions {
        piece_length: (c.piece_length != 0).then_some(c.piece_length),
        private: c.private,
        source: (!c.source.is_empty()).then(|| c.source.clone()),
        name: (!c.name.is_empty()).then(|| c.name.clone()),
    };
    proto::create::create(&abs, &opts, cancel, progress).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, data: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    fn cmd(path: &str) -> CreateTorrentCmd {
        CreateTorrentCmd {
            job_id: "job-1".into(),
            path: path.into(),
            ..CreateTorrentCmd::default()
        }
    }

    #[test]
    fn browse_lists_folders_first_hides_internal_dirs_and_marks_symlinks() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "movies/b.mkv", b"12345");
        write(t.path(), "movies/A.srt", b"1");
        write(t.path(), "movies/Zdir/x", b"x");
        write(t.path(), "movies/adir/x", b"x");
        write(t.path(), "movies/.nxt-xseed/abc/x", b"x");
        let (e, trunc) = browse(t.path(), "movies").unwrap();
        let names: Vec<(&str, bool)> = e.iter().map(|x| (x.name.as_str(), x.dir)).collect();
        assert_eq!(
            names,
            [
                ("adir", true),
                ("Zdir", true),
                ("A.srt", false),
                ("b.mkv", false)
            ]
        );
        assert!(!trunc);
        assert_eq!(e.iter().find(|x| x.name == "b.mkv").unwrap().size, 5);
        // La racine elle-même, par une chaîne vide.
        let (root, _) = browse(t.path(), "").unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, "movies");
    }

    #[test]
    fn browse_refuses_hostile_paths_and_non_directories() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "a/f.bin", b"x");
        for bad in [
            "..",
            "../x",
            "/etc",
            "C:\\Windows",
            "a/../..",
            "a\0b",
            "a/f.bin",
            "absent",
        ] {
            assert!(browse(t.path(), bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn browse_truncates_huge_directories() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..(MAX_ENTRIES + 5) {
            write(t.path(), &format!("d/f{i:05}"), b"");
        }
        let (e, trunc) = browse(t.path(), "d").unwrap();
        assert_eq!((e.len(), trunc), (MAX_ENTRIES, true));
    }

    #[test]
    fn create_hashes_files_under_the_root_and_refuses_everything_else() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "movies/Film/video.mkv", &vec![3u8; 70_000]);
        let cancel = AtomicBool::new(false);
        let bytes = create(t.path(), &cmd("movies/Film"), &cancel, &mut |_, _| {}).unwrap();
        let m = proto::metainfo::parse(&bytes).unwrap();
        assert_eq!((m.name.as_str(), m.size), ("Film", 70_000));
        // Chemins hostiles, racine entière, absent, identifiant invalide.
        for bad in [
            "",
            ".",
            "/",
            "..",
            "../x",
            "/etc/passwd",
            "C:\\x",
            "movies/absent",
        ] {
            assert!(
                create(t.path(), &cmd(bad), &cancel, &mut |_, _| {}).is_err(),
                "{bad:?}"
            );
        }
        let mut c = cmd("movies/Film");
        c.job_id = "../x".into();
        assert!(create(t.path(), &c, &cancel, &mut |_, _| {}).is_err());
        // Valeurs : taille de pièce, privé, nom.
        let mut c = cmd("movies/Film");
        c.piece_length = 1000;
        assert!(create(t.path(), &c, &cancel, &mut |_, _| {}).is_err());
        let mut c = cmd("movies/Film");
        c.private = true;
        c.source = "T".into();
        c.name = "Autre nom".into();
        let m = proto::metainfo::parse(&create(t.path(), &c, &cancel, &mut |_, _| {}).unwrap())
            .unwrap();
        assert!(m.private);
        assert_eq!(m.name, "Autre nom");
    }

    #[test]
    fn the_job_table_limits_concurrency_and_supports_cancel() {
        let jobs = JobTable::new();
        let a = jobs.start("a").unwrap();
        assert!(jobs.start("a").is_err(), "même identifiant deux fois");
        let _b = jobs.start("b").unwrap();
        assert!(jobs.start("c").is_err(), "au plus {MAX_JOBS} tâches");
        assert!(jobs.cancel("a") && a.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!jobs.cancel("inconnue"));
        jobs.finish("a");
        assert!(jobs.start("c").is_ok(), "une place libérée");
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn job_ids_are_plain_names() {
        for ok in ["a", "job-1", "A_b-9", &"x".repeat(64)] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "a b", "../x", "a/b", "é", &"x".repeat(65), "a\n"] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }
}
