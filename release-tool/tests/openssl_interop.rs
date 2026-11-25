//! Interopérabilité : la CI signe avec `openssl pkeyutl -sign -rawin` (clé Ed25519 chiffrée par une passphrase, comme le
//! secret de l'environnement `release`) ; l'agent vérifie avec `ring`. Ce test rejoue la chaîne complète avec une clé
//! jetable. Sans `openssl` sur la machine, il est ignoré (la CI Linux l'a).
use proto::release::TrustedKey;
use proto::version::Version;
use release_tool::{collect_assets, make_manifest, verify_release};
use std::path::Path;
use std::process::Command;

const PASS: &str = "passphrase-de-test";

fn openssl_available() -> bool {
    Command::new("openssl")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn openssl(args: &[&str]) -> Vec<u8> {
    let o = Command::new("openssl")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("openssl : {e}"));
    assert!(
        o.status.success(),
        "openssl {args:?} : {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o.stdout
}

/// Clé jetable chiffrée par passphrase (même format que `genpkey -aes-256-cbc` de la procédure) + sa clé publique brute.
fn throwaway_key(dir: &Path) -> (String, TrustedKey) {
    let key = dir.join("release.key").to_string_lossy().into_owned();
    openssl(&[
        "genpkey",
        "-algorithm",
        "ED25519",
        "-aes-256-cbc",
        "-pass",
        &format!("pass:{PASS}"),
        "-out",
        &key,
    ]);
    let der = openssl(&[
        "pkey",
        "-in",
        &key,
        "-passin",
        &format!("pass:{PASS}"),
        "-pubout",
        "-outform",
        "DER",
    ]);
    let public: [u8; 32] = der[der.len() - 32..].try_into().unwrap();
    (key, TrustedKey { public })
}

fn sign(dir: &Path, key: &str, manifest: &Path) -> Vec<u8> {
    let sig = dir.join("manifest.json.sig").to_string_lossy().into_owned();
    openssl(&[
        "pkeyutl",
        "-sign",
        "-inkey",
        key,
        "-passin",
        &format!("pass:{PASS}"),
        "-rawin",
        "-in",
        &manifest.to_string_lossy(),
        "-out",
        &sig,
    ]);
    std::fs::read(sig).unwrap()
}

fn dist(dir: &Path) -> std::path::PathBuf {
    let d = dir.join("dist");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("nxt-agent-linux-amd64"), b"binaire agent amd64").unwrap();
    std::fs::write(
        d.join("nxt-agent-windows-amd64.exe"),
        b"binaire agent windows",
    )
    .unwrap();
    std::fs::write(d.join("nxt-master-linux-amd64"), b"binaire master").unwrap();
    // Pas décrits : sommes de contrôle, notes… (seuls les nxt-* sont des binaires de release).
    std::fs::write(d.join("SHA256SUMS"), b"...").unwrap();
    d
}

