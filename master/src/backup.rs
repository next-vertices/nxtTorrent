//! Sauvegarde et restauration du master : base de données (instantané cohérent), CA (clé et certificat), clé de
//! chiffrement des secrets TOTP/alertes, URL de webhook éventuelle.
//!
//! Une sauvegarde contient **la clé privée de la CA** (qui permet d'émettre des certificats de nœuds) et les secrets
//! de l'administrateur : elle est donc **toujours chiffrée** par une phrase de passe (Argon2id -> AES-256-GCM) et
//! authentifiée (toute altération est détectée). La restauration n'accepte qu'une liste fermée de noms de fichiers :
//! aucune entrée d'archive ne peut écrire hors du dossier de données.
use crate::store::Store;
use argon2::{Algorithm, Argon2, Params, Version};
use ring::rand::{SecureRandom, SystemRandom};
use std::path::Path;

const MAGIC: &[u8; 8] = b"NXTBAK01";
const SALT_LEN: usize = 16;
const DB_NAME: &str = "master.db";
/// Fichiers secrets sauvegardés tels quels s'ils existent (0600 à la restauration).
const SECRET_FILES: [&str; 4] = ["ca.crt", "ca.key", "totp.key", "webhook_url"];
pub const MIN_PASSPHRASE: usize = 12;
/// Taille maximale acceptée pour une entrée à la restauration (un fichier corrompu ne doit pas saturer la mémoire).
const MAX_ENTRY: usize = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("phrase de passe trop courte ({MIN_PASSPHRASE} caractères minimum)")]
    WeakPassphrase,
    #[error("phrase de passe incorrecte ou sauvegarde altérée")]
    Decrypt,
    #[error("ce n'est pas une sauvegarde nxtTorrent (ou version inconnue)")]
    Format,
    #[error("sauvegarde invalide : {0}")]
    Invalid(String),
    #[error("{0} existe déjà : arrêtez le master et utilisez --force pour remplacer les données")]
    Exists(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

fn derive(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], BackupError> {
    let params =
        Params::new(19 * 1024, 2, 1, Some(32)).map_err(|e| BackupError::Io(e.to_string()))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| BackupError::Io(e.to_string()))?;
    Ok(key)
}

fn entry(out: &mut Vec<u8>, name: &str, data: &[u8]) {
    out.push(u8::try_from(name.len()).unwrap_or(0));
    out.extend(name.as_bytes());
    out.extend((data.len() as u64).to_be_bytes());
    out.extend(data);
}

/// Crée la sauvegarde chiffrée. `store` est la base ouverte (instantané cohérent même pendant l'écriture).
pub fn create(data_dir: &Path, store: &Store, passphrase: &str) -> Result<Vec<u8>, BackupError> {
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(BackupError::WeakPassphrase);
    }
    let tmp = tempfile_in(data_dir)?;
    store
        .snapshot_into(&tmp)
        .map_err(|e| BackupError::Io(e.to_string()))?;
    let db = std::fs::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let mut plain = Vec::new();
    entry(&mut plain, DB_NAME, &db?);
    for name in SECRET_FILES {
        let p = data_dir.join(name);
        if p.exists() {
            entry(&mut plain, name, &std::fs::read(p)?);
        }
    }
    let mut salt = [0u8; SALT_LEN];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| BackupError::Io("générateur aléatoire indisponible".into()))?;
    let key = derive(passphrase, &salt)?;
    let mut out = MAGIC.to_vec();
    out.extend(salt);
    out.extend(crate::admin::totp::seal(&key, &plain));
    Ok(out)
}

fn tempfile_in(dir: &Path) -> Result<std::path::PathBuf, BackupError> {
    let mut r = [0u8; 8];
    SystemRandom::new()
        .fill(&mut r)
        .map_err(|_| BackupError::Io("générateur aléatoire indisponible".into()))?;
    Ok(dir.join(format!(
        ".backup-{}.tmp",
        data_encoding::HEXLOWER.encode(&r)
    )))
}

