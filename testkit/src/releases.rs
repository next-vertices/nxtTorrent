//! Faux serveur de releases (forme de l'API GitHub) : liste JSON configurable, statut imposable, compteur de requêtes.
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct St {
    body: String,
    status: u16,
    hits: usize,
    user_agents: Vec<String>,
    /// En-têtes reçus (noms), pour vérifier qu'aucune donnée sur l'installation ne sort.
    header_names: Vec<String>,
}

#[derive(Clone)]
pub struct MockReleases {
    pub addr: SocketAddr,
    st: Arc<Mutex<St>>,
}

fn lock(s: &Arc<Mutex<St>>) -> std::sync::MutexGuard<'_, St> {
    s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn list(State(st): State<Arc<Mutex<St>>>, headers: HeaderMap) -> Response {
    let mut g = lock(&st);
    g.hits += 1;
    g.user_agents.push(
        headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned(),
    );
    g.header_names = headers.keys().map(|k| k.as_str().to_owned()).collect();
    let status = StatusCode::from_u16(g.status).unwrap_or(StatusCode::OK);
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        g.body.clone(),
    )
        .into_response()
}

impl MockReleases {
    /// `tags` : étiquettes servies, la première étant la plus récente ; une étiquette en `-rc` est une pré-version.
    pub async fn start(tags: &[&str]) -> Self {
        let m = Self {
            addr: "127.0.0.1:0".parse().unwrap_or_else(|e| panic!("{e}")),
            st: Arc::new(Mutex::new(St {
                status: 200,
                ..St::default()
            })),
        };
        m.set_tags(tags);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("addr: {e}"));
        let app = Router::new()
            .route("/releases", get(list))
            .with_state(m.st.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, ..m }
    }

    pub fn url(&self) -> String {
        format!("http://{}/releases", self.addr)
    }

    pub fn set_tags(&self, tags: &[&str]) {
        let items: Vec<String> = tags
            .iter()
            .map(|t| {
                format!(
                    r#"{{"tag_name":"{t}","prerelease":{},"draft":false,"published_at":"2026-11-23T20:00:00Z","html_url":"https://evil.example/{t}"}}"#,
                    t.contains('-')
                )
            })
            .collect();
        lock(&self.st).body = format!("[{}]", items.join(","));
    }

    /// Corps brut (réponse démesurée, JSON invalide…).
    pub fn set_body(&self, body: String) {
        lock(&self.st).body = body;
    }

    pub fn set_status(&self, status: u16) {
        lock(&self.st).status = status;
    }

    pub fn hits(&self) -> usize {
        lock(&self.st).hits
    }

    pub fn user_agents(&self) -> Vec<String> {
        lock(&self.st).user_agents.clone()
    }

    pub fn header_names(&self) -> Vec<String> {
        lock(&self.st).header_names.clone()
    }
}
