//! Manifeste de release SIGNÉ : ce que l'agent vérifiera avant d'installer quoi que ce soit (étape 3 des mises à jour).
//!
//! Principe : la clé privée de signature n'est JAMAIS sur le master (elle vit dans un secret d'environnement GitHub
//! protégé). Un master compromis peut donc relayer un manifeste, pas en fabriquer un : l'agent ne fait confiance qu'aux
//! clés publiques compilées ici (`TRUSTED_KEYS`). La signature porte sur les octets EXACTS de `manifest.json` ; on ne
//! réinterprète jamais un JSON avant de l'avoir authentifié.
//!
//! Ce que la vérification impose : signature Ed25519 valide d'une clé de confiance ; but (`purpose`) et schéma connus ;
//! dépôt attendu ; identifiant de clé cohérent avec celle qui a signé ; étiquette = `v` + version canonique ; dates
//! (pas dans le futur au-delà d'un jour, pas expiré, validité plafonnée) ; chaque fichier décrit par son SHA-256 et sa
//! taille. Le refus d'un retour en arrière (`require_newer_than`) et la vérification des fichiers (`check_asset`) sont des
//! étapes distinctes, appelées par celui qui installe.
use crate::version::Version;
use ring::signature;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// But déclaré dans le manifeste : une signature obtenue pour autre chose ne peut pas être rejouée ici.
pub const PURPOSE: &str = "nxtTorrent-release-manifest";
pub const SCHEMA: u64 = 1;
/// Dépôt qui publie les releases (le manifeste d'un autre projet signé par la même clé serait refusé).
pub const REPOSITORY: &str = "next-vertices/nxtTorrent";

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const SIGNATURE_BYTES: usize = 64;
/// Un manifeste ne vit pas plus de 400 jours : une fuite de vieux fichiers signés n'est pas éternelle.
pub const MAX_VALIDITY_SECS: i64 = 400 * 24 * 3600;
/// Tolérance sur l'horloge de la machine qui vérifie.
pub const MAX_CLOCK_SKEW_SECS: i64 = 24 * 3600;
const MAX_ASSETS: usize = 64;
const MAX_ASSET_BYTES: u64 = 1 << 30;
const MAX_NAME: usize = 64;

/// Préfixe DER d'une clé publique Ed25519 (SubjectPublicKeyInfo) : l'identifiant de clé est l'empreinte de ce DER, la
/// même que celle que donne `openssl pkey -pubin -outform DER | sha256sum`.
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Clé publique Ed25519 (32 octets bruts) à laquelle l'agent fait confiance.
#[derive(Clone, Copy, Debug)]
pub struct TrustedKey {
    pub public: [u8; 32],
}

impl TrustedKey {
    /// Empreinte SHA-256 complète du SubjectPublicKeyInfo (hexadécimal minuscule).
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(SPKI_PREFIX);
        h.update(self.public);
        hex(&h.finalize())
    }

    /// Identifiant court inscrit dans le manifeste : 8 premiers caractères de l'empreinte.
    pub fn id(&self) -> String {
        self.fingerprint()[..8].to_owned()
    }
}

/// Clé de release 2026. Une seconde clé (de secours, ou la suivante lors d'une rotation) s'ajoute à `TRUSTED_KEYS` AVANT
/// de retirer la première : les agents déjà installés doivent pouvoir accepter la transition.
const PUBLIC_KEY_2026: [u8; 32] = [
    0x4f, 0x7a, 0x34, 0xcb, 0xfd, 0x7c, 0x50, 0x29, 0xbc, 0x94, 0x29, 0xce, 0x06, 0x7f, 0x00, 0xf2,
    0xcf, 0x1e, 0x45, 0x7a, 0xf4, 0x65, 0x43, 0x99, 0x58, 0x5e, 0x09, 0x60, 0x59, 0x43, 0x08, 0xbc,
];