/// Déchiffre et analyse strictement : seuls les noms connus sont acceptés, sans doublon.
fn open(bytes: &[u8], passphrase: &str) -> Result<Vec<(String, Vec<u8>)>, BackupError> {
    let head = MAGIC.len() + SALT_LEN;
    if bytes.len() < head || &bytes[..MAGIC.len()] != MAGIC {
        return Err(BackupError::Format);
    }
    let key = derive(passphrase, &bytes[MAGIC.len()..head])?;
    let plain = crate::admin::totp::open(&key, &bytes[head..]).ok_or(BackupError::Decrypt)?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut i = 0;
    while i < plain.len() {
        let nl = usize::from(plain[i]);
        i += 1;
        let name = plain
            .get(i..i + nl)
            .and_then(|b| std::str::from_utf8(b).ok())
            .ok_or_else(|| BackupError::Invalid("nom d'entrée illisible".into()))?
            .to_owned();
        i += nl;
        let len = plain
            .get(i..i + 8)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_be_bytes)
            .and_then(|l| usize::try_from(l).ok())
            .filter(|l| *l <= MAX_ENTRY)
            .ok_or_else(|| BackupError::Invalid("taille d'entrée invalide".into()))?;
        i += 8;
        let data = plain
            .get(i..i + len)
            .ok_or_else(|| BackupError::Invalid("entrée tronquée".into()))?
            .to_vec();
        i += len;
        let allowed = name == DB_NAME || SECRET_FILES.contains(&name.as_str());
        if !allowed || entries.iter().any(|(n, _)| *n == name) {
            return Err(BackupError::Invalid(format!("entrée inattendue : {name}")));
        }
        entries.push((name, data));
    }
    if !entries.iter().any(|(n, _)| n == DB_NAME) || !entries.iter().any(|(n, _)| n == "ca.key") {
        return Err(BackupError::Invalid(
            "base de données ou clé de CA absente".into(),
        ));
    }
    Ok(entries)
}

