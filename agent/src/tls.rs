//! TLS côté agent : vérification de la CA par empreinte (premier contact).
use hyper_util::rt::TokioIo;
use proto::pairing::{ca_fingerprint, FP_LEN};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tonic::transport::{Channel, Endpoint, Uri};

/// Vérificateur « trust on first use » ancré sur l'empreinte du code d'appairage.
///
/// Le serveur présente `leaf + CA`. On exige qu'un certificat de la chaîne ait l'empreinte
/// attendue, puis on valide NORMALEMENT (chaîne, dates, nom d'hôte, signature) avec cette CA
/// comme unique ancre de confiance. Aucune vérification n'est désactivée.
#[derive(Debug)]
pub struct PinnedCaVerifier {
    fp: [u8; FP_LEN],
    provider: Arc<CryptoProvider>,
}

impl PinnedCaVerifier {
    pub fn new(fp: [u8; FP_LEN]) -> Self {
        Self {
            fp,
            provider: Arc::new(ring::default_provider()),
        }
    }
}

impl ServerCertVerifier for PinnedCaVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let ca = intermediates
            .iter()
            .find(|c| ca_fingerprint(c) == self.fp)
            .ok_or_else(|| Error::General("empreinte de la CA inattendue".into()))?;
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone().into_owned())?;
        let inner =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), self.provider.clone())
                .build()
                .map_err(|e| Error::General(e.to_string()))?;
        inner.verify_server_cert(end_entity, &[], server_name, ocsp, now)
    }

    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            m,
            c,
            d,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            d,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Canal gRPC vers `addr` (`host:port`) avec la configuration TLS fournie.
pub async fn connect_channel(addr: &str, mut cfg: ClientConfig) -> anyhow::Result<Channel> {
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    let host = addr
        .rsplit_once(':')
        .map(|(h, _)| h.trim_matches(['[', ']']))
        .ok_or_else(|| anyhow::anyhow!("adresse attendue au format host:port"))?
        .to_owned();
    let server_name = ServerName::try_from(host)?;
    let connector = TlsConnector::from(Arc::new(cfg));
    let target = addr.to_owned();
    let svc = tower::service_fn(move |_: Uri| {
        let (connector, name, target) = (connector.clone(), server_name.clone(), target.clone());
        async move {
            let tcp = TcpStream::connect(target).await?;
            let tls = connector.connect(name, tcp).await?;
            Ok::<_, std::io::Error>(TokioIo::new(tls))
        }
    });
    let ep = Endpoint::from_shared("http://orchestrator.invalid".to_owned())?;
    Ok(ep.connect_with_connector(svc).await?)
}

pub fn pinned_client_config(fp: [u8; FP_LEN]) -> Result<ClientConfig, Error> {
    Ok(
        ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .dangerous() // API rustls pour un vérificateur personnalisé ; la vérification reste complète.
            .with_custom_certificate_verifier(Arc::new(PinnedCaVerifier::new(fp)))
            .with_no_client_auth(),
    )
}