/// Clés auxquelles on fait confiance (l'agent n'en connaît pas d'autres, quoi que lui dise le master).
pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    public: PUBLIC_KEY_2026,
}];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReleaseError {
    #[error("manifeste trop volumineux")]
    TooLarge,
    #[error("signature de longueur invalide")]
    BadSignatureLength,
    #[error("signature invalide (aucune clé de confiance ne la valide)")]
    BadSignature,
    #[error("manifeste illisible : {0}")]
    Malformed(&'static str),
    #[error("manifeste destiné à un autre usage")]
    WrongPurpose,
    #[error("schéma de manifeste non pris en charge : {0}")]
    UnsupportedSchema(u64),
    #[error("manifeste d'un autre dépôt")]
    WrongRepository,
    #[error("l'identifiant de clé du manifeste ne correspond pas à la clé qui l'a signé")]
    KeyIdMismatch,
    #[error("manifeste daté du futur")]
    NotYetValid,
    #[error("manifeste expiré")]
    Expired,
    #[error("durée de validité excessive")]
    ValidityTooLong,
    #[error("version proposée ({offered}) pas plus récente que celle installée ({installed})")]
    NotNewer { offered: String, installed: String },
    #[error("fichier absent du manifeste")]
    UnknownAsset,
    #[error("fichier non conforme au manifeste : {0}")]
    AssetMismatch(&'static str),
}

/// Un fichier de la release : empreinte et taille attendues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub sha256: String,
    pub size: u64,
}

/// Manifeste dont la signature ET la cohérence ont été vérifiées.
#[derive(Clone, Debug)]
pub struct Verified {
    pub version: Version,
    pub tag: String,
    pub key_id: String,
    pub issued_at: i64,
    pub expires_at: i64,
    assets: BTreeMap<String, Asset>,
}

fn hex(b: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(b)
}

/// SHA-256 en hexadécimal minuscule.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= MAX_NAME
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !n.starts_with('.')
}

fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Vérifie avec les clés compilées dans le binaire (le cas d'usage réel).
pub fn verify(manifest: &[u8], sig: &[u8], now: i64) -> Result<Verified, ReleaseError> {
    verify_with(TRUSTED_KEYS, manifest, sig, now)
}

/// Vérifie avec des clés données (tests, et outil de release pour la répétition avec une clé jetable).
pub fn verify_with(
    keys: &[TrustedKey],
    manifest: &[u8],
    sig: &[u8],
    now: i64,
) -> Result<Verified, ReleaseError> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(ReleaseError::TooLarge);
    }
    if sig.len() != SIGNATURE_BYTES {
        return Err(ReleaseError::BadSignatureLength);
    }
    // 1. Authentifier les octets AVANT de les interpréter.
    let signer = keys
        .iter()
        .find(|k| {
            signature::UnparsedPublicKey::new(&signature::ED25519, &k.public)
                .verify(manifest, sig)
                .is_ok()
        })
        .ok_or(ReleaseError::BadSignature)?;

    // 2. Lire le contenu, strictement.
    let v: serde_json::Value =
        serde_json::from_slice(manifest).map_err(|_| ReleaseError::Malformed("JSON invalide"))?;
    let obj = v
        .as_object()
        .ok_or(ReleaseError::Malformed("objet attendu"))?;
    let text = |k: &'static str| {
        obj.get(k)
            .and_then(|x| x.as_str())
            .ok_or(ReleaseError::Malformed(k))
    };
    let int = |k: &'static str| {
        obj.get(k)
            .and_then(|x| x.as_i64())
            .ok_or(ReleaseError::Malformed(k))
    };

    if text("purpose")? != PURPOSE {
        return Err(ReleaseError::WrongPurpose);
    }
    let schema = obj
        .get("schema")
        .and_then(|x| x.as_u64())
        .ok_or(ReleaseError::Malformed("schema"))?;
    if schema != SCHEMA {
        return Err(ReleaseError::UnsupportedSchema(schema));
    }
    if text("repository")? != REPOSITORY {
        return Err(ReleaseError::WrongRepository);
    }
    let key_id = text("key_id")?;
    if key_id != signer.id() {
        return Err(ReleaseError::KeyIdMismatch);
    }
    let version_txt = text("version")?;
    let version = Version::parse(version_txt).ok_or(ReleaseError::Malformed("version"))?;
    // Forme canonique exigée : pas de préfixe `v`, pas de métadonnées de build (deux écritures d'une même version).
    if version.to_string() != version_txt {
        return Err(ReleaseError::Malformed("version non canonique"));
    }
    let tag = text("tag")?;
    if tag != format!("v{version_txt}") {
        return Err(ReleaseError::Malformed("étiquette"));
    }
    let issued_at = int("issued_at")?;
    let expires_at = int("expires_at")?;
    if issued_at > now.saturating_add(MAX_CLOCK_SKEW_SECS) {
        return Err(ReleaseError::NotYetValid);
    }
    if now >= expires_at {
        return Err(ReleaseError::Expired);
    }
    if expires_at <= issued_at || expires_at - issued_at > MAX_VALIDITY_SECS {
        return Err(ReleaseError::ValidityTooLong);
    }

    let raw_assets = obj
        .get("assets")
        .and_then(|a| a.as_object())
        .ok_or(ReleaseError::Malformed("assets"))?;
    if raw_assets.is_empty() || raw_assets.len() > MAX_ASSETS {
        return Err(ReleaseError::Malformed("nombre de fichiers"));
    }
    let mut assets = BTreeMap::new();
    for (name, a) in raw_assets {
        if !valid_name(name) {
            return Err(ReleaseError::Malformed("nom de fichier"));
        }
        let sha256 = a
            .get("sha256")
            .and_then(|s| s.as_str())
            .filter(|s| valid_sha256(s))
            .ok_or(ReleaseError::Malformed("sha256"))?;
        let size = a
            .get("size")
            .and_then(|s| s.as_u64())
            .filter(|s| *s <= MAX_ASSET_BYTES)
            .ok_or(ReleaseError::Malformed("taille"))?;
        assets.insert(
            name.clone(),
            Asset {
                sha256: sha256.to_owned(),
                size,
            },
        );
    }

    Ok(Verified {
        version,
        tag: tag.to_owned(),
        key_id: key_id.to_owned(),
        issued_at,
        expires_at,
        assets,
    })
}