#[test]
fn a_manifest_signed_by_openssl_is_verified_by_the_agent_code() {
    if !openssl_available() {
        eprintln!("openssl absent : test ignoré");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (key, trusted) = throwaway_key(tmp.path());
    let dist = dist(tmp.path());
    let now = 1_800_000_000;

    // 1. Le manifeste est fabriqué par l'outil, SANS clé.
    let manifest = make_manifest(&dist, "0.1.0-rc.4", &trusted.id(), 180, now).unwrap();
    let manifest_path = tmp.path().join("manifest.json");
    std::fs::write(&manifest_path, &manifest).unwrap();
    assert_eq!(collect_assets(&dist).unwrap().len(), 3, "SHA256SUMS exclu");

    // 2. Signé par OpenSSL, comme dans la CI.
    let sig = sign(tmp.path(), &key, &manifest_path);
    assert_eq!(sig.len(), 64);

    // 3. Vérifié par le code de l'agent : signature, cohérence, fichiers.
    let v = verify_release(&manifest, &sig, &[trusted], now + 3600, Some(&dist), None).unwrap();
    assert_eq!(v.version.to_string(), "0.1.0-rc.4");
    assert_eq!(v.assets().count(), 3);
    let installed = Version::parse("0.1.0-rc.3").unwrap();
    assert!(verify_release(&manifest, &sig, &[trusted], now, None, Some(&installed)).is_ok());
    let same = Version::parse("0.1.0-rc.4").unwrap();
    assert!(
        verify_release(&manifest, &sig, &[trusted], now, None, Some(&same)).is_err(),
        "la même version n'est pas une mise à jour"
    );

    // 4. Les garde-fous.
    // Un binaire modifié après la signature.
    std::fs::write(
        dist.join("nxt-master-linux-amd64"),
        b"binaire master TROYEN",
    )
    .unwrap();
    let err = verify_release(&manifest, &sig, &[trusted], now, Some(&dist), None).unwrap_err();
    assert!(format!("{err}").contains("nxt-master-linux-amd64"), "{err}");
    std::fs::write(dist.join("nxt-master-linux-amd64"), b"binaire master").unwrap();
    // Un binaire ajouté à la release sans figurer au manifeste signé.
    std::fs::write(dist.join("nxt-extra"), b"surprise").unwrap();
    let err = verify_release(&manifest, &sig, &[trusted], now, Some(&dist), None).unwrap_err();
    assert!(format!("{err}").contains("nxt-extra"), "{err}");
    std::fs::remove_file(dist.join("nxt-extra")).unwrap();
    // Manifeste altéré d'un octet.
    let mut forged = manifest.clone();
    forged[20] ^= 1;
    assert!(verify_release(&forged, &sig, &[trusted], now, None, None).is_err());
    // Les clés COMPILÉES dans l'agent ne reconnaissent pas une clé jetable : c'est ce qui fera échouer la CI si le secret
    // GitHub n'est pas la sœur de la clé publique compilée.
    assert!(verify_release(
        &manifest,
        &sig,
        proto::release::TRUSTED_KEYS,
        now,
        None,
        None
    )
    .is_err());
    // Expiré.
    assert!(verify_release(
        &manifest,
        &sig,
        &[trusted],
        now + 181 * 24 * 3600,
        None,
        None
    )
    .is_err());
}

#[test]
fn the_command_line_tool_runs_the_same_chain() {
    if !openssl_available() {
        eprintln!("openssl absent : test ignoré");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (key, trusted) = throwaway_key(tmp.path());
    let dist = dist(tmp.path());
    let bin = env!("CARGO_BIN_EXE_release-tool");
    let manifest = tmp.path().join("manifest.json");
    let hex = data_encoding::HEXLOWER.encode(&trusted.public);

    let out = Command::new(bin)
        .args([
            "manifest",
            "--version",
            "0.1.0-rc.4",
            "--key-id",
            &trusted.id(),
            "--dist",
        ])
        .arg(&dist)
        .arg("--out")
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sig = tmp.path().join("manifest.json.sig");
    sign(tmp.path(), &key, &manifest);

    // Clé jetable : accepté.
    let verify = |extra: &[&str]| {
        Command::new(bin)
            .args(["verify", "--manifest"])
            .arg(&manifest)
            .arg("--sig")
            .arg(&sig)
            .args(extra)
            .output()
            .unwrap()
    };
    let dist_s = dist.to_string_lossy().into_owned();
    let ok = verify(&["--trusted-key", &hex, "--dist", &dist_s]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("OK : version 0.1.0-rc.4"));
    // Sans `--trusted-key` : les clés compilées s'appliquent, et refusent la clé jetable (code de sortie non nul).
    let bad = verify(&[]);
    assert!(
        !bad.status.success(),
        "les clés compilées ne doivent pas accepter une clé jetable"
    );
    // Mauvaise clé fournie, version déjà installée.
    let other = "00".repeat(32);
    assert!(!verify(&["--trusted-key", &other]).status.success());
    assert!(
        !verify(&["--trusted-key", &hex, "--installed", "0.1.0-rc.9"])
            .status
            .success()
    );
    // Version écrite avec un préfixe : refusée à la fabrication.
    let out = Command::new(bin)
        .args(["manifest", "--version", "v0.1.0-rc.4", "--dist"])
        .arg(&dist)
        .arg("--out")
        .arg(tmp.path().join("x.json"))
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn an_empty_or_hostile_distribution_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        collect_assets(tmp.path()).is_err(),
        "aucun binaire à décrire"
    );
    std::fs::create_dir(tmp.path().join("nxt-dossier")).unwrap();
    assert!(
        collect_assets(tmp.path()).is_err(),
        "un dossier nxt-* n'est pas un fichier"
    );
}
