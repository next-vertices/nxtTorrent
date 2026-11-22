//! Code d'appairage : `CODE-` + base32 de (secret 80 bits || empreinte CA tronquée 128 bits).
//!
//! L'empreinte permet à l'agent d'épingler la CA dès le premier contact (anti-MITM).
//! 128 bits d'un SHA-256 suffisent : trouver une CA alternative revient à une seconde pré-image.

use data_encoding::BASE32_NOPAD;
use sha2::{Digest, Sha256};

pub const SECRET_LEN: usize = 10;
pub const FP_LEN: usize = 16;
const PREFIX: &str = "CODE-";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodeError {
    #[error("format de code invalide")]
    Malformed,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub secret: [u8; SECRET_LEN],
    pub ca_fp: [u8; FP_LEN],
}

// Pas de `Debug` dérivé : le secret ne doit jamais finir dans un log.
impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingCode(<redacted>)")
    }
}

/// Empreinte tronquée d'une CA (DER).
pub fn ca_fingerprint(ca_der: &[u8]) -> [u8; FP_LEN] {
    let full = Sha256::digest(ca_der);
    let mut fp = [0u8; FP_LEN];
    fp.copy_from_slice(&full[..FP_LEN]);
    fp
}

impl PairingCode {
    pub fn new(secret: [u8; SECRET_LEN], ca_fp: [u8; FP_LEN]) -> Self {
        Self { secret, ca_fp }
    }

    pub fn encode(&self) -> String {
        let mut raw = Vec::with_capacity(SECRET_LEN + FP_LEN);
        raw.extend_from_slice(&self.secret);
        raw.extend_from_slice(&self.ca_fp);
        let b32 = BASE32_NOPAD.encode(&raw);
        let groups: Vec<&str> = b32
            .as_bytes()
            .chunks(6)
            .map(|c| std::str::from_utf8(c).unwrap_or_default())
            .collect();
        format!("{PREFIX}{}", groups.join("-"))
    }

    pub fn parse(s: &str) -> Result<Self, CodeError> {
        let s = s.trim();
        let body = s
            .get(..PREFIX.len())
            .filter(|p| p.eq_ignore_ascii_case(PREFIX))
            .map(|_| &s[PREFIX.len()..])
            .ok_or(CodeError::Malformed)?;
        let clean: String = body
            .chars()
            .filter(|c| *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let raw = BASE32_NOPAD
            .decode(clean.as_bytes())
            .map_err(|_| CodeError::Malformed)?;
        if raw.len() != SECRET_LEN + FP_LEN {
            return Err(CodeError::Malformed);
        }
        let mut secret = [0u8; SECRET_LEN];
        let mut ca_fp = [0u8; FP_LEN];
        secret.copy_from_slice(&raw[..SECRET_LEN]);
        ca_fp.copy_from_slice(&raw[SECRET_LEN..]);
        Ok(Self { secret, ca_fp })
    }

    /// Condensat stocké côté master (le secret a 80 bits d'entropie : SHA-256 suffit, cf. ADR 0001).
    pub fn secret_hash(&self) -> String {
        secret_hash(&self.secret)
    }
}

pub fn secret_hash(secret: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = PairingCode::new([7; SECRET_LEN], [9; FP_LEN]);
        let s = c.encode();
        assert!(s.starts_with("CODE-"));
        assert_eq!(PairingCode::parse(&s).unwrap(), c);
        assert_eq!(PairingCode::parse(&s.to_lowercase()).unwrap(), c);
    }

    #[test]
    fn rejects_garbage() {
        assert!(PairingCode::parse("").is_err());
        assert!(PairingCode::parse("CODE-AAAA").is_err());
        assert!(PairingCode::parse("XXXX-AAAA").is_err());
        assert!(PairingCode::parse("CODE-!!!!").is_err());
    }

    #[test]
    fn debug_is_redacted() {
        let c = PairingCode::new([1; SECRET_LEN], [2; FP_LEN]);
        assert!(!format!("{c:?}").contains('1'));
    }
}