impl Verified {
    /// Refuse un retour en arrière (et la même version) : un ancien manifeste, même authentique, n'installe rien.
    pub fn require_newer_than(&self, installed: &Version) -> Result<(), ReleaseError> {
        if self.version > *installed {
            Ok(())
        } else {
            Err(ReleaseError::NotNewer {
                offered: self.version.to_string(),
                installed: installed.to_string(),
            })
        }
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.get(name)
    }

    pub fn assets(&self) -> impl Iterator<Item = (&String, &Asset)> {
        self.assets.iter()
    }

    /// Le fichier reçu est-il celui que le manifeste décrit (taille ET SHA-256) ?
    pub fn check_asset(&self, name: &str, data: &[u8]) -> Result<(), ReleaseError> {
        let a = self.asset(name).ok_or(ReleaseError::UnknownAsset)?;
        if data.len() as u64 != a.size {
            return Err(ReleaseError::AssetMismatch("taille"));
        }
        if sha256_hex(data) != a.sha256 {
            return Err(ReleaseError::AssetMismatch("empreinte"));
        }
        Ok(())
    }
}

/// Contenu d'un manifeste à produire (outil de release, tests).
pub struct ManifestInput<'a> {
    pub version: &'a Version,
    pub key_id: &'a str,
    pub issued_at: i64,
    pub expires_at: i64,
    pub assets: &'a BTreeMap<String, Asset>,
}