/// Restaure dans `data_dir`. Le master doit être arrêté. Renvoie les noms des fichiers écrits.
pub fn restore(
    data_dir: &Path,
    bytes: &[u8],
    passphrase: &str,
    force: bool,
) -> Result<Vec<String>, BackupError> {
    let entries = open(bytes, passphrase)?;
    proto::fsutil::ensure_private_dir(data_dir)?;
    let db_path = data_dir.join(DB_NAME);
    if db_path.exists() && !force {
        return Err(BackupError::Exists(db_path.display().to_string()));
    }
    // Contrôle d'intégrité AVANT de toucher quoi que ce soit.
    let staged = tempfile_in(data_dir)?;
    let db = &entries
        .iter()
        .find(|(n, _)| n == DB_NAME)
        .map(|(_, d)| d.clone())
        .unwrap_or_default();
    std::fs::write(&staged, db)?;
    let ok = rusqlite::Connection::open(&staged)
        .and_then(|c| c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)))
        .is_ok_and(|r| r == "ok");
    if !ok {
        let _ = std::fs::remove_file(&staged);
        return Err(BackupError::Invalid(
            "la base de données de la sauvegarde est corrompue".into(),
        ));
    }
    let mut written = Vec::new();
    for (name, data) in entries.iter().filter(|(n, _)| n != DB_NAME) {
        proto::fsutil::write_secret_atomic(&data_dir.join(name), data)?;
        written.push(name.clone());
    }
    // Journaux WAL/SHM de l'ancienne base : ils ne correspondent plus à la base restaurée.
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(data_dir.join(format!("{DB_NAME}{ext}")));
    }
    std::fs::rename(&staged, &db_path)?;
    written.push(DB_NAME.to_owned());
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "une phrase de passe solide";

    fn setup() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("ca.crt"),
            b"-----BEGIN CERTIFICATE-----\nCA-PUBLIC\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("ca.key"),
            b"-----BEGIN PRIVATE KEY-----\nCA-PRIVATE-SECRET\n",
        )
        .unwrap();
        let s = Store::open(&d.path().join("master.db")).unwrap();
        s.upsert_admin("alice", "hash").unwrap();
        s.audit("test", "sauvegarde", "marqueur");
        (d, s)
    }

    #[test]
    fn round_trips_and_the_archive_is_opaque() {
        let (d, s) = setup();
        let bak = create(d.path(), &s, PASS).unwrap();
        // Rien de lisible : ni la clé de CA, ni le marqueur de la base.
        for needle in [
            &b"CA-PRIVATE-SECRET"[..],
            b"BEGIN PRIVATE KEY",
            b"alice",
            b"marqueur",
            b"SQLite format",
        ] {
            assert!(
                !bak.windows(needle.len()).any(|w| w == needle),
                "{:?} lisible",
                String::from_utf8_lossy(needle)
            );
        }
        let out = tempfile::tempdir().unwrap();
        let files = restore(out.path(), &bak, PASS, false).unwrap();
        assert!(
            files.contains(&"master.db".to_owned())
                && files.contains(&"ca.key".to_owned())
                && files.contains(&"totp.key".to_owned())
        );
        assert_eq!(
            std::fs::read(out.path().join("ca.key")).unwrap(),
            std::fs::read(d.path().join("ca.key")).unwrap()
        );
        let r = Store::open(&out.path().join("master.db")).unwrap();
        assert!(
            r.admin_hash("alice").unwrap().is_some(),
            "les données sont revenues"
        );
        // La clé de chiffrement des secrets suit : un secret TOTP écrit avant la sauvegarde reste lisible après.
        assert_eq!(
            std::fs::read(out.path().join("totp.key")).unwrap().len(),
            32
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(out.path().join("ca.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(m, 0o600, "secrets en 0600");
        }
    }

    #[test]
    fn wrong_passphrase_tampering_and_weak_passphrases_are_refused() {
        let (d, s) = setup();
        assert!(matches!(
            create(d.path(), &s, "court"),
            Err(BackupError::WeakPassphrase)
        ));
        let bak = create(d.path(), &s, PASS).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert!(matches!(
            restore(out.path(), &bak, "mauvaise phrase de passe", false),
            Err(BackupError::Decrypt)
        ));
        let mut bad = bak.clone();
        let n = bad.len() / 2;
        bad[n] ^= 1;
        assert!(
            matches!(
                restore(out.path(), &bad, PASS, false),
                Err(BackupError::Decrypt)
            ),
            "altération détectée"
        );
        assert!(matches!(
            restore(out.path(), b"pas une sauvegarde", PASS, false),
            Err(BackupError::Format)
        ));
        assert!(
            !out.path().join("master.db").exists(),
            "rien d'écrit après un échec"
        );
    }

    #[test]
    fn restore_never_overwrites_without_force_and_cleans_stale_wal() {
        let (d, s) = setup();
        let bak = create(d.path(), &s, PASS).unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("master.db"), b"donnees existantes").unwrap();
        assert!(matches!(
            restore(out.path(), &bak, PASS, false),
            Err(BackupError::Exists(_))
        ));
        assert_eq!(
            std::fs::read(out.path().join("master.db")).unwrap(),
            b"donnees existantes",
            "intact"
        );
        std::fs::write(out.path().join("master.db-wal"), b"journal perime").unwrap();
        restore(out.path(), &bak, PASS, true).unwrap();
        assert!(
            !out.path().join("master.db-wal").exists(),
            "WAL périmé supprimé"
        );
        assert!(Store::open(&out.path().join("master.db"))
            .unwrap()
            .admin_hash("alice")
            .unwrap()
            .is_some());
    }

    #[test]
    fn crafted_archives_cannot_write_outside_or_smuggle_files() {
        // Archive valide cryptographiquement mais au contenu hostile : noms hors liste, doublons, base absente.
        let forge = |entries: &[(&str, &[u8])]| {
            let mut plain = Vec::new();
            for (n, d) in entries {
                entry(&mut plain, n, d);
            }
            let salt = [9u8; SALT_LEN];
            let key = derive(PASS, &salt).unwrap();
            let mut out = MAGIC.to_vec();
            out.extend(salt);
            out.extend(crate::admin::totp::seal(&key, &plain));
            out
        };
        let out = tempfile::tempdir().unwrap();
        for evil in [
            "../evil",
            "..\\evil",
            "/etc/passwd",
            "sub/ca.key",
            "authorized_keys",
            "ca.key.tmp",
        ] {
            let f = forge(&[("master.db", b"x"), ("ca.key", b"k"), (evil, b"pwn")]);
            assert!(
                matches!(
                    restore(out.path(), &f, PASS, false),
                    Err(BackupError::Invalid(_))
                ),
                "{evil}"
            );
        }
        let dup = forge(&[("master.db", b"x"), ("ca.key", b"k"), ("ca.key", b"k2")]);
        assert!(
            matches!(
                restore(out.path(), &dup, PASS, false),
                Err(BackupError::Invalid(_))
            ),
            "doublon"
        );
        let nodb = forge(&[("ca.key", b"k")]);
        assert!(matches!(
            restore(out.path(), &nodb, PASS, false),
            Err(BackupError::Invalid(_))
        ));
        let corrupt = forge(&[
            ("master.db", b"ceci n'est pas une base sqlite"),
            ("ca.key", b"k"),
        ]);
        assert!(
            matches!(
                restore(out.path(), &corrupt, PASS, false),
                Err(BackupError::Invalid(_))
            ),
            "base corrompue"
        );
        assert!(
            !out.path().join("master.db").exists() && !out.path().join("ca.key").exists(),
            "rien d'écrit"
        );
        assert!(std::fs::read_dir(out.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("..")));
    }
}
