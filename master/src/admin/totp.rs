//! Double authentification de l'administrateur : TOTP (RFC 6238, SHA-1, 6 chiffres, pas de 30 s) et chiffrement
//! du secret au repos (AES-256-GCM).
//!
//! Le secret TOTP doit pouvoir être relu (on ne peut pas le hacher comme un mot de passe) : il est donc chiffré
//! avec une clé aléatoire stockée dans un fichier 0600 à côté de la base. Une copie de la base seule ne suffit
//! pas à générer des codes.
use data_encoding::BASE32_NOPAD;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

pub const STEP_SECS: i64 = 30;
pub const SECRET_LEN: usize = 20;

pub fn generate_secret() -> [u8; SECRET_LEN] {
    let mut s = [0u8; SECRET_LEN];
    // Échec du générateur système : situation irrécupérable (même politique que le reste du master).
    SystemRandom::new()
        .fill(&mut s)
        .unwrap_or_else(|_| panic!("générateur aléatoire indisponible"));
    s
}

/// 32 octets aléatoires (clé AES-256 du chiffrement au repos).
pub fn generate_secret_32() -> [u8; 32] {
    let mut s = [0u8; 32];
    SystemRandom::new()
        .fill(&mut s)
        .unwrap_or_else(|_| panic!("générateur aléatoire indisponible"));
    s
}

pub fn secret_b32(secret: &[u8]) -> String {
    BASE32_NOPAD.encode(secret)
}

/// Code à 6 chiffres pour un pas de temps donné (RFC 4226 §5.3, troncature dynamique).
pub fn code_at(secret: &[u8], step: i64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let h = tag.as_ref();
    let off = usize::from(h[h.len() - 1] & 0x0f);
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    bin % 1_000_000
}

pub fn step_of(now_secs: i64) -> i64 {
    now_secs.div_euclid(STEP_SECS)
}

/// Vérifie un code saisi (tolérance ±1 pas pour les horloges légèrement décalées). Renvoie le pas accepté, ou
/// `None`. Un pas déjà utilisé (`<= last_step`) est refusé : un code intercepté ne peut pas être rejoué.
pub fn verify(secret: &[u8], input: &str, now_secs: i64, last_step: Option<i64>) -> Option<i64> {
    let digits: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let want: u32 = digits.parse().ok()?;
    let now = step_of(now_secs);
    let mut found = None;
    for step in [now - 1, now, now + 1] {
        // Comparaison sur les trois pas sans court-circuit (pas de fuite par le temps de réponse).
        if code_at(secret, step) == want && last_step.is_none_or(|l| step > l) {
            found = Some(found.map_or(step, |f: i64| f.max(step)));
        }
    }
    found
}

/// URI `otpauth://` lue par les applications d'authentification (Aegis, Google Authenticator, 1Password…).
pub fn otpauth_uri(issuer: &str, account: &str, secret_b32: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret_b32}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(account),
        enc(issuer)
    )
}

/// Code QR de l'URI, en SVG encodé pour une balise `<img src="data:…">` (la CSP autorise `data:` pour les images).
pub fn qr_data_url(uri: &str) -> Option<String> {
    let code = qrcode::QrCode::new(uri.as_bytes()).ok()?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build();
    Some(format!(
        "data:image/svg+xml;base64,{}",
        data_encoding::BASE64.encode(svg.as_bytes())
    ))
}

// ---------- Chiffrement au repos ----------

const NONCE_LEN: usize = 12;

/// Chiffre `plain` : `nonce (12 o) || chiffré+étiquette`. Nonce aléatoire par appel.
pub fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    let unbound =
        UnboundKey::new(&AES_256_GCM, key).unwrap_or_else(|_| panic!("clé AES-256 de 32 octets"));
    let key = LessSafeKey::new(unbound);
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .unwrap_or_else(|_| panic!("générateur aléatoire indisponible"));
    let mut buf = plain.to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .unwrap_or_else(|_| panic!("chiffrement AES-GCM"));
    let mut out = nonce.to_vec();
    out.extend(buf);
    out
}

/// Déchiffre ; `None` si la clé est mauvaise ou si les données ont été altérées.
pub fn open(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN + aead::AES_256_GCM.tag_len() {
        return None;
    }
    let (nonce, rest) = sealed.split_at(NONCE_LEN);
    let unbound = UnboundKey::new(&AES_256_GCM, key).ok()?;
    let key = LessSafeKey::new(unbound);
    let mut buf = rest.to_vec();
    let n: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    let plain = key
        .open_in_place(Nonce::assume_unique_for_key(n), Aad::empty(), &mut buf)
        .ok()?;
    Some(plain.to_vec())
}