/// Sérialise le manifeste : clés triées, donc les mêmes entrées donnent les mêmes octets.
pub fn build_manifest(input: &ManifestInput<'_>) -> Vec<u8> {
    let assets: serde_json::Map<String, serde_json::Value> = input
        .assets
        .iter()
        .map(|(n, a)| {
            (
                n.clone(),
                serde_json::json!({ "sha256": a.sha256, "size": a.size }),
            )
        })
        .collect();
    let v = serde_json::json!({
        "purpose": PURPOSE,
        "schema": SCHEMA,
        "repository": REPOSITORY,
        "key_id": input.key_id,
        "version": input.version.to_string(),
        "tag": format!("v{}", input.version),
        "issued_at": input.issued_at,
        "expires_at": input.expires_at,
        "assets": assets,
    });
    let mut out = serde_json::to_vec_pretty(&v).unwrap_or_default();
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 24 * 3600;

    fn keypair() -> (Ed25519KeyPair, TrustedKey) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut public = [0u8; 32];
        public.copy_from_slice(kp.public_key().as_ref());
        (kp, TrustedKey { public })
    }

    fn assets() -> BTreeMap<String, Asset> {
        let mut m = BTreeMap::new();
        m.insert(
            "nxt-agent-linux-amd64".to_owned(),
            Asset {
                sha256: sha256_hex(b"agent-amd64"),
                size: 11,
            },
        );
        m
    }

    /// Manifeste par défaut, valide à NOW : modifié par `edit` avant d'être signé.
    fn signed(
        kp: &Ed25519KeyPair,
        key: &TrustedKey,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> (Vec<u8>, Vec<u8>) {
        let v = Version::parse("0.1.0-rc.4").unwrap();
        let bytes = build_manifest(&ManifestInput {
            version: &v,
            key_id: &key.id(),
            issued_at: NOW - DAY,
            expires_at: NOW + 90 * DAY,
            assets: &assets(),
        });
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        edit(&mut json);
        let bytes = serde_json::to_vec_pretty(&json).unwrap();
        let sig = kp.sign(&bytes).as_ref().to_vec();
        (bytes, sig)
    }

    #[test]
    fn the_embedded_production_key_is_the_one_that_was_generated_and_recorded() {
        let k = &TRUSTED_KEYS[0];
        // Empreinte relevée par `openssl pkey -pubin -outform DER | sha256sum` sur la clé publique générée hors ligne.
        assert_eq!(
            k.fingerprint(),
            "1c0224a857fa577446915a5ff32a9e1732b8ecc907d3710d0da5d69da18f3a74"
        );
        assert_eq!(k.id(), "1c0224a8");
    }

    #[test]
    fn a_genuine_manifest_is_accepted_and_describes_its_files() {
        let (kp, key) = keypair();
        let (m, s) = signed(&kp, &key, |_| {});
        let ok = verify_with(&[key], &m, &s, NOW).unwrap();
        assert_eq!(ok.version.to_string(), "0.1.0-rc.4");
        assert_eq!(ok.tag, "v0.1.0-rc.4");
        assert_eq!(ok.key_id, key.id());
        assert_eq!(ok.assets().count(), 1);
        assert!(ok
            .check_asset("nxt-agent-linux-amd64", b"agent-amd64")
            .is_ok());
        // Le même manifeste est reproductible octet pour octet.
        let (m2, _) = signed(&kp, &key, |_| {});
        assert_eq!(m, m2);
    }

    #[test]
    fn nothing_is_trusted_without_a_valid_signature_from_a_trusted_key() {
        let (kp, key) = keypair();
        let (m, s) = signed(&kp, &key, |_| {});
        // Un octet du manifeste ou de la signature modifié.
        let mut m2 = m.clone();
        m2[10] ^= 1;
        assert_eq!(
            verify_with(&[key], &m2, &s, NOW).unwrap_err(),
            ReleaseError::BadSignature
        );
        let mut s2 = s.clone();
        s2[0] ^= 1;
        assert_eq!(
            verify_with(&[key], &m, &s2, NOW).unwrap_err(),
            ReleaseError::BadSignature
        );
        // Mauvaise longueur.
        assert_eq!(
            verify_with(&[key], &m, &s[..63], NOW).unwrap_err(),
            ReleaseError::BadSignatureLength
        );
        assert_eq!(
            verify_with(&[key], &m, &[], NOW).unwrap_err(),
            ReleaseError::BadSignatureLength
        );
        // Signé par une AUTRE clé : refusé, même si le manifeste revendique l'identifiant de la clé de confiance.
        let (evil, evil_key) = keypair();
        let (m3, s3) = signed(&evil, &evil_key, |j| {
            j["key_id"] = key.id().into();
        });
        assert_eq!(
            verify_with(&[key], &m3, &s3, NOW).unwrap_err(),
            ReleaseError::BadSignature
        );
        // Aucune clé de confiance : rien ne passe.
        assert_eq!(
            verify_with(&[], &m, &s, NOW).unwrap_err(),
            ReleaseError::BadSignature
        );
        // Avec les VRAIES clés compilées, une clé de test ne passe pas non plus.
        assert_eq!(verify(&m, &s, NOW).unwrap_err(), ReleaseError::BadSignature);
    }

    #[test]
    fn any_of_several_trusted_keys_may_sign_which_allows_a_rotation() {
        let (kp_old, old) = keypair();
        let (kp_new, new) = keypair();
        let keys = [old, new];
        for (kp, key) in [(&kp_old, &old), (&kp_new, &new)] {
            let (m, s) = signed(kp, key, |_| {});
            assert_eq!(verify_with(&keys, &m, &s, NOW).unwrap().key_id, key.id());
        }
        // Le manifeste doit citer la clé qui l'a réellement signé.
        let (m, s) = signed(&kp_new, &new, |j| j["key_id"] = old.id().into());
        assert_eq!(
            verify_with(&keys, &m, &s, NOW).unwrap_err(),
            ReleaseError::KeyIdMismatch
        );
    }

    #[test]
    fn dates_are_enforced() {
        let (kp, key) = keypair();
        let (m, s) = signed(&kp, &key, |_| {});
        // Expiré (un instant après la limite) ; la limite elle-même est déjà expirée.
        assert!(verify_with(&[key], &m, &s, NOW + 90 * DAY - 1).is_ok());
        assert_eq!(
            verify_with(&[key], &m, &s, NOW + 90 * DAY).unwrap_err(),
            ReleaseError::Expired
        );
        assert_eq!(
            verify_with(&[key], &m, &s, NOW + 500 * DAY).unwrap_err(),
            ReleaseError::Expired
        );
        // Émis dans le futur au-delà de la tolérance : horloge fausse ou manifeste antidaté.
        let (m2, s2) = signed(&kp, &key, |j| {
            j["issued_at"] = (NOW + 3 * DAY).into();
            j["expires_at"] = (NOW + 30 * DAY).into();
        });
        assert_eq!(
            verify_with(&[key], &m2, &s2, NOW).unwrap_err(),
            ReleaseError::NotYetValid
        );
        // Validité absurde (signé « pour toujours ») ou inversée.
        let (m3, s3) = signed(&kp, &key, |j| j["expires_at"] = (NOW + 800 * DAY).into());
        assert_eq!(
            verify_with(&[key], &m3, &s3, NOW).unwrap_err(),
            ReleaseError::ValidityTooLong
        );
        let (m4, s4) = signed(&kp, &key, |j| j["expires_at"] = (NOW - 2 * DAY).into());
        assert!(matches!(
            verify_with(&[key], &m4, &s4, NOW).unwrap_err(),
            ReleaseError::Expired
        ));
    }

    #[test]
    fn the_manifest_must_be_for_this_project_and_this_format() {
        let (kp, key) = keypair();
        type Edit = Box<dyn Fn(&mut serde_json::Value)>;
        let cases: Vec<(&str, Edit, ReleaseError)> = vec![
            (
                "autre but",
                Box::new(|j| j["purpose"] = "autre-chose".into()),
                ReleaseError::WrongPurpose,
            ),
            (
                "schéma futur",
                Box::new(|j| j["schema"] = 2.into()),
                ReleaseError::UnsupportedSchema(2),
            ),
            (
                "autre dépôt",
                Box::new(|j| j["repository"] = "evil/fork".into()),
                ReleaseError::WrongRepository,
            ),
            (
                "étiquette incohérente",
                Box::new(|j| j["tag"] = "v9.9.9".into()),
                ReleaseError::Malformed("étiquette"),
            ),
            (
                "version non canonique",
                Box::new(|j| {
                    j["version"] = "v0.1.0-rc.4".into();
                }),
                ReleaseError::Malformed("version non canonique"),
            ),
            (
                "version illisible",
                Box::new(|j| j["version"] = "n'importe quoi".into()),
                ReleaseError::Malformed("version"),
            ),
            (
                "sans fichiers",
                Box::new(|j| j["assets"] = serde_json::json!({})),
                ReleaseError::Malformed("nombre de fichiers"),
            ),
            (
                "nom de fichier dangereux",
                Box::new(|j| {
                    j["assets"] = serde_json::json!({ "../../etc/passwd": { "sha256": "0".repeat(64), "size": 1 } });
                }),
                ReleaseError::Malformed("nom de fichier"),
            ),
            (
                "nom caché",
                Box::new(|j| {
                    j["assets"] =
                        serde_json::json!({ ".bashrc": { "sha256": "0".repeat(64), "size": 1 } });
                }),
                ReleaseError::Malformed("nom de fichier"),
            ),
            (
                "empreinte en majuscules",
                Box::new(|j| {
                    j["assets"] =
                        serde_json::json!({ "nxt-agent": { "sha256": "A".repeat(64), "size": 1 } });
                }),
                ReleaseError::Malformed("sha256"),
            ),
            (
                "empreinte trop courte",
                Box::new(|j| {
                    j["assets"] = serde_json::json!({ "nxt-agent": { "sha256": "ab", "size": 1 } });
                }),
                ReleaseError::Malformed("sha256"),
            ),
            (
                "taille démesurée",
                Box::new(|j| {
                    j["assets"] = serde_json::json!({ "nxt-agent": { "sha256": "0".repeat(64), "size": u64::MAX } });
                }),
                ReleaseError::Malformed("taille"),
            ),
        ];
        for (name, edit, expected) in cases {
            let (m, s) = signed(&kp, &key, |j| edit(j));
            assert_eq!(
                verify_with(&[key], &m, &s, NOW).unwrap_err(),
                expected,
                "{name}"
            );
        }
        // Trop de fichiers.
        let (m, s) = signed(&kp, &key, |j| {
            let many: serde_json::Map<String, serde_json::Value> = (0..65)
                .map(|i| {
                    (
                        format!("f{i}"),
                        serde_json::json!({ "sha256": "0".repeat(64), "size": 1 }),
                    )
                })
                .collect();
            j["assets"] = many.into();
        });
        assert_eq!(
            verify_with(&[key], &m, &s, NOW).unwrap_err(),
            ReleaseError::Malformed("nombre de fichiers")
        );
        // Signé, mais pas du JSON / pas un objet : jamais interprété.
        for junk in [&b"pas du json"[..], b"[]", b"42"] {
            let sig = kp.sign(junk).as_ref().to_vec();
            assert!(matches!(
                verify_with(&[key], junk, &sig, NOW).unwrap_err(),
                ReleaseError::Malformed(_)
            ));
        }
        // Trop gros : refusé avant même de vérifier.
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        let sig = kp.sign(&big).as_ref().to_vec();
        assert_eq!(
            verify_with(&[key], &big, &sig, NOW).unwrap_err(),
            ReleaseError::TooLarge
        );
    }

    #[test]
    fn a_rollback_is_refused_even_with_a_genuine_manifest() {
        let (kp, key) = keypair();
        let (m, s) = signed(&kp, &key, |_| {}); // 0.1.0-rc.4
        let ok = verify_with(&[key], &m, &s, NOW).unwrap();
        let v = |x: &str| Version::parse(x).unwrap();
        assert!(ok.require_newer_than(&v("0.1.0-rc.3")).is_ok());
        assert!(ok.require_newer_than(&v("0.0.9")).is_ok());
        for installed in ["0.1.0-rc.4", "0.1.0-rc.5", "0.1.0", "0.2.0"] {
            assert!(
                matches!(
                    ok.require_newer_than(&v(installed)),
                    Err(ReleaseError::NotNewer { .. })
                ),
                "{installed} ne doit pas être « mise à jour » vers 0.1.0-rc.4"
            );
        }
    }

    #[test]
    fn files_are_checked_against_the_manifest() {
        let (kp, key) = keypair();
        let (m, s) = signed(&kp, &key, |_| {});
        let ok = verify_with(&[key], &m, &s, NOW).unwrap();
        assert!(ok
            .check_asset("nxt-agent-linux-amd64", b"agent-amd64")
            .is_ok());
        assert_eq!(
            ok.check_asset("nxt-agent-linux-amd64", b"agent-amd6X")
                .unwrap_err(),
            ReleaseError::AssetMismatch("empreinte")
        );
        assert_eq!(
            ok.check_asset("nxt-agent-linux-amd64", b"agent-amd64+")
                .unwrap_err(),
            ReleaseError::AssetMismatch("taille")
        );
        assert_eq!(
            ok.check_asset("nxt-agent-linux-arm64", b"agent-amd64")
                .unwrap_err(),
            ReleaseError::UnknownAsset
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
