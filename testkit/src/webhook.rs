//! Récepteur de webhook de test : conserve les corps JSON reçus.
use axum::{extract::State, routing::post, Router};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct WebhookSink {
    pub addr: SocketAddr,
    bodies: Arc<Mutex<Vec<String>>>,
}

async fn recv(State(b): State<Arc<Mutex<Vec<String>>>>, body: String) {
    b.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(body);
}

impl WebhookSink {
    pub async fn start() -> Self {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/hook", post(recv))
            .with_state(bodies.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("addr: {e}"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, bodies }
    }
    pub fn url(&self) -> String {
        format!("http://{}/hook", self.addr)
    }
    pub fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