// ---------- Codes de secours ----------

/// 10 codes à usage unique `XXXXX-XXXXX` (50 bits chacun), à montrer UNE fois ; seul leur condensat est conservé.
pub fn generate_recovery_codes() -> Vec<String> {
    (0..10)
        .map(|_| {
            let mut b = [0u8; 7];
            SystemRandom::new()
                .fill(&mut b)
                .unwrap_or_else(|_| panic!("générateur aléatoire indisponible"));
            let s = BASE32_NOPAD.encode(&b);
            format!("{}-{}", &s[..5], &s[5..10])
        })
        .collect()
}

pub fn normalize_recovery(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

pub fn recovery_hash(code: &str) -> String {
    use sha2::{Digest, Sha256};
    data_encoding::HEXLOWER.encode(&Sha256::digest(normalize_recovery(code).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn matches_the_rfc_6238_test_vectors() {
        // RFC 6238 annexe B (SHA-1) : codes à 8 chiffres ; les 6 derniers sont ceux d'un code à 6 chiffres.
        for (t, want8) in [
            (59, 94_287_082u32),
            (1_111_111_109, 7_081_804),
            (1_111_111_111, 14_050_471),
            (1_234_567_890, 89_005_924),
            (2_000_000_000, 69_279_037),
        ] {
            assert_eq!(code_at(RFC_SECRET, step_of(t)), want8 % 1_000_000, "t={t}");
        }
    }

    #[test]
    fn accepts_a_small_clock_skew_but_never_replays_a_code() {
        let now = 1_700_000_010;
        let step = step_of(now);
        let code = format!("{:06}", code_at(RFC_SECRET, step));
        assert_eq!(verify(RFC_SECRET, &code, now, None), Some(step));
        // Horloge de l'utilisateur en retard d'un pas : accepté.
        let late = format!("{:06}", code_at(RFC_SECRET, step - 1));
        assert_eq!(verify(RFC_SECRET, &late, now, None), Some(step - 1));
        // Deux pas d'écart : refusé.
        let old = format!("{:06}", code_at(RFC_SECRET, step - 2));
        assert_eq!(verify(RFC_SECRET, &old, now, None), None);
        // Rejeu : le même pas (ou un pas antérieur) n'est plus accepté.
        assert_eq!(verify(RFC_SECRET, &code, now, Some(step)), None);
        assert_eq!(verify(RFC_SECRET, &late, now, Some(step)), None);
        // Formats invalides.
        for bad in ["", "12345", "1234567", "12a456", "      "] {
            assert_eq!(verify(RFC_SECRET, bad, now, None), None, "{bad:?}");
        }
        // Espaces tolérés (« 123 456 »).
        let spaced = format!("{} {}", &code[..3], &code[3..]);
        assert_eq!(verify(RFC_SECRET, &spaced, now, None), Some(step));
    }

    #[test]
    fn secret_is_encrypted_and_tampering_is_detected() {
        let key = [7u8; 32];
        let sealed = seal(&key, RFC_SECRET);
        assert!(
            !sealed.windows(RFC_SECRET.len()).any(|w| w == RFC_SECRET),
            "pas de secret en clair"
        );
        assert_eq!(open(&key, &sealed).as_deref(), Some(RFC_SECRET));
        assert_ne!(
            seal(&key, RFC_SECRET),
            sealed,
            "nonce différent à chaque fois"
        );
        assert!(open(&[8u8; 32], &sealed).is_none(), "mauvaise clé");
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(open(&key, &bad).is_none(), "données altérées");
        assert!(open(&key, &[0u8; 5]).is_none());
    }

    #[test]
    fn recovery_codes_are_unique_normalized_and_hashed() {
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), 10);
        let uniq: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(uniq.len(), 10);
        for c in &codes {
            assert_eq!(c.len(), 11);
            assert_eq!(
                recovery_hash(c),
                recovery_hash(&c.to_lowercase().replace('-', " "))
            );
        }
        assert_ne!(recovery_hash(&codes[0]), recovery_hash(&codes[1]));
    }

    #[test]
    fn otpauth_uri_is_well_formed_and_qr_renders() {
        let u = otpauth_uri("nxtTorrent", "alice@master", "JBSWY3DPEHPK3PXP");
        assert!(u.starts_with(
            "otpauth://totp/nxtTorrent:alice%40master?secret=JBSWY3DPEHPK3PXP&issuer=nxtTorrent"
        ));
        assert!(qr_data_url(&u)
            .unwrap()
            .starts_with("data:image/svg+xml;base64,"));
    }
}
