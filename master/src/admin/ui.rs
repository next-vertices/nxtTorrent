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

/// Sert un fichier embarqué. Les chemins sans extension (routes de l'application, ex. `/torrents`) et la racine `/`
/// retombent sur `index.html` ; `/api/*` inconnu est un 404 JSON, jamais une page HTML. Sans interface construite (aucun
/// `index.html` embarqué), la racine et les routes affichent la page de secours.
pub async fn handler(method: Method, uri: Uri) -> Response {
    serve(&method, uri.path(), |name| {
        Assets::get(name).map(|f| f.data.into_owned())
    })
}

/// Cœur du service des fichiers, avec la recherche d'un fichier embarqué passée en paramètre (testable sans dépendre de
/// l'état du dossier `ui/dist` au moment de la compilation).
fn serve(method: &Method, uri_path: &str, lookup: impl Fn(&str) -> Option<Vec<u8>>) -> Response {
    let path = uri_path.trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "route inconnue" })),
        )
            .into_response();
    }
    // Hors API, seuls GET et HEAD ont un sens (fichiers statiques).
    if !matches!(*method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let wanted = if path.is_empty() { "index.html" } else { path };
    if let Some(data) = lookup(wanted) {
        // Les fichiers de `/assets/` portent un condensat dans leur nom : cache longue durée.
        let cache = if wanted.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        return response(mime_of(wanted), cache, data);
    }
    // Fichier introuvable : la racine, ou un « chemin d'application » (sans extension), c'est l'application elle-même.
    let is_route = path.is_empty() || !wanted.rsplit('/').next().unwrap_or("").contains('.');
    if is_route {
        if let Some(data) = lookup("index.html") {
            return response("text/html; charset=utf-8", "no-store", data);
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

    async fn body(r: Response) -> String {
        let b = axum::body::to_bytes(r.into_body(), 1 << 20)
            .await
            .unwrap_or_default();
        String::from_utf8_lossy(&b).into_owned()
    }

    #[tokio::test]
    async fn without_a_built_interface_the_root_and_app_routes_show_the_fallback_page() {
        let none = |_: &str| None;
        for p in ["/", "", "/torrents", "/nodes/abc"] {
            let r = serve(&Method::GET, p, none);
            assert_eq!(r.status(), StatusCode::OK, "{p:?}");
            assert!(r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html"));
            assert_eq!(r.headers()["cache-control"], "no-store");
            assert!(r.headers().contains_key("content-security-policy"));
            assert_eq!(body(r).await, FALLBACK, "{p:?}");
        }
        // Un FICHIER absent reste un 404 (jamais la page de secours à la place d'une icône ou d'un script).
        for p in ["/favicon.ico", "/assets/app-abc.js", "/x.png"] {
            assert_eq!(
                serve(&Method::GET, p, none).status(),
                StatusCode::NOT_FOUND,
                "{p}"
            );
        }
        // L'API inconnue : 404 JSON ; méthode autre que GET/HEAD : 405.
        assert_eq!(
            serve(&Method::GET, "/api/x", none).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve(&Method::POST, "/", none).status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn with_a_built_interface_files_are_served_and_routes_fall_back_to_index() {
        let files = |n: &str| match n {
            "index.html" => Some(b"<html>app</html>".to_vec()),
            "assets/app-1.js" => Some(b"js".to_vec()),
            _ => None,
        };
        let r = serve(&Method::GET, "/", files);
        assert_eq!(body(r).await, "<html>app</html>");
        assert_eq!(
            body(serve(&Method::GET, "/torrents", files)).await,
            "<html>app</html>"
        );
        let js = serve(&Method::GET, "/assets/app-1.js", files);
        assert_eq!(
            js.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            serve(&Method::GET, "/absent.png", files).status(),
            StatusCode::NOT_FOUND
        );
    }
}
