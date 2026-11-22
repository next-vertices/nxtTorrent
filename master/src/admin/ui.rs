//! Interface web : fichiers statiques (Svelte, construits dans `ui/dist`) embarqués dans le binaire.
//! Si l'interface n'a pas été construite, une page de secours l'explique (le master fonctionne sans).
use axum::body::Body;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../ui/dist"]
struct Assets;

const FALLBACK: &str = include_str!("fallback.html");

/// Politique de sécurité de l'interface : AUCUN script ni style en ligne, uniquement des fichiers du
/// même serveur ; pas d'iframe ; formulaires et requêtes limités à l'origine.
const UI_CSP: &str =
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
connect-src 'self'; font-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

fn response(mime: &str, cache: &'static str, body: Vec<u8>) -> Response {
    let mut r = Response::new(Body::from(body));
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(UI_CSP),
    );
    r
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webmanifest" => "application/manifest+json",
        "json" => "application/json",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Sert un fichier embarqué. Les chemins sans extension (routes de l'application, ex. `/torrents`)
/// retombent sur `index.html` ; `/api/*` inconnu est un 404 JSON, jamais une page HTML.
pub async fn handler(method: Method, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "route inconnue" })),
        )
            .into_response();
    }
    // Hors API, seuls GET et HEAD ont un sens (fichiers statiques).
    if !matches!(method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let wanted = if path.is_empty() { "index.html" } else { path };
    if let Some(f) = Assets::get(wanted) {
        // Les fichiers de `/assets/` portent un condensat dans leur nom : cache longue durée.
        let cache = if wanted.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        return response(mime_of(wanted), cache, f.data.into_owned());
    }
    // Fichier introuvable : un « chemin d'application » (sans extension) = l'application elle-même.
    let is_route = !wanted.rsplit('/').next().unwrap_or("").contains('.');
    if is_route {
        if let Some(f) = Assets::get("index.html") {
            return response("text/html; charset=utf-8", "no-store", f.data.into_owned());
        }
        return response(
            "text/html; charset=utf-8",
            "no-store",
            FALLBACK.as_bytes().to_vec(),
        );
    }
    (StatusCode::NOT_FOUND, "introuvable").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types() {
        assert_eq!(
            mime_of("assets/app-abc.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_of("assets/app-abc.css"), "text/css; charset=utf-8");
        assert_eq!(mime_of("favicon.svg"), "image/svg+xml");
        assert_eq!(mime_of("favicon.ico"), "image/x-icon");
        assert_eq!(mime_of("apple-touch-icon.png"), "image/png");
        assert_eq!(mime_of("site.webmanifest"), "application/manifest+json");
        assert_eq!(mime_of("x.bin"), "application/octet-stream");
    }
}
