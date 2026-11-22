//! Normalisation des infohash et validation des liens magnet (partagées master/agent).

/// Normalise un infohash : 40 hex (casse libre) ou 32 base32 -> 40 hex minuscules.
pub fn normalize_infohash(s: &str) -> Option<String> {
    let s = s.trim();
    match s.len() {
        40 if s.bytes().all(|b| b.is_ascii_hexdigit()) => Some(s.to_ascii_lowercase()),
        32 => data_encoding::BASE32_NOPAD
            .decode(s.to_ascii_uppercase().as_bytes())
            .ok()
            .filter(|b| b.len() == 20)
            .map(|b| data_encoding::HEXLOWER.encode(&b)),
        _ => None,
    }
}

/// Valide un lien magnet et en extrait l'infohash.
pub fn magnet_infohash(m: &str) -> Option<String> {
    if m.len() > 4096 || m.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let q = m.get(..8).filter(|p| p.eq_ignore_ascii_case("magnet:?"))?;
    let rest = &m[q.len()..];
    rest.split('&')
        .find_map(|kv| kv.strip_prefix("xt=urn:btih:"))
        .and_then(normalize_infohash)
}
