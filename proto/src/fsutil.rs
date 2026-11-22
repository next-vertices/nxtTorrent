//! Écriture de fichiers secrets (clés privées) accessibles au seul propriétaire :
//! permissions 0600 sous Unix, ACL restreinte sous Windows.
use std::io::Write;
use std::path::Path;

pub fn write_secret(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // Restreint les droits AVANT d'écrire le secret : il n'est jamais lisible par un autre compte.
    #[cfg(windows)]
    restrict_to_owner(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(data)?;
    f.sync_all()
}

/// Sous Windows : coupe l'héritage des droits du dossier parent et n'accorde l'accès complet qu'à
/// l'utilisateur courant, à SYSTEM et aux Administrateurs (SID bien connus, indépendants de la langue).
/// Passe par `icacls` (livré avec Windows) : le crate reste sans `unsafe`.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    let mut grants = vec!["*S-1-5-18:(F)".to_owned(), "*S-1-5-32-544:(F)".to_owned()];
    if let (Ok(dom), Ok(user)) = (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        grants.push(format!("{dom}\\{user}:(F)"));
    }
    let mut cmd = std::process::Command::new("icacls");
    cmd.arg(path).arg("/inheritance:r");
    for g in &grants {
        cmd.arg("/grant:r").arg(g);
    }
    let out = cmd.output()?;
    if out.status.success() {
        return Ok(());
    }
    // Le compte courant peut ne pas se résoudre (service, domaine injoignable) : SYSTEM et
    // Administrateurs suffisent alors, mais jamais moins que cela.
    let mut fallback = std::process::Command::new("icacls");
    fallback.arg(path).args([
        "/inheritance:r",
        "/grant:r",
        "*S-1-5-18:(F)",
        "/grant:r",
        "*S-1-5-32-544:(F)",
    ]);
    let out = fallback.output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "impossible de restreindre les droits de {} : {}",
            path.display(),
            String::from_utf8_lossy(&out.stdout)
        )))
    }
}

/// Crée (si besoin) le dossier de données et le rend privé : 0700 sous Unix. Un dossier préexistant
/// (volume Docker, `/var/lib` créé par un installateur) est resserré lui aussi : il contient la CA ou
/// la clé du nœud et ne doit pas être traversable par les autres comptes. Sous Windows, ce sont les
/// fichiers secrets qui portent une ACL (cf. `write_secret`).
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Écriture atomique d'un secret : fichier temporaire 0600 puis renommage.
/// Un crash ne laisse jamais un fichier tronqué à la place de l'ancien.
pub fn write_secret_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "chemin sans nom"))?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    write_secret(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Lit un secret : variable d'environnement d'abord, sinon fichier protégé (cas d'un service, qui
/// n'hérite pas de l'environnement de l'utilisateur). Les fins de ligne sont retirées ; vide = absent.
pub fn read_secret(env_var: &str, file: &Path) -> Option<String> {
    let from_env = std::env::var(env_var).ok().filter(|v| !v.is_empty());
    from_env.or_else(|| {
        std::fs::read_to_string(file)
            .ok()
            .map(|s| s.trim_end_matches(['\r', '\n']).to_owned())
            .filter(|s| !s.is_empty())
    })
}

/// Ouvre un fichier de journal en ajout. Au-delà de `max_bytes`, l'ancien journal est conservé en
/// `.old` (une génération) : un service tourne des mois sans console ni rotation externe.
pub fn open_log(path: &Path, max_bytes: u64) -> std::io::Result<std::fs::File> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(path, &old)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_without_leftovers() {
        let d = std::env::temp_dir().join(format!("fsutil-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("a.key");
        write_secret_atomic(&p, b"un").unwrap();
        write_secret_atomic(&p, b"deux").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"deux");
        assert!(!d.join("a.key.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_is_0700_even_when_it_already_exists() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("fsutil-priv-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&d).unwrap();
        assert_eq!(
            std::fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let nested = d.join("a/b");
        ensure_private_dir(&nested).unwrap();
        assert_eq!(
            std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn read_secret_prefers_env_then_file_and_trims() {
        let d = std::env::temp_dir().join(format!("fsutil-rs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("pw");
        assert_eq!(read_secret("NXT_TEST_ABSENT_VAR", &f), None);
        std::fs::write(&f, "du-fichier\r\n").unwrap();
        assert_eq!(
            read_secret("NXT_TEST_ABSENT_VAR", &f).as_deref(),
            Some("du-fichier")
        );
        std::fs::write(&f, "\n").unwrap();
        assert_eq!(
            read_secret("NXT_TEST_ABSENT_VAR", &f),
            None,
            "fichier vide = absent"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn log_rotates_once_over_the_limit() {
        let d = std::env::temp_dir().join(format!("fsutil-log-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("a.log");
        std::fs::write(&p, vec![b'x'; 100]).unwrap();
        drop(open_log(&p, 1000).unwrap());
        assert!(
            !d.join("a.log.old").exists(),
            "sous la limite : pas de rotation"
        );
        drop(open_log(&p, 50).unwrap());
        assert!(d.join("a.log.old").exists());
        assert_eq!(
            std::fs::metadata(&p).unwrap().len(),
            0,
            "nouveau journal vide"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn secret_acl_excludes_other_accounts_on_windows() {
        let d = std::env::temp_dir().join(format!("fsutil-acl-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("a.key");
        write_secret_atomic(&p, b"secret").unwrap();
        let out = std::process::Command::new("icacls")
            .arg(&p)
            .output()
            .unwrap();
        let acl = String::from_utf8_lossy(&out.stdout).to_lowercase();
        for banned in ["everyone", "builtin\\users", "authenticated users"] {
            assert!(
                !acl.contains(banned),
                "{banned} ne doit pas avoir accès:\n{acl}"
            );
        }
        assert!(!acl.contains("(i)"), "aucun droit hérité:\n{acl}");
        assert_eq!(std::fs::read(&p).unwrap(), b"secret");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
