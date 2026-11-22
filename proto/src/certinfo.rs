//! Inspection minimale d'un certificat de nœud (identité SAN + validité).
use x509_parser::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// `node_id` extrait du SAN DNS `node-<uuid>` (None si absent/invalide).
    pub node_id: Option<String>,
    pub not_before: i64,
    pub not_after: i64,
}

impl CertInfo {
    pub fn total_secs(&self) -> i64 {
        self.not_after - self.not_before
    }

    /// Vrai s'il reste moins d'un tiers de la validité totale.
    pub fn needs_renewal(&self, now: i64) -> bool {
        (self.not_after - now) * 3 < self.total_secs()
    }
}

pub fn parse_der(der: &[u8]) -> Option<CertInfo> {
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    let mut node_id = None;
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for n in &san.value.general_names {
            if let GeneralName::DNSName(d) = n {
                if let Some(id) = crate::parse_node_san(d) {
                    node_id = Some(id);
                    break;
                }
            }
        }
    }
    let v = cert.validity();
    Some(CertInfo {
        node_id,
        not_before: v.not_before.timestamp(),
        not_after: v.not_after.timestamp(),
    })
}

pub fn parse_pem(pem: &str) -> Option<CertInfo> {
    let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).ok()?;
    parse_der(&p.contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_threshold() {
        let c = CertInfo {
            node_id: None,
            not_before: 0,
            not_after: 300,
        };
        assert!(!c.needs_renewal(100)); // reste 200 > 100
        assert!(c.needs_renewal(201)); // reste 99 < 100
    }
}
