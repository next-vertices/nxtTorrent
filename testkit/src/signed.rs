//! Release signée avec une clé jetable, servie comme les fichiers d'une release GitHub (`<base><tag>/<fichier>`).
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use proto::release::{build_manifest, sha256_hex, Asset, ManifestInput, TrustedKey};
use proto::version::Version;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Signataire de test.
pub struct Signer {
    pair: Ed25519KeyPair,
    pub key: TrustedKey,
}

impl Default for Signer {
    fn default() -> Self {
        Self::new()
    }
}

impl Signer {
    pub fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap_or_else(|e| panic!("clé : {e}"));
        let pair =
            Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap_or_else(|e| panic!("clé : {e}"));
        let mut public = [0u8; 32];
        public.copy_from_slice(pair.public_key().as_ref());
        Self {
            pair,
            key: TrustedKey { public },
        }
    }

    /// `(manifeste, signature)` pour ces fichiers.
    pub fn release(&self, version: &str, assets: &[(&str, &[u8])]) -> (Vec<u8>, Vec<u8>) {
        let version = Version::parse(version).unwrap_or_else(|| panic!("version {version}"));
        let assets: BTreeMap<String, Asset> = assets
            .iter()
            .map(|(n, d)| {
                (
                    (*n).to_owned(),
                    Asset {
                        sha256: sha256_hex(d),
                        size: d.len() as u64,
                    },
                )
            })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let manifest = build_manifest(&ManifestInput {
            version: &version,
            key_id: &self.key.id(),
            issued_at: now - 60,
            expires_at: now + 30 * 24 * 3600,
            assets: &assets,
        });
        let sig = self.pair.sign(&manifest).as_ref().to_vec();
        (manifest, sig)
    }
}

/// Serveur de fichiers de release.
pub struct ReleaseServer {
    pub signer: Signer,
    files: Files,
    base: String,
}

async fn serve(State(f): State<Files>, Path(path): Path<String>) -> Response {
    let found = f
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&path)
        .cloned();
    match found {
        Some(b) => b.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

impl ReleaseServer {
    pub async fn start() -> Self {
        let files: Files = Arc::default();
        let app = Router::new()
            .route("/{*path}", get(serve))
            .with_state(files.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("addr: {e}"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            signer: Signer::new(),
            files,
            base: format!("http://{addr}/"),
        }
    }

    /// Adresse à mettre à la place de celle des releases GitHub.
    pub fn base_url(&self) -> String {
        self.base.clone()
    }

    pub fn publish(&self, version: &str, assets: &[(&str, &[u8])]) {
        let (manifest, sig) = self.signer.release(version, assets);
        self.put(version, "manifest.json", manifest);
        self.put(version, "manifest.json.sig", sig);
        for (name, data) in assets {
            self.put(version, name, data.to_vec());
        }
    }

    /// Remplace (ou ajoute) un fichier publié : pour simuler une altération.
    pub fn put(&self, version: &str, name: &str, data: Vec<u8>) {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(format!("v{version}/{name}"), data);
    }

    pub fn remove(&self, version: &str, name: &str) {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&format!("v{version}/{name}"));
    }
}
