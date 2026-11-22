//! Faux indexeur Torznab : `/api?t=caps|search` (clé d'API exigée) et `/dl/<n>` (le .torrent). Sert aux tests du
//! cross-seed sans indexeur réel.
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// Réponse imposée à TOUTE requête (recherche, caps, téléchargement) : simule un indexeur qui se protège.
#[derive(Clone, Debug, Default)]
pub enum Behavior {
    #[default]
    Normal,
    /// Statut HTTP (ex. 429, 403, 503) avec, éventuellement, un `Retry-After` en secondes.
    Status(u16, Option<u64>),
    /// `<error code=… description=…/>` Torznab avec statut 200.
    Error(u32, &'static str),
}

#[derive(Default)]
struct St {
    behavior: Behavior,
    /// Instant de chaque requête reçue (toutes routes), pour vérifier le rythme.
    times: Vec<std::time::Instant>,
    key: String,
    /// (titre, contenu du .torrent)
    results: Vec<(String, Vec<u8>)>,
    searches: Vec<String>,
    downloads: Vec<usize>,
    /// Texte de la taille annoncée, par résultat (None : taille du .torrent).
    announce_size: Option<u64>,
}

#[derive(Clone)]
pub struct MockTorznab {
    pub addr: SocketAddr,
    st: Arc<Mutex<St>>,
}

type S = State<Arc<Mutex<St>>>;

fn lock(s: &Arc<Mutex<St>>) -> std::sync::MutexGuard<'_, St> {
    s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn xml(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

fn denied() -> Response {
    xml(
        r#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#
            .into(),
    )
}

/// Applique le comportement imposé ; `Some(réponse)` si la requête ne doit pas aller plus loin.
fn imposed(st: &mut St) -> Option<Response> {
    st.times.push(std::time::Instant::now());
    match st.behavior.clone() {
        Behavior::Normal => None,
        Behavior::Status(code, retry) => {
            let mut r = StatusCode::from_u16(code)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
                .into_response();
            if let Some(s) = retry {
                if let Ok(v) = s.to_string().parse() {
                    r.headers_mut().insert(header::RETRY_AFTER, v);
                }
            }
            Some(r)
        }
        Behavior::Error(code, d) => Some(xml(format!(
            r#"<?xml version="1.0"?><error code="{code}" description="{d}"/>"#
        ))),
    }
}

async fn api(State(s): S, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut st = lock(&s);
    if let Some(r) = imposed(&mut st) {
        return r;
    }
    if q.get("apikey").map(String::as_str) != Some(st.key.as_str()) {
        return denied();
    }
    match q.get("t").map(String::as_str) {
        Some("caps") => xml(r#"<?xml version="1.0"?><caps><server title="mock"/></caps>"#.into()),
        Some("search") => {
            st.searches.push(q.get("q").cloned().unwrap_or_default());
            let mut out = String::from(
                r#"<?xml version="1.0"?><rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>"#,
            );
            for (i, (title, bytes)) in st.results.iter().enumerate() {
                // Comme un vrai indexeur : taille du CONTENU (pas du fichier .torrent).
                let size = st
                    .announce_size
                    .or_else(|| proto::metainfo::parse(bytes).ok().map(|m| m.size))
                    .unwrap_or(bytes.len() as u64);
                out.push_str(&format!(
                    "<item><title>{title}</title><link>@@BASE@@/dl/{i}?apikey={}</link><size>{size}</size></item>",
                    st.key
                ));
            }
            out.push_str("</channel></rss>");
            xml(out)
        }
        _ => xml(r#"<error code="202" description="No such function"/>"#.into()),
    }
}

async fn dl(
    State(s): S,
    Path(i): Path<usize>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mut st = lock(&s);
    if let Some(r) = imposed(&mut st) {
        return r;
    }
    if q.get("apikey").map(String::as_str) != Some(st.key.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    st.downloads.push(i);
    match st.results.get(i) {
        Some((_, b)) => b.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

impl MockTorznab {
    pub async fn start(api_key: &str) -> Self {
        let st = Arc::new(Mutex::new(St {
            key: api_key.to_owned(),
            ..St::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("addr: {e}"));
        let base = format!("http://{addr}");
        // Les liens de téléchargement portent l'adresse réelle du serveur : on la substitue à la volée.
        let st2 = st.clone();
        let app = Router::new()
            .route(
                "/api",
                get({
                    let base = base.clone();
                    move |state: S, q: Query<HashMap<String, String>>| {
                        let base = base.clone();
                        async move {
                            let r = api(state, q).await;
                            substitute(r, &base).await
                        }
                    }
                }),
            )
            .route("/dl/{i}", get(dl))
            .with_state(st2);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, st }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    /// Résultats renvoyés par toute recherche (titre, contenu du .torrent).
    pub fn set_results(&self, r: Vec<(&str, Vec<u8>)>) {
        lock(&self.st).results = r.into_iter().map(|(t, b)| (t.to_owned(), b)).collect();
    }
    /// Force la taille annoncée dans les résultats (None : celle du .torrent).
    pub fn set_announced_size(&self, size: Option<u64>) {
        lock(&self.st).announce_size = size;
    }
    /// Impose une réponse à toutes les requêtes suivantes.
    pub fn set_behavior(&self, b: Behavior) {
        lock(&self.st).behavior = b;
    }
    /// Nombre total de requêtes reçues (recherches, caps et téléchargements).
    pub fn requests(&self) -> usize {
        lock(&self.st).times.len()
    }
    /// Instant de chaque requête reçue.
    pub fn request_times(&self) -> Vec<std::time::Instant> {
        lock(&self.st).times.clone()
    }
    pub fn searches(&self) -> Vec<String> {
        lock(&self.st).searches.clone()
    }
    pub fn downloads(&self) -> Vec<usize> {
        lock(&self.st).downloads.clone()
    }
}

async fn substitute(r: Response, base: &str) -> Response {
    let (parts, body) = r.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).replace("@@BASE@@", base);
    Response::from_parts(parts, axum::body::Body::from(text))
}
