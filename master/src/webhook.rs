//! Notifier webhook générique : POST JSON sur une URL (compatible ntfy/Gotify/Discord-like via relais).
use crate::notify::{Notification, Notifier, PresenceKind};
use std::time::Duration;

pub struct WebhookNotifier {
    client: reqwest::Client,
    url: reqwest::Url,
}

impl WebhookNotifier {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(url)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "le webhook doit être en http(s)"
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { client, url })
    }
}

impl Notifier for WebhookNotifier {
    fn notify(&self, n: &Notification) {
        let event = match n.kind {
            PresenceKind::WentOffline => "node_offline",
            PresenceKind::CameOnline => "node_online",
        };
        let ts = crate::store::unix_now();
        let body = serde_json::json!({ "event": event, "node_id": n.node_id, "timestamp": ts });
        let (client, url) = (self.client.clone(), self.url.clone());
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            // L'URL peut contenir un secret (topic/token) : jamais loggée.
            if let Err(e) = client
                .post(url)
                .json(&body)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
            {
                tracing::warn!(error = %e.without_url(), "échec d'envoi du webhook");
            }
        });
    }
}
