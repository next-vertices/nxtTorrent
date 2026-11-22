//! CA intégrée : génération, persistance (clé 0600), émission de certificats serveur et nœud.
use proto::{fsutil::write_secret_atomic, pairing::ca_fingerprint, pairing::FP_LEN};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CaError {
    #[error("E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("certificat: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("CA invalide sur disque")]
    BadCa,
    #[error("CA incomplète dans {0} : ca.crt et ca.key doivent exister ensemble (restaurez les deux fichiers, ou supprimez le survivant pour créer une nouvelle CA — ce qui exige de ré-appairer tous les nœuds)")]
    Incomplete(String),
}

pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
    pub cert_pem: String,
    cert_der: Vec<u8>,
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>, CaError> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    data_encoding::BASE64
        .decode(body.as_bytes())
        .map_err(|_| CaError::BadCa)
}

impl Ca {
    /// Charge la CA du répertoire, ou la crée au premier lancement.
    pub fn load_or_create(dir: &Path) -> Result<Self, CaError> {
        proto::fsutil::ensure_private_dir(dir)?;
        let (crt, key) = (dir.join("ca.crt"), dir.join("ca.key"));
        // Une moitié de CA ne doit JAMAIS déclencher une régénération silencieuse : l'empreinte
        // changerait et tous les agents appairés cesseraient de fonctionner.
        if crt.exists() != key.exists() {
            return Err(CaError::Incomplete(dir.display().to_string()));
        }
        if crt.exists() && key.exists() {
            let cert_pem = std::fs::read_to_string(&crt)?;
            let kp = KeyPair::from_pem(&std::fs::read_to_string(&key)?)?;
            let issuer = Issuer::from_ca_cert_pem(&cert_pem, kp)?;
            let cert_der = pem_to_der(&cert_pem)?;
            return Ok(Self {
                issuer,
                cert_pem,
                cert_der,
            });
        }
        let kp = KeyPair::generate()?;
        let mut p = CertificateParams::new(Vec::<String>::new())?;
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "nxtTorrent CA");
        p.distinguished_name = dn;
        p.not_before = rcgen::date_time_ymd(2024, 1, 1);
        p.not_after = rcgen::date_time_ymd(2124, 1, 1);
        let cert = p.self_signed(&kp)?;
        let cert_pem = cert.pem();
        write_secret_atomic(&key, kp.serialize_pem().as_bytes())?;
        write_secret_atomic(&crt, cert_pem.as_bytes())?;
        let cert_der = cert.der().to_vec();
        let issuer = Issuer::new(p, kp);
        Ok(Self {
            issuer,
            cert_pem,
            cert_der,
        })
    }

    pub fn fingerprint(&self) -> [u8; FP_LEN] {
        ca_fingerprint(&self.cert_der)
    }

    /// Certificat serveur (régénéré à chaque démarrage : seul l'épinglage de la CA compte).
    /// Retourne (chaîne PEM leaf+CA, clé privée PEM).
    pub fn server_identity(&self, sans: &[String]) -> Result<(String, String), CaError> {
        let kp = KeyPair::generate()?;
        let mut p = CertificateParams::new(Vec::<String>::new())?;
        p.subject_alt_names = sans
            .iter()
            .map(|s| match s.parse::<IpAddr>() {
                Ok(ip) => Ok(SanType::IpAddress(ip)),
                Err(_) => s.clone().try_into().map(SanType::DnsName),
            })
            .collect::<Result<_, rcgen::Error>>()?;
        p.is_ca = IsCa::NoCa;
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.not_before = time_in_past(Duration::from_secs(60));
        p.not_after = time_in(Duration::from_secs(365 * 86400));
        let cert = p.signed_by(&kp, &self.issuer)?;
        Ok((
            format!("{}{}", cert.pem(), self.cert_pem),
            kp.serialize_pem(),
        ))
    }

    /// Signe le CSR d'un nœud. L'identité (SAN) est imposée par le master, jamais lue du CSR.
    pub fn sign_node_csr(
        &self,
        csr_pem: &str,
        node_id: &str,
        validity: Duration,
    ) -> Result<String, CaError> {
        // `from_pem` vérifie la signature du CSR (preuve de possession de la clé).
        let mut csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
        let p = &mut csr.params;
        p.subject_alt_names = vec![SanType::DnsName(format!("node-{node_id}").try_into()?)];
        p.distinguished_name = DistinguishedName::new();
        p.is_ca = IsCa::NoCa;
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        // Marge d'horloge : 60 s au plus, et jamais plus de 5 % de la validité.
        p.not_before = time_in_past(Duration::from_secs(60).min(validity / 20));
        p.not_after = time_in(validity);
        p.custom_extensions.clear();
        Ok(csr.signed_by(&self.issuer)?.pem())
    }
}

fn time_in(d: Duration) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() + time::Duration::try_from(d).unwrap_or(time::Duration::DAY)
}

fn time_in_past(d: Duration) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() - time::Duration::try_from(d).unwrap_or(time::Duration::MINUTE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_a_ca_is_a_hard_error_never_a_silent_regeneration() {
        for lone in ["ca.key", "ca.crt"] {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join(lone);
            std::fs::write(&p, "contenu précieux").unwrap();
            assert!(matches!(
                Ca::load_or_create(d.path()),
                Err(CaError::Incomplete(_))
            ));
            assert_eq!(
                std::fs::read_to_string(&p).unwrap(),
                "contenu précieux",
                "{lone} intact"
            );
        }
    }

    #[test]
    fn ca_is_stable_across_restarts() {
        let d = tempfile::tempdir().unwrap();
        let fp = Ca::load_or_create(d.path()).unwrap().fingerprint();
        assert_eq!(Ca::load_or_create(d.path()).unwrap().fingerprint(), fp);
        assert!(!d.path().join("ca.key.tmp").exists());
    }
}
