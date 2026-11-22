//! Mots de passe administrateur : politique de robustesse et hachage Argon2id.
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};
use std::sync::OnceLock;

pub const MIN_LEN: usize = 12;
pub const MAX_LEN: usize = 128;

/// Mots de passe courants refusés quelle que soit leur longueur (liste volontairement courte : la
/// longueur minimale et la variété font l'essentiel du travail).
const COMMON: &[&str] = &[
    "password1234",
    "passwordpassword",
    "administrator",
    "adminadminadmin",
    "123456789012",
    "1234567890123",
    "qwertyuiop12",
    "azertyuiop12",
    "azertyazerty",
    "motdepasse123",
    "motdepasse1234",
    "changemechangeme",
    "letmeinletmein",
    "iloveyou1234",
];

/// Vérifie qu'un mot de passe est assez robuste ; le message d'erreur est destiné à l'utilisateur.
pub fn validate_strength(pw: &str, username: &str) -> Result<(), &'static str> {
    let n = pw.chars().count();
    if n < MIN_LEN {
        return Err("mot de passe trop court (12 caractères minimum)");
    }
    if n > MAX_LEN {
        return Err("mot de passe trop long (128 caractères maximum)");
    }
    let lower = pw.to_lowercase();
    if !username.is_empty() && lower.contains(&username.to_lowercase()) {
        return Err("le mot de passe ne doit pas contenir le nom d'utilisateur");
    }
    if COMMON.contains(&lower.as_str()) {
        return Err("mot de passe trop courant");
    }
    let distinct = pw.chars().collect::<std::collections::HashSet<_>>().len();
    if distinct < 6 {
        return Err("mot de passe trop répétitif");
    }
    // Variété : au moins 3 classes parmi minuscules, majuscules, chiffres, autres — sauf phrase de passe longue.
    let classes = [
        pw.chars().any(|c| c.is_lowercase()),
        pw.chars().any(|c| c.is_uppercase()),
        pw.chars().any(|c| c.is_ascii_digit()),
        pw.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if classes < 3 && n < 20 {
        return Err("mélangez minuscules, majuscules, chiffres et symboles (ou utilisez 20 caractères et plus)");
    }
    Ok(())
}

/// Argon2id, paramètres OWASP (19 Mio, 2 itérations, 1 voie).
fn argon2() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).unwrap_or_default();
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

pub fn hash(pw: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    argon2()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Vérification à temps constant (via Argon2). Un hachage illisible est un refus.
pub fn verify(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc).is_ok_and(|h| argon2().verify_password(pw.as_bytes(), &h).is_ok())
}

/// Vérifie contre un hachage factice : un compte inconnu coûte le même temps qu'un mauvais mot de
/// passe (pas d'énumération des comptes par mesure du temps de réponse).
pub fn dummy_verify(pw: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let h = DUMMY.get_or_init(|| hash("dummy-password-for-timing").unwrap_or_default());
    let _ = verify(pw, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_policy() {
        let u = "alice";
        assert!(validate_strength("Tr0ub4dor&3xyz", u).is_ok());
        assert!(
            validate_strength("une phrase de passe assez longue", u).is_ok(),
            "phrase longue : variété non exigée"
        );
        for (pw, why) in [
            ("Court1!", "court"),
            ("aaaaaaaaaaaaaaaa", "répétitif"),
            ("abababababababab", "répétitif"),
            ("Alice-Secret-2024", "contient le nom"),
            ("azertyazerty", "courant"),
            ("motsdepassetrop", "une seule classe"),
            ("123456789012", "courant"),
        ] {
            assert!(
                validate_strength(pw, u).is_err(),
                "{pw} ({why}) aurait dû être refusé"
            );
        }
        assert!(
            validate_strength(&"Ab1!".repeat(40), u).is_err(),
            "trop long"
        );
    }

    #[test]
    fn hash_is_argon2id_salted_and_verifiable() {
        let h1 = hash("Tr0ub4dor&3xyz").unwrap();
        let h2 = hash("Tr0ub4dor&3xyz").unwrap();
        assert!(h1.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h1}");
        assert_ne!(h1, h2, "sel différent à chaque hachage");
        assert!(verify("Tr0ub4dor&3xyz", &h1));
        assert!(!verify("tr0ub4dor&3xyz", &h1));
        assert!(!verify("x", "pas-un-hachage-phc"));
        dummy_verify("x"); // ne doit ni paniquer ni échouer
    }
}
