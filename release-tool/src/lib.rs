//! Chaîne de release : fabrique le manifeste à signer et vérifie le résultat signé.
//!
//! Cet outil ne voit JAMAIS la clé privée : la signature se fait à part (`openssl pkeyutl -sign -rawin`, avec le secret de
//! l'environnement GitHub `release`). Il sert aussi de dernier garde-fou : après la signature, `verify` rejoue exactement la
//! vérification de l'agent avec les clés compilées dans le binaire ; si la clé du secret n'est pas celle qui est compilée,
//! la release échoue avant d'être publiée.
use anyhow::{bail, Context, Result};
use proto::release::{self, Asset, ManifestInput, TrustedKey, Verified};
use proto::version::Version;
use std::collections::BTreeMap;
use std::path::Path;

/// Préfixe des fichiers de release décrits dans le manifeste (`nxt-agent-linux-amd64`, `nxt-master-…`).
const ASSET_PREFIX: &str = "nxt-";

/// Les fichiers `nxt-*` de `dist` avec leur SHA-256 et leur taille. Les liens symboliques sont refusés.
pub fn collect_assets(dist: &Path) -> Result<BTreeMap<String, Asset>> {
    let mut out = BTreeMap::new();
    for entry in
        std::fs::read_dir(dist).with_context(|| format!("lecture de {}", dist.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(ASSET_PREFIX) {
            continue;
        }
        let meta = std::fs::symlink_metadata(entry.path())?;
        if !meta.is_file() {
            bail!("{name} n'est pas un fichier ordinaire (lien symbolique ou dossier)");
        }
        let data = std::fs::read(entry.path())?;
        out.insert(
            name,
            Asset {
                sha256: release::sha256_hex(&data),
                size: data.len() as u64,
            },
        );
    }
    if out.is_empty() {
        bail!("aucun fichier {ASSET_PREFIX}* dans {}", dist.display());
    }
    Ok(out)
}

/// Le manifeste de `version`, valable `valid_days` jours à partir de `now`.
pub fn make_manifest(
    dist: &Path,
    version: &str,
    key_id: &str,
    valid_days: i64,
    now: i64,
) -> Result<Vec<u8>> {
    let parsed = Version::parse(version).context("version illisible")?;
    if parsed.to_string() != version {
        bail!("version non canonique ({version}) : écrire {parsed}, sans préfixe v ni métadonnées de build");
    }
    let version = parsed;
    if key_id.len() != 8
        || !key_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        bail!("identifiant de clé invalide (8 caractères hexadécimaux attendus) : {key_id:?}");
    }
    if !(1..=400).contains(&valid_days) {
        bail!("durée de validité hors limites (1 à 400 jours)");
    }
    let assets = collect_assets(dist)?;
    Ok(release::build_manifest(&ManifestInput {
        version: &version,
        key_id,
        issued_at: now,
        expires_at: now + valid_days * 24 * 3600,
        assets: &assets,
    }))
}

/// Vérifie signature et cohérence ; avec `dist`, vérifie aussi chaque fichier listé (présence, taille, SHA-256) et
/// qu'aucun fichier `nxt-*` n'est publié sans figurer au manifeste.
pub fn verify_release(
    manifest: &[u8],
    sig: &[u8],
    keys: &[TrustedKey],
    now: i64,
    dist: Option<&Path>,
    installed: Option<&Version>,
) -> Result<Verified> {
    let v = release::verify_with(keys, manifest, sig, now).map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(i) = installed {
        v.require_newer_than(i)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if let Some(dist) = dist {
        for (name, _) in v.assets() {
            let data = std::fs::read(dist.join(name))
                .with_context(|| format!("fichier {name} absent de {}", dist.display()))?;
            v.check_asset(name, &data)
                .map_err(|e| anyhow::anyhow!("{name} : {e}"))?;
        }
        for name in collect_assets(dist)?.keys() {
            if v.asset(name).is_none() {
                bail!("{name} est publié mais absent du manifeste signé");
            }
        }
    }
    Ok(v)
}

/// Clé publique brute (64 caractères hexadécimaux) -> clé de confiance, pour les répétitions avec une clé jetable.
pub fn trusted_key_from_hex(h: &str) -> Result<TrustedKey> {
    let bytes = data_encoding::HEXLOWER_PERMISSIVE
        .decode(h.trim().as_bytes())
        .context("clé hexadécimale invalide")?;
    let public: [u8; 32] = bytes.try_into().map_err(|_| {
        anyhow::anyhow!("une clé Ed25519 fait 32 octets (64 caractères hexadécimaux)")
    })?;
    Ok(TrustedKey { public })
}
