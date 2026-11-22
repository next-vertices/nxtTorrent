//! Routes et handlers de l'API d'administration.
use super::describe::{self, Names};
use super::password;
use super::session::Sessions;
use super::AdminSettings;
use crate::limiter::FailureLimiter;
use crate::manage::{ManageError, NewTorrent, Source};
use crate::overview::TorrentQuery;
use crate::store::unix_now;
use crate::{Core, PAIRING_CODE_TTL};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

const COOKIE: &str = "nxt_session";
/// En-tête exigé sur toute requête qui modifie l'état : un site tiers ne peut pas l'ajouter sans
/// préalable CORS (que ce serveur n'accorde jamais).
const CSRF_HEADER: &str = "x-requested-with";
const CSRF_VALUE: &str = "nxt";

pub struct AdminState {
    pub core: Arc<Core>,
    settings: AdminSettings,
    sessions: Sessions,
    ip_failures: FailureLimiter<IpAddr>,
    user_failures: FailureLimiter<String>,
    /// Évite de remplir l'audit quand un attaquant insiste : un seul « bloqué » par IP et par fenêtre.
    block_logged: FailureLimiter<IpAddr>,
    /// Secrets TOTP en cours de configuration (jamais écrits en base avant que le code soit vérifié).
    pending_totp:
        std::sync::Mutex<std::collections::HashMap<String, (Vec<u8>, std::time::Instant)>>,
    /// Connexions dont le mot de passe est validé et qui attendent le code TOTP.
    tickets: std::sync::Mutex<std::collections::HashMap<String, Ticket>>,
}

/// Mot de passe validé, code TOTP attendu : valable peu de temps, quelques essais, lié à l'adresse source.
struct Ticket {
    user: String,
    ip: IpAddr,
    created: std::time::Instant,
    attempts: u8,
}

const TICKET_TTL: Duration = Duration::from_secs(300);
const TICKET_MAX_ATTEMPTS: u8 = 5;
const PENDING_TOTP_TTL: Duration = Duration::from_secs(600);

impl AdminState {
    pub fn new(core: Arc<Core>, s: AdminSettings) -> Arc<Self> {
        Arc::new(Self {
            sessions: Sessions::new(s.idle_timeout, s.absolute_timeout),
            ip_failures: FailureLimiter::new(s.max_failures_per_ip, s.lockout_window),
            user_failures: FailureLimiter::new(s.max_failures_per_user, s.lockout_window),
            block_logged: FailureLimiter::new(1, s.lockout_window),
            pending_totp: std::sync::Mutex::new(std::collections::HashMap::new()),
            tickets: std::sync::Mutex::new(std::collections::HashMap::new()),
            settings: s,
            core,
        })
    }
}

// ---------- Erreurs ----------

struct ApiError {
    status: StatusCode,
    message: String,
    retry_after: Option<Duration>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
        }
    }
    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "authentification requise")
    }
    fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = (self.status, Json(json!({ "error": self.message }))).into_response();
        if let Some(d) = self.retry_after {
            if let Ok(v) = HeaderValue::from_str(&d.as_secs().max(1).to_string()) {
                r.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        r
    }
}

impl From<crate::store::StoreError> for ApiError {
    fn from(e: crate::store::StoreError) -> Self {
        tracing::error!(error = %e, "base de données");
        Self::internal()
    }
}

impl From<ManageError> for ApiError {
    fn from(e: ManageError) -> Self {
        match e {
            ManageError::Invalid(m) if m.contains("inconnu") => Self::new(StatusCode::NOT_FOUND, m),
            ManageError::Invalid(m) => Self::new(StatusCode::BAD_REQUEST, m),
            ManageError::Store(e) => e.into(),
        }
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ---------- Routeur et garde-fous communs ----------

pub fn router(state: Arc<AdminState>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/me", get(me))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/login/totp", post(login_totp))
        .route("/api/logout", post(logout))
        .route("/api/account", get(account))
        .route("/api/info", get(info))
        .route("/api/backup", post(backup))
        .route(
            "/api/replication",
            get(replication_get).post(replication_set),
        )
        .route("/api/nodes/{id}/browse", get(node_browse))
        .route("/api/torrent-jobs", get(jobs_list).post(jobs_start))
        .route("/api/torrent-jobs/{id}", get(jobs_get).delete(jobs_delete))
        .route("/api/torrent-jobs/{id}/file", get(jobs_file))
        .route("/api/crossseed", get(xseed_get))
        .route("/api/crossseed/mode", post(xseed_set_mode))
        .route("/api/crossseed/run", post(xseed_run))
        .route("/api/crossseed/indexers", post(xseed_indexer_add))
        .route("/api/crossseed/indexers/test", post(xseed_indexer_test))
        .route(
            "/api/crossseed/indexers/{id}/limits",
            post(xseed_indexer_limits),
        )
        .route(
            "/api/crossseed/indexers/{id}",
            post(xseed_indexer_enable).delete(xseed_indexer_delete),
        )
        .route("/api/alerts", get(alerts_get))
        .route("/api/alerts/rules", post(alerts_set_rules))
        .route("/api/alerts/channels", post(alerts_add_channel))
        .route("/api/alerts/channels/{id}", delete(alerts_delete_channel))
        .route(
            "/api/alerts/channels/{id}/enabled",
            post(alerts_enable_channel),
        )
        .route("/api/alerts/channels/{id}/test", post(alerts_test_channel))
        .route("/api/agent-install", post(agent_install))
        .route("/api/totp/setup", post(totp_setup))
        .route("/api/totp/enable", post(totp_enable))
        .route("/api/totp/disable", post(totp_disable))
        .route("/api/password", post(change_password))
        .route("/api/nodes", get(nodes))
        .route("/api/nodes/{id}/revoke", post(revoke_node))
        .route("/api/nodes/{id}/rename", post(rename_node))
        .route("/api/nodes/{id}/labels", post(set_labels))
        .route("/api/nodes/{id}/location", post(set_location))
        .route("/api/nodes/{id}/limits", post(set_limits))
        .route("/api/nodes/stats", get(node_stats))
        .route("/api/pairing-codes", post(pairing_code))
        .route("/api/stats", get(stats))
        .route("/api/metrics", get(metrics))
        .route("/api/live", get(live))
        .route("/api/torrents", get(list_torrents).post(add_torrent))
        .route(
            "/api/torrents/file",
            post(add_torrent_file).layer(DefaultBodyLimit::max(
                proto::metainfo::MAX_TORRENT_FILE + 4096,
            )),
        )
        .route("/api/torrents/bulk", post(bulk_torrents))
        .route("/api/torrents/{ih}/pause", post(pause_torrent))
        .route("/api/torrents/{ih}/resume", post(resume_torrent))
        .route("/api/torrents/{ih}", delete(remove_torrent))
        .route("/api/audit", get(audit)) // Tout le reste = l'interface web (fichiers statiques embarqués) ; /api/* inconnu = 404 JSON.
        .fallback(axum::routing::any(super::ui::handler))
        .layer(middleware::from_fn(guard))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

/// CSRF + en-têtes de sécurité sur toutes les réponses.
async fn guard(req: Request, next: Next) -> Response {
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if mutating {
        let ok_header = req
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case(CSRF_VALUE));
        // Si le navigateur indique une origine, elle doit être celle du serveur (même hôte:port).
        let origin_ok = match (
            req.headers().get(header::ORIGIN),
            req.headers().get(header::HOST),
        ) {
            (Some(o), Some(h)) => o
                .to_str()
                .ok()
                .and_then(|o| o.split_once("://").map(|(_, rest)| rest))
                .is_some_and(|o| Some(o) == h.to_str().ok()),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if !ok_header || !origin_ok {
            return ApiError::new(StatusCode::FORBIDDEN, "requête refusée (protection CSRF)")
                .into_response();
        }
    }
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    // `or_insert` : l'interface web (fichiers statiques) fixe sa propre politique de cache et sa CSP.
    h.entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(
            "default-src 'none'; frame-ancestors 'none'",
        ));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

fn token_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|c| c.split(';'))
        .filter_map(|p| {
            p.trim()
                .strip_prefix(&format!("{COOKIE}="))
                .map(str::to_owned)
        })
        .next()
}

/// Utilisateur authentifié (session valide) ou 401.
fn authed(st: &AdminState, headers: &HeaderMap) -> ApiResult<(String, String)> {
    let token = token_from(headers).ok_or_else(ApiError::unauthorized)?;
    // Les rafraîchissements automatiques de l'interface (en-tête passif) ne comptent pas comme une activité :
    // un onglet laissé ouvert ne doit pas garder la session vivante indéfiniment.
    let user = if headers.contains_key("x-nxt-passive") {
        st.sessions
            .is_valid(&token)
            .then(|| st.sessions.user_of(&token))
            .flatten()
    } else {
        st.sessions.lookup(&token)
    }
    .ok_or_else(ApiError::unauthorized)?;
    Ok((user, token))
}

fn actor(user: &str) -> String {
    format!("admin:{user}")
}

/// Nettoie une valeur fournie par un client avant de l'écrire dans l'audit (longueur, contrôles).
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// `Secure` seulement si la requête est arrivée en HTTPS via un proxy (`X-Forwarded-Proto: https`).
/// Le décider d'après la configuration serait faux : en HTTP simple (Docker, boucle locale) un cookie
/// `Secure` n'est pas renvoyé par certains clients et la session serait perdue à chaque requête.
fn via_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

fn session_cookie(headers: &HeaderMap, token: &str, max_age: u64) -> String {
    let secure = if via_https(headers) { "; Secure" } else { "" };
    format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}")
}

// ---------- Compte et session ----------

#[derive(Serialize)]
struct Health {
    ok: bool,
    setup_required: bool,
}

async fn health(State(st): State<Arc<AdminState>>) -> ApiResult<Json<Health>> {
    Ok(Json(Health {
        ok: true,
        setup_required: st.core.store.admin_count()? == 0,
    }))
}

/// Utilisateur de la session courante (l'interface s'en sert pour savoir si elle doit afficher la connexion).
async fn me(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    Ok(Json(json!({ "username": user })))
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

pub fn valid_username(u: &str) -> bool {
    (3..=32).contains(&u.len())
        && u.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Création du premier compte, au premier démarrage, et SEULEMENT depuis la machine locale : même si
/// l'API est exposée (`--admin-allow-remote`), personne d'autre ne peut s'emparer d'un master neuf.
async fn setup(
    State(st): State<Arc<AdminState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(c): Json<Credentials>,
) -> ApiResult<Response> {
    if !setup_allowed(addr.ip(), &st.settings) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "l'initialisation n'est possible que depuis la machine locale (ou via `master admin set-password`, \
             ou avec MASTER_ADMIN_ALLOW_REMOTE_SETUP=1 si le port n'est exposé qu'en local)",
        ));
    }
    if !valid_username(&c.username) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "nom d'utilisateur invalide (3 à 32 caractères : lettres, chiffres, _ . -)",
        ));
    }
    password::validate_strength(&c.password, &c.username)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    let pw = c.password.clone();
    let hash = tokio::task::spawn_blocking(move || password::hash(&pw))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|_| ApiError::internal())?;
    if !st.core.store.create_first_admin(&c.username, &hash)? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "un compte administrateur existe déjà",
        ));
    }
    st.core
        .store
        .audit(&actor(&c.username), "admin_created", &addr.ip().to_string());
    Ok((StatusCode::CREATED, Json(json!({ "ok": true }))).into_response())
}

/// Le premier compte se crée depuis la machine locale, sauf option explicite (cf. `AdminSettings`).
fn setup_allowed(ip: IpAddr, settings: &AdminSettings) -> bool {
    ip.is_loopback() || settings.allow_remote_setup
}

async fn login(
    State(st): State<Arc<AdminState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(c): Json<Credentials>,
) -> ApiResult<Response> {
    let ip = addr.ip();
    let user_key = c.username.to_lowercase();
    // Blocage anti brute-force : par IP, puis par compte.
    let wait = st
        .ip_failures
        .blocked_for(&ip)
        .max(st.user_failures.blocked_for(&user_key));
    if let Some(wait) = wait {
        if !st.block_logged.is_blocked(ip) {
            st.block_logged.record_failure(ip);
            st.core.store.audit(
                "anonymous",
                "admin_login_blocked",
                &format!("{ip} user={}", clean(&c.username)),
            );
        }
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "trop d'échecs de connexion, réessayez plus tard",
        );
        e.retry_after = Some(wait);
        return Err(e);
    }

    let stored = st.core.store.admin_hash(&c.username)?;
    let pw = c.password.clone();
    let ok = tokio::task::spawn_blocking(move || match stored {
        Some(h) => password::verify(&pw, &h),
        None => {
            password::dummy_verify(&pw); // même coût qu'un mauvais mot de passe
            false
        }
    })
    .await
    .map_err(|_| ApiError::internal())?;

    if !ok {
        st.ip_failures.record_failure(ip);
        st.user_failures.record_failure(user_key);
        st.core.store.audit(
            "anonymous",
            "admin_login_failed",
            &format!("{ip} user={}", clean(&c.username)),
        );
        // Message identique pour « compte inconnu » et « mauvais mot de passe ».
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "identifiants invalides",
        ));
    }
    // Double authentification : le mot de passe seul ne suffit plus. Les compteurs d'échecs ne sont PAS remis à zéro
    // ici (sinon un attaquant qui connaît le mot de passe pourrait essayer des codes sans limite).
    if st.core.store.admin_totp_enabled(&c.username)? {
        let ticket = new_ticket(&st, &c.username, ip);
        st.core.store.audit(
            "anonymous",
            "admin_login_password_ok",
            &format!("{ip} user={}", clean(&c.username)),
        );
        return Ok(Json(json!({ "needs_totp": true, "ticket": ticket })).into_response());
    }
    st.ip_failures.clear(&ip);
    st.user_failures.clear(&user_key);
    let token = st.sessions.create(&c.username);
    st.core
        .store
        .audit(&actor(&c.username), "admin_login", &ip.to_string());
    let cookie = session_cookie(&headers, &token, st.settings.absolute_timeout.as_secs());
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "ok": true, "username": c.username })),
    )
        .into_response())
}

fn new_ticket(st: &AdminState, user: &str, ip: IpAddr) -> String {
    use rand::RngCore;
    let mut raw = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let t = data_encoding::HEXLOWER.encode(&raw);
    let mut m = st
        .tickets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    m.retain(|_, v| v.created.elapsed() < TICKET_TTL);
    m.insert(
        t.clone(),
        Ticket {
            user: user.to_owned(),
            ip,
            created: std::time::Instant::now(),
            attempts: 0,
        },
    );
    t
}

#[derive(Deserialize)]
struct TotpLogin {
    ticket: String,
    /// Code à 6 chiffres de l'application, ou code de secours.
    code: String,
}

/// Vérifie un code TOTP, ou à défaut un code de secours. `Ok(true)` si accepté (pas de temps réservé / code consommé).
async fn check_second_factor(st: &AdminState, user: &str, code: &str) -> ApiResult<(bool, bool)> {
    let now = unix_now();
    if let Some((secret, last)) = st.core.store.admin_totp_secret(user)? {
        if let Some(step) = super::totp::verify(&secret, code, now, last) {
            // Réservation atomique : deux requêtes simultanées avec le même code ne passent pas toutes les deux.
            if st.core.store.admin_totp_claim_step(user, step)? {
                return Ok((true, false));
            }
            return Ok((false, false));
        }
    }
    // Code de secours (à usage unique).
    let h = super::totp::recovery_hash(code);
    if code.chars().filter(|c| c.is_ascii_alphanumeric()).count() == 10
        && st.core.store.admin_recovery_consume(user, &h)?
    {
        return Ok((true, true));
    }
    Ok((false, false))
}

async fn login_totp(
    State(st): State<Arc<AdminState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<TotpLogin>,
) -> ApiResult<Response> {
    let ip = addr.ip();
    let bad = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "code invalide ou connexion expirée : recommencez",
        )
    };
    // Ticket : existe, récent, même adresse source, pas épuisé.
    let user = {
        let mut m = st
            .tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(t) = m.get_mut(&r.ticket) else {
            return Err(bad());
        };
        if t.created.elapsed() >= TICKET_TTL || t.ip != ip || t.attempts >= TICKET_MAX_ATTEMPTS {
            m.remove(&r.ticket);
            return Err(bad());
        }
        t.attempts += 1;
        t.user.clone()
    };
    let user_key = user.to_lowercase();
    if let Some(wait) = st
        .ip_failures
        .blocked_for(&ip)
        .max(st.user_failures.blocked_for(&user_key))
    {
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "trop d'échecs de connexion, réessayez plus tard",
        );
        e.retry_after = Some(wait);
        return Err(e);
    }
    let (ok, recovery) = check_second_factor(&st, &user, &r.code).await?;
    if !ok {
        st.ip_failures.record_failure(ip);
        st.user_failures.record_failure(user_key);
        st.core.store.audit(
            "anonymous",
            "admin_login_totp_failed",
            &format!("{ip} user={}", clean(&user)),
        );
        return Err(bad());
    }
    st.tickets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&r.ticket);
    st.ip_failures.clear(&ip);
    st.user_failures.clear(&user_key);
    let token = st.sessions.create(&user);
    if recovery {
        st.core.store.audit(
            &actor(&user),
            "admin_recovery_code_used",
            &st.core.store.admin_recovery_remaining(&user)?.to_string(),
        );
    }
    st.core
        .store
        .audit(&actor(&user), "admin_login", &format!("{ip} 2fa"));
    let cookie = session_cookie(&headers, &token, st.settings.absolute_timeout.as_secs());
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "ok": true, "username": user })),
    )
        .into_response())
}

// ---------- Alertes ----------

const MAX_CHANNELS: usize = 5;

fn channel_view(c: &crate::store::ChannelRow) -> serde_json::Value {
    // Jamais l'URL : elle peut contenir un secret. Seul l'hôte est montré.
    json!({ "id": c.id, "kind": c.kind, "label": c.label, "enabled": c.enabled, "hint": crate::alerts::url_hint(&c.url) })
}

async fn alerts_get(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let channels: Vec<_> = st
        .core
        .store
        .alert_channels()?
        .iter()
        .map(channel_view)
        .collect();
    Ok(Json(
        json!({ "rules": st.core.alert_rules(), "channels": channels }),
    ))
}

async fn alerts_set_rules(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<crate::alerts::Rules>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    r.validate()
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    st.core
        .store
        .alert_rules_set(&serde_json::to_string(&r).map_err(|_| ApiError::internal())?)?;
    st.core
        .store
        .audit(&actor(&user), "alert_rules_changed", "");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct NewChannel {
    kind: String,
    url: String,
    #[serde(default)]
    label: String,
}

async fn alerts_add_channel(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<NewChannel>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    let kind = crate::alerts::ChannelKind::parse(&r.kind).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "type de canal inconnu (webhook ou ntfy)",
        )
    })?;
    crate::alerts::parse_channel_url(&r.url)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    let label: String = r
        .label
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    if st.core.store.alert_channels()?.len() >= MAX_CHANNELS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{MAX_CHANNELS} canaux au maximum"),
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let label = if label.is_empty() {
        kind.as_str().to_owned()
    } else {
        label
    };
    st.core
        .store
        .alert_channel_insert(&id, kind.as_str(), &label, r.url.trim())?;
    st.core.store.audit(
        &actor(&user),
        "alert_channel_added",
        &format!("{label} ({})", kind.as_str()),
    );
    let ch = st
        .core
        .store
        .alert_channels()?
        .into_iter()
        .find(|c| c.id == id);
    Ok((StatusCode::CREATED, Json(ch.map(|c| channel_view(&c)))).into_response())
}

async fn alerts_delete_channel(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    let label = st
        .core
        .store
        .alert_channels()?
        .into_iter()
        .find(|c| c.id == id)
        .map(|c| c.label);
    if !st.core.store.alert_channel_delete(&id)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "canal inconnu"));
    }
    st.core.store.audit(
        &actor(&user),
        "alert_channel_removed",
        label.as_deref().unwrap_or(""),
    );
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct EnabledReq {
    enabled: bool,
}

async fn alerts_enable_channel(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<EnabledReq>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    if !st.core.store.alert_channel_enable(&id, r.enabled)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "canal inconnu"));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Envoie un message de test sur un canal. L'erreur renvoyée ne contient jamais l'URL.
async fn alerts_test_channel(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let ch = st
        .core
        .store
        .alert_channels()?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "canal inconnu"))?;
    let kind = crate::alerts::ChannelKind::parse(&ch.kind).ok_or_else(ApiError::internal)?;
    let ev = crate::alerts::Event {
        key: "test".into(),
        firing: true,
        severity: crate::alerts::Severity::Warning,
        title: "Test d'alerte nxtTorrent".into(),
        text: "Ceci est un message de test : ce canal est correctement configuré.".into(),
    };
    crate::alerts::send(kind, &ch.url, &ev)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("l'envoi a échoué : {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// Ce qu'il faut pour pré-remplir le script d'installation d'un agent.
async fn info(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    Ok(Json(json!({
        "agent_port": st.settings.agent_port,
        "agent_hosts": st.settings.agent_hosts,
        "binary_configured": !st.settings.agent_binary_url.is_empty(),
    })))
}

#[derive(Deserialize)]
struct InstallReq {
    #[serde(flatten)]
    req: super::install::InstallRequest,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

/// Crée un code d'appairage ET le script d'installation qui l'embarque. Le script (donc le code) n'est renvoyé
/// qu'ici, une fois ; le journal ne retient que le nom prévu et les labels, jamais le code.
async fn agent_install(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<InstallReq>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    // Valider AVANT de créer le code : une requête refusée ne laisse aucun code valide derrière elle.
    super::install::render(
        &r.req,
        "CODE-VALIDATION",
        &st.settings.agent_binary_url,
        &st.settings.agent_binary_sha256,
    )
    .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    let code = st
        .core
        .create_pairing_code_named(
            &actor(&user),
            PAIRING_CODE_TTL,
            &r.labels,
            r.name.as_deref(),
        )
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let rendered = super::install::render(
        &r.req,
        &code,
        &st.settings.agent_binary_url,
        &st.settings.agent_binary_sha256,
    )
    .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    st.core.store.audit(
        &actor(&user),
        "agent_install_created",
        &format!("{}|{}", r.name.as_deref().unwrap_or(""), r.labels.join(",")),
    );
    // Commande prête à coller : le script (base64 sur une ligne) est passé à `sh -c`, ce qui laisse l'entrée du
    // terminal libre pour la saisie du mot de passe du client torrent.
    let b64 = data_encoding::BASE64.encode(rendered.script.as_bytes());
    let command = format!("sudo sh -c \"$(echo '{b64}' | base64 -d)\"");
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "script": rendered.script,
            "command": command,
            "expires_at": unix_now() + PAIRING_CODE_TTL.as_secs() as i64,
            "binary_configured": !st.settings.agent_binary_url.is_empty(),
        })),
    )
        .into_response())
}

async fn account(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    Ok(Json(json!({
        "username": user,
        "totp_enabled": st.core.store.admin_totp_enabled(&user)?,
        "recovery_remaining": st.core.store.admin_recovery_remaining(&user)?,
    })))
}

async fn totp_setup(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    if st.core.store.admin_totp_enabled(&user)? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "la double authentification est déjà activée (désactivez-la d'abord pour la reconfigurer)",
        ));
    }
    let secret = super::totp::generate_secret();
    let b32 = super::totp::secret_b32(&secret);
    let uri = super::totp::otpauth_uri("nxtTorrent", &user, &b32);
    {
        let mut m = st
            .pending_totp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.retain(|_, (_, at)| at.elapsed() < PENDING_TOTP_TTL);
        m.insert(user.clone(), (secret.to_vec(), std::time::Instant::now()));
    }
    Ok(Json(json!({
        "secret": b32,
        "uri": uri,
        "qr": super::totp::qr_data_url(&uri),
    })))
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

async fn totp_enable(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<CodeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, token) = authed(&st, &headers)?;
    let user_key = user.to_lowercase();
    if let Some(wait) = st.user_failures.blocked_for(&user_key) {
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "trop d'échecs, réessayez plus tard",
        );
        e.retry_after = Some(wait);
        return Err(e);
    }
    let secret = {
        let m = st
            .pending_totp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.get(&user)
            .filter(|(_, at)| at.elapsed() < PENDING_TOTP_TTL)
            .map(|(s, _)| s.clone())
    }
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "aucune configuration en cours (ou expirée) : recommencez",
        )
    })?;
    let Some(step) = super::totp::verify(&secret, &r.code, unix_now(), None) else {
        st.user_failures.record_failure(user_key);
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "code incorrect : vérifiez l'heure de votre téléphone et réessayez",
        ));
    };
    let codes = super::totp::generate_recovery_codes();
    let hashes: Vec<String> = codes
        .iter()
        .map(|c| super::totp::recovery_hash(c))
        .collect();
    st.core.store.admin_totp_enable(&user, &secret, &hashes)?;
    // Le code qui vient de servir à confirmer ne pourra pas être rejoué pour se connecter.
    st.core.store.admin_totp_claim_step(&user, step)?;
    st.pending_totp
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&user);
    // Les autres sessions (peut-être volées) sont fermées ; celle-ci reste ouverte.
    st.sessions.remove_user_except(&user, Some(&token));
    st.core.store.audit(&actor(&user), "totp_enabled", "");
    Ok(Json(json!({ "ok": true, "recovery_codes": codes })))
}

// ---------- Création de .torrent sur un nœud ----------

fn job_err(e: crate::torrentjobs::JobError) -> ApiError {
    use crate::torrentjobs::JobError as E;
    let status = match &e {
        E::Invalid(_) => StatusCode::BAD_REQUEST,
        E::Offline | E::Busy(_) => StatusCode::CONFLICT,
        E::Timeout => StatusCode::GATEWAY_TIMEOUT,
        E::Unknown => StatusCode::NOT_FOUND,
    };
    ApiError::new(status, e.to_string())
}

#[derive(Deserialize, Default)]
struct BrowseQuery {
    #[serde(default)]
    path: String,
}

/// Contenu d'un dossier du nœud (sous sa racine locale) : sert à choisir le fichier ou le dossier à partager.
async fn node_browse(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<BrowseQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    if proto::parse_node_san(&format!("node-{id}")).is_none()
        || st.core.store.node_info(&id)?.is_none()
    {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    let l = st.core.browse_node(&id, &q.path).await.map_err(job_err)?;
    Ok(Json(json!({
        "path": q.path,
        "truncated": l.truncated,
        "entries": l.entries.iter().map(|e| json!({
            "name": e.name, "dir": e.dir, "size": e.size, "link": e.link,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct JobReq {
    node_id: String,
    /// Fichier ou dossier du nœud, relatif à sa racine.
    path: String,
    /// Trackers par paliers : `[["https://a/announce","udp://b"],["http://secours/announce"]]`.
    #[serde(default)]
    trackers: Vec<Vec<String>>,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    source: String,
    /// 0 = automatique.
    #[serde(default)]
    piece_length: u32,
    /// Ajoute le torrent au nœud pour qu'il vérifie les fichiers présents et partage.
    #[serde(default)]
    add_to_node: bool,
}

async fn jobs_start(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<JobReq>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    if proto::parse_node_san(&format!("node-{}", r.node_id)).is_none()
        || st.core.store.node_info(&r.node_id)?.is_none()
    {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    let id = st
        .core
        .start_torrent_job(
            &actor(&user),
            crate::torrentjobs::CreateRequest {
                node_id: r.node_id,
                path: r.path,
                tiers: r.trackers,
                comment: r.comment,
                private: r.private,
                source: r.source,
                piece_length: r.piece_length,
                add_to_node: r.add_to_node,
            },
        )
        .await
        .map_err(job_err)?;
    st.core.bump_live();
    Ok((StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response())
}

async fn jobs_list(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    Ok(Json(json!({ "items": st.core.torrent_jobs_list() })))
}

async fn jobs_get(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::torrentjobs::JobView>> {
    authed(&st, &headers)?;
    st.core
        .torrent_job(&id)
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "tâche inconnue ou expirée"))
}

/// Le fichier .torrent créé. L'adresse d'un tracker privé contient un passkey : jamais mis en cache.
async fn jobs_file(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    authed(&st, &headers)?;
    let Some((name, bytes)) = st.core.torrent_job_file(&id) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "pas de fichier : tâche inconnue, en cours ou échouée",
        ));
    };
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-bittorrent".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

/// Annule une création en cours, ou oublie une tâche terminée.
async fn jobs_delete(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let running = st
        .core
        .torrent_job(&id)
        .is_some_and(|j| j.state == "running");
    if running {
        st.core.cancel_torrent_job(&id).await.map_err(job_err)?;
    } else {
        st.core.forget_torrent_job(&id).map_err(job_err)?;
    }
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

// ---------- Cross-seed ----------

/// Configuration, indexeurs (sans clé) et historique du cross-seed.
async fn xseed_get(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let names: std::collections::HashMap<String, String> = st
        .core
        .store
        .node_details()?
        .into_iter()
        .map(|n| {
            let label = n.name.filter(|x| !x.is_empty()).unwrap_or(n.hostname);
            (n.id, label)
        })
        .collect();
    let torrent_names: std::collections::HashMap<String, String> = st
        .core
        .store
        .list_torrents()?
        .into_iter()
        .map(|t| (t.infohash, t.name))
        .collect();
    let log: Vec<serde_json::Value> = st
        .core
        .store
        .xseed_log(200)?
        .into_iter()
        .map(|r| {
            json!({
                "ts": r.ts,
                "parent": r.parent,
                "parent_name": torrent_names.get(&r.parent).cloned().unwrap_or_default(),
                "candidate": r.candidate,
                "node": names.get(&r.node_id).cloned().unwrap_or_default(),
                "indexer": r.indexer,
                "title": r.title,
                "tracker": r.tracker,
                "state": r.state,
                "detail": r.detail,
            })
        })
        .collect();
    let now = crate::store::unix_now();
    let mut indexers: Vec<serde_json::Value> = Vec::new();
    for i in st.core.store.indexers()? {
        let used_hour = st
            .core
            .store
            .indexer_requests_since(&i.id, (now - 3600) * 1000)?;
        let used_day = st
            .core
            .store
            .indexer_requests_since(&i.id, (now - 86_400) * 1000)?;
        // Un seul état lisible : ok | paused (pause en cours) | disabled (désactivé, à la main ou automatiquement).
        let state = if !i.enabled {
            "disabled"
        } else if i.paused_until > now {
            "paused"
        } else {
            "ok"
        };
        indexers.push(json!({
            "id": i.id, "name": i.name, "url": i.url, "enabled": i.enabled,
            "state": state,
            "min_interval_ms": i.min_interval_ms, "max_per_hour": i.max_per_hour,
            "max_per_day": i.max_per_day, "max_candidates": i.max_candidates,
            "paused_until": if i.paused_until > now { i.paused_until } else { 0 },
            "fail_count": i.fail_count,
            "last_error": i.last_error, "last_error_ts": i.last_error_ts,
            // Non vide : désactivé AUTOMATIQUEMENT (avec la raison) ; vide : à la main.
            "disabled_reason": i.disabled_reason,
            "used_hour": used_hour, "used_day": used_day,
        }));
    }
    Ok(Json(json!({
        "mode": st.core.xseed_mode().as_str(),
        "running": st.core.xseed_is_running(),
        "indexers": indexers,
        "log": log,
    })))
}

#[derive(Deserialize)]
struct XseedModeReq {
    mode: String,
}

async fn xseed_set_mode(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<XseedModeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    let Some(mode) = crate::crossseed::XseedMode::parse(&r.mode) else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "mode inconnu (off, dry ou active)",
        ));
    };
    st.core.store.setting_set("xseed_mode", mode.as_str())?;
    st.core
        .store
        .audit(&actor(&user), "xseed_mode_changed", mode.as_str());
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

async fn xseed_run(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    if st.core.xseed_mode() == crate::crossseed::XseedMode::Off {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Multi-trackers est désactivé",
        ));
    }
    let started = st.core.xseed_run_in_background();
    if started {
        st.core.store.audit(&actor(&user), "xseed_run", "");
    }
    st.core.bump_live();
    Ok(Json(json!({ "started": started })))
}

#[derive(Deserialize)]
struct IndexerReq {
    name: String,
    url: String,
    api_key: String,
    /// Réglages de prudence (facultatifs : défauts conservateurs).
    #[serde(flatten)]
    limits: IndexerLimitsReq,
}

/// Réglages de prudence d'un indexeur : chaque indexeur a SES règles (un compte peut être banni pour des requêtes
/// trop fréquentes), donc rien n'est figé dans le code.
#[derive(Deserialize, Default)]
struct IndexerLimitsReq {
    /// Délai minimal entre deux requêtes, en millisecondes (500 à 3 600 000).
    min_interval_ms: Option<u64>,
    /// Requêtes au plus par heure glissante (1 à 1 000), recherches ET téléchargements de .torrent confondus.
    max_per_hour: Option<u32>,
    /// Requêtes au plus par 24 h glissantes (1 à 20 000).
    max_per_day: Option<u32>,
    /// Résultats téléchargés au plus par recherche (1 à 50).
    max_candidates: Option<u32>,
}

impl IndexerLimitsReq {
    /// Valeurs validées, les défauts conservateurs complétant ce qui manque.
    fn resolve(&self) -> Result<(u64, u32, u32, u32), ApiError> {
        let d = crate::store::INDEXER_DEFAULTS;
        let v = (
            self.min_interval_ms.unwrap_or(d.0),
            self.max_per_hour.unwrap_or(d.1),
            self.max_per_day.unwrap_or(d.2),
            self.max_candidates.unwrap_or(d.3),
        );
        let bad = |m: &'static str| Err(ApiError::new(StatusCode::BAD_REQUEST, m));
        if !(500..=3_600_000).contains(&v.0) {
            return bad("délai entre requêtes : 500 ms à 1 h");
        }
        if !(1..=1000).contains(&v.1) {
            return bad("requêtes par heure : 1 à 1 000");
        }
        if !(1..=20_000).contains(&v.2) {
            return bad("requêtes par jour : 1 à 20 000");
        }
        if !(1..=50).contains(&v.3) {
            return bad("résultats examinés par recherche : 1 à 50");
        }
        Ok(v)
    }
}

fn check_indexer(r: &IndexerReq) -> Result<(), ApiError> {
    let name = r.name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "nom invalide (1 à 60 caractères)",
        ));
    }
    if r.api_key.trim().is_empty()
        || r.api_key.len() > 200
        || r.api_key.chars().any(char::is_control)
    {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "clé d'API invalide"));
    }
    crate::torznab::check_base_url(r.url.trim())
        .map(|_| ())
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))
}

/// Teste une configuration (URL + clé) SANS l'enregistrer.
async fn xseed_indexer_test(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<IndexerReq>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    check_indexer(&r)?;
    // Chaque test avec une mauvaise clé est une tentative d'accès : on espace les essais pour ne pas faire bannir le
    // compte de l'utilisateur qui insiste.
    if let Err(wait) = st.core.xseed_test_allowed() {
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("attendez {wait} s avant un nouveau test : un indexeur peut sanctionner les essais répétés"),
        );
        e.retry_after = Some(Duration::from_secs(wait));
        return Err(e);
    }
    let c = crate::torznab::Torznab::new(r.url.trim(), r.api_key.trim())
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let res = c.test().await;
    st.core
        .xseed_test_done(matches!(&res, Err(crate::torznab::TorznabError::Auth(_))));
    match res {
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        // Message de l'indexeur ou erreur générique : jamais la clé, jamais le détail réseau.
        Err(e) => Ok(Json(json!({ "ok": false, "error": e.to_string() }))),
    }
}

async fn xseed_indexer_add(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<IndexerReq>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    check_indexer(&r)?;
    let limits = r.limits.resolve()?;
    if st.core.store.indexers()?.len() >= 20 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "20 indexeurs au maximum",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    st.core
        .store
        .indexer_add(&id, r.name.trim(), r.url.trim(), r.api_key.trim())?;
    st.core
        .store
        .indexer_set_limits(&id, limits.0, limits.1, limits.2, limits.3)?;
    // Journal : le nom seulement, jamais l'URL ni la clé.
    st.core
        .store
        .audit(&actor(&user), "xseed_indexer_added", r.name.trim());
    st.core.bump_live();
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))).into_response())
}

/// Modifie les réglages de prudence d'un indexeur.
async fn xseed_indexer_limits(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<IndexerLimitsReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    let v = r.resolve()?;
    if !st.core.store.indexer_set_limits(&id, v.0, v.1, v.2, v.3)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "indexeur inconnu"));
    }
    st.core.store.audit(
        &actor(&user),
        "xseed_indexer_limits",
        &format!("{id}|{}|{}|{}|{}", v.0, v.1, v.2, v.3),
    );
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct EnableReq {
    enabled: bool,
}

async fn xseed_indexer_enable(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<EnableReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    if !st.core.store.indexer_enable(&id, r.enabled)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "indexeur inconnu"));
    }
    st.core.store.audit(
        &actor(&user),
        "xseed_indexer_enabled",
        &format!("{id} {}", r.enabled),
    );
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

async fn xseed_indexer_delete(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    if !st.core.store.indexer_delete(&id)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "indexeur inconnu"));
    }
    st.core
        .store
        .audit(&actor(&user), "xseed_indexer_deleted", &id);
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

/// État de la réplication entre nœuds (tracker privé, copies en cours, plafond de débit).
async fn replication_get(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let torrents = st.core.store.list_torrents()?;
    let with_group = torrents
        .iter()
        .filter(|t| t.replicate_group.is_some() && !t.deleted)
        .count();
    let waiting = torrents
        .iter()
        .filter(|t| t.replicate_group.is_some() && !t.has_file && !t.deleted)
        .count();
    Ok(Json(json!({
        "enabled": st.core.swarm().is_some(),
        "tracker_url": st.core.swarm().map(|s| s.public_base.clone()),
        "copy_dl_limit": st.core.copy_dl_limit(),
        "transfers": st.core.store.count_transfers()?,
        "magnet_waiting": if st.core.swarm().is_some() { waiting } else { 0 },
        "with_group": with_group,
    })))
}

#[derive(Deserialize)]
struct ReplicationReq {
    /// Plafond de téléchargement d'une copie, octets/s ; 0 = illimité.
    copy_dl_limit: u64,
}

async fn replication_set(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<ReplicationReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    const MAX: u64 = 100 * 1024 * 1024 * 1024;
    if r.copy_dl_limit > MAX {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "limite trop élevée"));
    }
    st.core
        .store
        .setting_set("copy_dl_limit", &r.copy_dl_limit.to_string())?;
    st.core.store.audit(
        &actor(&user),
        "replication_limit_changed",
        &r.copy_dl_limit.to_string(),
    );
    // Les copies en cours reçoivent le nouveau plafond au prochain passage.
    st.core.schedule_pending().await;
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BackupReq {
    password: String,
    /// Code TOTP ou de secours : exigé si la double authentification est activée.
    #[serde(default)]
    code: String,
    passphrase: String,
}

/// Télécharge une sauvegarde chiffrée. Elle contient la clé privée de la CA : une session volée ne doit pas suffire,
/// on exige donc le mot de passe (et le code de double authentification s'il est activé).
async fn backup(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<BackupReq>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    let user_key = user.to_lowercase();
    if let Some(wait) = st.user_failures.blocked_for(&user_key) {
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "trop d'échecs, réessayez plus tard",
        );
        e.retry_after = Some(wait);
        return Err(e);
    }
    let stored = st
        .core
        .store
        .admin_hash(&user)?
        .ok_or_else(ApiError::unauthorized)?;
    let pw = r.password.clone();
    let pw_ok = tokio::task::spawn_blocking(move || password::verify(&pw, &stored))
        .await
        .map_err(|_| ApiError::internal())?;
    let code_ok = pw_ok
        && (!st.core.store.admin_totp_enabled(&user)?
            || check_second_factor(&st, &user, &r.code).await?.0);
    if !(pw_ok && code_ok) {
        st.user_failures.record_failure(user_key);
        st.core.store.audit(&actor(&user), "backup_refused", "");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "mot de passe ou code incorrect",
        ));
    }
    let data_dir = st.settings.data_dir.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "sauvegarde indisponible dans ce mode (pas de dossier de données)",
        )
    })?;
    let (core, pass) = (st.core.clone(), r.passphrase.clone());
    let bytes =
        tokio::task::spawn_blocking(move || crate::backup::create(&data_dir, &core.store, &pass))
            .await
            .map_err(|_| ApiError::internal())?
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    st.core.store.audit(&actor(&user), "backup_created", "");
    let day = time::OffsetDateTime::now_utc().date();
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"nxttorrent-backup-{day}.nxtbak\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
struct DisableTotp {
    password: String,
    code: String,
}

async fn totp_disable(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<DisableTotp>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, token) = authed(&st, &headers)?;
    let user_key = user.to_lowercase();
    if let Some(wait) = st.user_failures.blocked_for(&user_key) {
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "trop d'échecs, réessayez plus tard",
        );
        e.retry_after = Some(wait);
        return Err(e);
    }
    // Les DEUX preuves sont exigées : une session volée ne suffit pas à retirer la protection.
    let stored = st
        .core
        .store
        .admin_hash(&user)?
        .ok_or_else(ApiError::unauthorized)?;
    let pw = r.password.clone();
    let pw_ok = tokio::task::spawn_blocking(move || password::verify(&pw, &stored))
        .await
        .map_err(|_| ApiError::internal())?;
    // Le mot de passe d'abord : un mauvais mot de passe ne doit pas consommer un code de secours ni un pas TOTP.
    let code_ok = pw_ok && check_second_factor(&st, &user, &r.code).await?.0;
    if !(pw_ok && code_ok) {
        st.user_failures.record_failure(user_key);
        st.core
            .store
            .audit(&actor(&user), "totp_disable_failed", "");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "mot de passe ou code incorrect",
        ));
    }
    st.core.store.admin_totp_disable(&user)?;
    st.sessions.remove_user_except(&user, Some(&token));
    st.core.store.audit(&actor(&user), "totp_disabled", "");
    Ok(Json(json!({ "ok": true })))
}

async fn logout(State(st): State<Arc<AdminState>>, headers: HeaderMap) -> ApiResult<Response> {
    let (user, token) = authed(&st, &headers)?;
    st.sessions.remove(&token);
    st.core.store.audit(&actor(&user), "admin_logout", "");
    Ok((
        [(header::SET_COOKIE, session_cookie(&headers, "", 0))],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

#[derive(Deserialize)]
struct ChangePassword {
    current: String,
    new: String,
}

async fn change_password(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(c): Json<ChangePassword>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, token) = authed(&st, &headers)?;
    password::validate_strength(&c.new, &user)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    let stored = st
        .core
        .store
        .admin_hash(&user)?
        .ok_or_else(ApiError::unauthorized)?;
    let (cur, new) = (c.current.clone(), c.new.clone());
    let hash = tokio::task::spawn_blocking(move || {
        password::verify(&cur, &stored).then(|| password::hash(&new))
    })
    .await
    .map_err(|_| ApiError::internal())?;
    let Some(hash) = hash else {
        st.core
            .store
            .audit(&actor(&user), "admin_password_change_failed", "");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "mot de passe actuel incorrect",
        ));
    };
    st.core
        .store
        .upsert_admin(&user, &hash.map_err(|_| ApiError::internal())?)?;
    // Les autres sessions (peut-être volées) sont fermées ; celle-ci reste ouverte.
    st.sessions.remove_user_except(&user, Some(&token));
    st.core
        .store
        .audit(&actor(&user), "admin_password_changed", "");
    Ok(Json(json!({ "ok": true })))
}

// ---------- Nœuds ----------

#[derive(Serialize)]
struct NodeView {
    id: String,
    /// Nom affiché : celui choisi par l'admin, sinon le nom d'hôte.
    name: String,
    /// Nom choisi par l'admin (None = pas encore nommé).
    custom_name: Option<String>,
    /// Localisation saisie par l'admin (texte libre).
    location: Option<String>,
    /// Dernière adresse source vue par le master.
    last_ip: Option<String>,
    /// Limites de débit voulues (octets/s ; 0 = illimité) ; `limits_managed` faux = réglages locaux du client.
    limits_managed: bool,
    dl_limit: u64,
    ul_limit: u64,
    /// Santé réseau et constats (voir `health`).
    network: crate::health::Assessment,
    hostname: String,
    agent_version: String,
    labels: Vec<String>,
    state: String,
    /// Tunnel actif en ce moment.
    connected: bool,
    created_at: i64,
    last_seen: Option<i64>,
    offline_since: Option<i64>,
    desired_rev: u64,
    applied_rev: u64,
    client_kind: String,
    client_version: String,
    disk_free: u64,
    torrent_count: usize,
    dl_rate: u64,
    ul_rate: u64,
}

fn node_views(st: &AdminState) -> ApiResult<Vec<NodeView>> {
    let mut digests = st.core.node_digests(|id| st.core.sessions.is_connected(id));
    Ok(st
        .core
        .store
        .node_details()?
        .into_iter()
        .map(|n| {
            let t = digests
                .remove(&n.id)
                .unwrap_or_else(crate::NodeDigest::empty);
            let connected = st.core.sessions.is_connected(&n.id);
            NodeView {
                network: t.network,
                location: n.location,
                last_ip: n.last_ip,
                limits_managed: n.limits_managed,
                dl_limit: n.dl_limit,
                ul_limit: n.ul_limit,
                connected,
                name: n
                    .name
                    .clone()
                    .filter(|x| !x.is_empty())
                    .unwrap_or_else(|| n.hostname.clone()),
                custom_name: n.name,
                id: n.id,
                hostname: n.hostname,
                agent_version: n.agent_version,
                labels: n.labels,
                state: n.state,
                created_at: n.created_at,
                last_seen: n.last_seen,
                offline_since: n.offline_since,
                desired_rev: n.desired_rev,
                applied_rev: t.applied_rev,
                client_kind: t.client_kind,
                client_version: t.client_version,
                disk_free: t.disk_free,
                torrent_count: t.torrent_count,
                dl_rate: t.dl_rate,
                ul_rate: t.ul_rate,
            }
        })
        .collect())
}

async fn nodes(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<NodeView>>> {
    authed(&st, &headers)?;
    Ok(Json(node_views(&st)?))
}

#[derive(Deserialize)]
struct RenameReq {
    /// Vide = retour au nom d'hôte.
    name: String,
}

async fn rename_node(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<RenameReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    let name = r.name.trim();
    if !name.is_empty() && !crate::valid_node_name(name) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "nom invalide (1 à 64 caractères, sans caractère de contrôle)",
        ));
    }
    if proto::parse_node_san(&format!("node-{id}")).is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    let old = st
        .core
        .store
        .node_details()?
        .into_iter()
        .find(|n| n.id == id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"))?;
    st.core.store.set_node_name(&id, Some(name))?;
    st.core.store.audit(
        &actor(&user),
        "node_renamed",
        &format!(
            "{id}|{}|{}",
            old.name.unwrap_or(old.hostname),
            if name.is_empty() {
                "(nom d'hôte)"
            } else {
                name
            }
        ),
    );
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}
async fn revoke_node(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    // Identifiant strict : jamais de valeur libre vers la couche métier.
    if proto::parse_node_san(&format!("node-{id}")).is_none()
        || st.core.store.node_info(&id)?.is_none()
    {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    st.core.revoke_node_as(&actor(&user), &id);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LocationReq {
    /// Vide = effacer.
    location: String,
}

/// Localisation d'un nœud : texte libre saisi par l'admin (« Hetzner · Falkenstein »). Pas de géolocalisation
/// automatique : elle exigerait une base d'adresses embarquée ou un service externe.
async fn set_location(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<LocationReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    let loc = r.location.trim();
    if loc.chars().count() > 80 || loc.chars().any(char::is_control) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "localisation invalide (80 caractères maximum, sans caractère de contrôle)",
        ));
    }
    if proto::parse_node_san(&format!("node-{id}")).is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    if !st.core.store.set_node_location(&id, Some(loc))? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    st.core.store.audit(
        &actor(&user),
        "node_location_changed",
        &format!("{id}|{loc}"),
    );
    st.core.bump_live();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LimitsReq {
    /// Octets par seconde ; 0 = illimité.
    dl_limit: u64,
    ul_limit: u64,
}

/// Limites de débit d'un nœud : enregistrées (état voulu) puis envoyées tout de suite si le nœud est connecté ;
/// réappliquées à chaque reconnexion.
async fn set_limits(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<LimitsReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    const MAX: u64 = 100 * 1024 * 1024 * 1024; // 100 Gio/s : au-delà, c'est une faute de frappe
    if r.dl_limit > MAX || r.ul_limit > MAX {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "limite trop élevée"));
    }
    if proto::parse_node_san(&format!("node-{id}")).is_none()
        || !st.core.store.set_node_limits(&id, r.dl_limit, r.ul_limit)?
    {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    st.core.store.audit(
        &actor(&user),
        "node_limits_changed",
        &format!("{id}|{}|{}", r.dl_limit, r.ul_limit),
    );
    let applied = st.core.apply_limits(&id).await;
    st.core.bump_live();
    Ok(Json(json!({ "ok": true, "applied": applied })))
}

#[derive(Deserialize, Default)]
struct StatsRange {
    range: Option<String>,
}

/// Comparaison des nœuds sur une période : débits moyens et maximaux, volumes estimés, disponibilité.
async fn node_stats(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Query(q): Query<StatsRange>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let secs: i64 = match q.range.as_deref() {
        Some("1h") => 3600,
        Some("7d") => 7 * 24 * 3600,
        Some("30d") => 30 * 24 * 3600,
        _ => 24 * 3600,
    };
    let step = i64::try_from(st.core.metrics_interval().as_secs()).unwrap_or(60);
    let sums = st.core.store.node_summaries(unix_now() - secs, step)?;
    let names: std::collections::HashMap<String, String> = st
        .core
        .store
        .node_details()?
        .into_iter()
        .map(|n| {
            let label = n.name.filter(|x| !x.is_empty()).unwrap_or(n.hostname);
            (n.id, label)
        })
        .collect();
    let rows: Vec<(String, f64, i64)> = sums
        .iter()
        .map(|s| {
            (
                s.node_id.clone(),
                s.up_bytes,
                st.core.telemetry_torrent_count(&s.node_id) as i64,
            )
        })
        .collect();
    let weak = crate::health::underperformers(&rows);
    let total_up: f64 = sums.iter().map(|s| s.up_bytes).sum();
    let nodes: Vec<serde_json::Value> = sums
        .iter()
        .filter(|s| names.contains_key(&s.node_id))
        .map(|s| {
            json!({
                "node_id": s.node_id,
                "name": names[&s.node_id],
                "availability": if s.samples > 0 { s.online_samples as f64 / s.samples as f64 } else { 0.0 },
                "avg_dl": s.avg_dl, "avg_ul": s.avg_ul, "max_dl": s.max_dl, "max_ul": s.max_ul,
                "avg_peers": s.avg_peers,
                "up_bytes": s.up_bytes, "down_bytes": s.down_bytes,
                "up_share": if total_up > 0.0 { s.up_bytes / total_up } else { 0.0 },
                "underperforming": weak.contains(&s.node_id),
            })
        })
        .collect();
    Ok(Json(json!({ "range_secs": secs, "nodes": nodes })))
}

#[derive(Deserialize)]
struct LabelsReq {
    labels: Vec<String>,
}

const MAX_LABELS: usize = 20;

/// Remplace les labels d'un nœud. Seul l'admin les fixe (jamais le nœud) ; le scheduler s'en sert aussitôt :
/// les torrents « en attente » qui conviennent désormais à ce nœud lui sont assignés.
async fn set_labels(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<LabelsReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    let mut labels: Vec<String> = Vec::new();
    for l in r.labels.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if !crate::manage::valid_label(l) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "label invalide (lettres, chiffres, - _ . ; 64 caractères maximum)",
            ));
        }
        if !labels.iter().any(|x| x == l) {
            labels.push(l.to_owned());
        }
    }
    if labels.len() > MAX_LABELS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{MAX_LABELS} labels au maximum par nœud"),
        ));
    }
    if proto::parse_node_san(&format!("node-{id}")).is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"));
    }
    let old = st
        .core
        .store
        .node_details()?
        .into_iter()
        .find(|n| n.id == id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "nœud inconnu"))?;
    st.core.store.set_node_labels(&id, &labels)?;
    st.core.store.audit(
        &actor(&user),
        "node_labels_changed",
        &format!("{id}|{}|{}", old.labels.join(","), labels.join(",")),
    );
    st.core.bump_live();
    // De nouveaux labels peuvent rendre ce nœud éligible à des torrents jusque-là en attente.
    st.core.schedule_pending().await;
    Ok(Json(json!({ "ok": true, "labels": labels })))
}

#[derive(Deserialize, Default)]
struct PairingReq {
    #[serde(default)]
    labels: Vec<String>,
    /// Nom d'affichage que recevra le nœud à l'appairage (modifiable ensuite).
    #[serde(default)]
    name: Option<String>,
}

async fn pairing_code(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<PairingReq>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    let code = st
        .core
        .create_pairing_code_named(
            &actor(&user),
            PAIRING_CODE_TTL,
            &r.labels,
            r.name.as_deref(),
        )
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    // Le code n'est montré qu'ici, une fois ; il n'est ni journalisé ni conservé en clair.
    Ok((
        StatusCode::CREATED,
        Json(json!({ "code": code, "expires_at": unix_now() + PAIRING_CODE_TTL.as_secs() as i64, "labels": r.labels, "name": r.name })),
    )
        .into_response())
}

// ---------- Torrents ----------

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    q: String,
    status: Option<String>,
    node: Option<String>,
    managed: Option<bool>,
    /// Hôte de tracker exact, ou `-` pour « tracker inconnu ».
    tracker: Option<String>,
    sort: Option<String>,
    /// `desc` ou `asc`.
    dir: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
}

async fn list_torrents(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<crate::overview::TorrentPage>> {
    authed(&st, &headers)?;
    let page = st.core.query_torrents(&TorrentQuery {
        q: q.q.chars().take(100).collect(),
        status: q.status.filter(|s| !s.is_empty()),
        node: q.node.filter(|s| !s.is_empty()),
        managed: q.managed,
        tracker: q.tracker.filter(|s| !s.is_empty()),
        sort: q.sort.unwrap_or_default(),
        desc: q.dir.as_deref() == Some("desc"),
        offset: q.offset,
        limit: q.limit,
    })?;
    Ok(Json(page))
}

#[derive(Deserialize)]
struct AddTorrent {
    magnet: Option<String>,
    infohash: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    save_subdir: String,
    #[serde(default)]
    requires: Vec<String>,
    replicate_group: Option<String>,
    #[serde(default)]
    size_hint: u64,
}

async fn add_torrent(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<AddTorrent>,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    let source = match (r.magnet, r.infohash) {
        (Some(m), None) => Source::Magnet(m),
        (None, Some(h)) => Source::Infohash(h),
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "fournir exactement un des champs `magnet` ou `infohash`",
            ))
        }
    };
    let ih = st
        .core
        .add_torrent_as(
            &actor(&user),
            NewTorrent {
                source,
                save_subdir: r.save_subdir,
                requires: r.requires,
                replicate_group: r.replicate_group,
                size_hint: r.size_hint,
                name: r.name,
            },
        )
        .await?;
    st.core.bump_live();
    Ok((StatusCode::CREATED, Json(json!({ "infohash": ih }))).into_response())
}

#[derive(Deserialize, Default)]
struct FileQuery {
    #[serde(default)]
    save_subdir: String,
    /// Labels séparés par des virgules.
    #[serde(default)]
    requires: String,
    replicate_group: Option<String>,
    #[serde(default)]
    name: String,
}

/// Envoi d'un fichier .torrent : le corps de la requête EST le fichier (octets bruts), les options passent
/// dans l'URL. Pas de multipart : une dépendance et une surface d'attaque en moins.
async fn add_torrent_file(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
    body: Bytes,
) -> ApiResult<Response> {
    let (user, _) = authed(&st, &headers)?;
    if body.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "fichier vide"));
    }
    let requires = q
        .requires
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let ih = st
        .core
        .add_torrent_as(
            &actor(&user),
            NewTorrent {
                source: Source::File(body.to_vec()),
                save_subdir: q.save_subdir,
                requires,
                replicate_group: q.replicate_group.filter(|g| !g.is_empty()),
                size_hint: 0,
                name: q.name,
            },
        )
        .await?;
    st.core.bump_live();
    Ok((StatusCode::CREATED, Json(json!({ "infohash": ih }))).into_response())
}

#[derive(Deserialize)]
struct BulkReq {
    /// `pause` | `resume` | `remove` | `placement`
    action: String,
    infohashes: Vec<String>,
    #[serde(default)]
    delete_files: bool,
    /// Pour `placement` : nouveaux labels requis et groupe de réplication (vide = aucun).
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    replicate_group: Option<String>,
}

const BULK_MAX: usize = 1000;

async fn bulk_torrents(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Json(r): Json<BulkReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    if r.infohashes.is_empty() || r.infohashes.len() > BULK_MAX {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("entre 1 et {BULK_MAX} torrents par action"),
        ));
    }
    if !matches!(
        r.action.as_str(),
        "pause" | "resume" | "remove" | "placement" | "recheck"
    ) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "action inconnue (pause, resume, remove, recheck ou placement)",
        ));
    }
    let who = actor(&user);
    let (mut done, mut failed) = (0usize, 0usize);
    for ih in &r.infohashes {
        let res = match r.action.as_str() {
            "pause" => st.core.group_set_paused_as(&who, ih, true).await,
            "resume" => st.core.group_set_paused_as(&who, ih, false).await,
            "recheck" => st.core.group_recheck_as(&who, ih).await.map(|_| ()),
            "placement" => {
                st.core
                    .set_torrent_placement_as(
                        &who,
                        ih,
                        r.requires.clone(),
                        r.replicate_group.clone(),
                    )
                    .await
            }
            _ => st.core.group_remove_as(&who, ih, r.delete_files).await,
        };
        if res.is_ok() {
            done += 1;
        } else {
            failed += 1;
        }
    }
    st.core.bump_live();
    Ok(Json(json!({ "done": done, "failed": failed })))
}
async fn pause_torrent(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(ih): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    st.core
        .group_set_paused_as(&actor(&user), &ih, true)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn resume_torrent(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(ih): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    st.core
        .group_set_paused_as(&actor(&user), &ih, false)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct RemoveQuery {
    #[serde(default)]
    delete_files: bool,
}

async fn remove_torrent(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Path(ih): Path<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = authed(&st, &headers)?;
    st.core
        .group_remove_as(&actor(&user), &ih, q.delete_files)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- Audit ----------

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
    /// Pagination : entrées d'identifiant strictement inférieur.
    before: Option<i64>,
    /// Catégorie : `security`, `access`, `nodes`, `torrents`, `system`.
    category: Option<String>,
    /// Gravité minimale : `warning` ou `critical`.
    severity: Option<String>,
    /// Texte libre (dans la phrase ou le détail).
    #[serde(default)]
    q: String,
    /// Action brute exacte (ex. `torrent_added`).
    action: Option<String>,
}

#[derive(Serialize)]
struct AuditView {
    id: i64,
    ts: i64,
    #[serde(flatten)]
    what: describe::Described,
    /// Action et auteur bruts (pour les exports et le débogage).
    action: String,
    actor: String,
}

#[derive(Serialize)]
struct AuditPage {
    items: Vec<AuditView>,
    /// À repasser dans `before` pour la page suivante ; absent quand il n'y a plus rien.
    next_before: Option<i64>,
}

fn severity_rank(s: describe::Severity) -> u8 {
    match s {
        describe::Severity::Info => 0,
        describe::Severity::Warning => 1,
        describe::Severity::Critical => 2,
    }
}

async fn audit(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<AuditPage>> {
    authed(&st, &headers)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let min = match q.severity.as_deref() {
        Some("critical") => 2,
        Some("warning") => 1,
        _ => 0,
    };
    let needle = q.q.trim().to_lowercase();
    let names = Names {
        nodes: st
            .core
            .store
            .node_details()?
            .into_iter()
            .map(|n| {
                let label = n.name.filter(|x| !x.is_empty()).unwrap_or(n.hostname);
                (n.id, label)
            })
            .collect(),
        torrents: st
            .core
            .store
            .list_torrents()?
            .into_iter()
            .map(|t| (t.infohash, t.name))
            .collect(),
    };
    // Le filtre porte sur la version « humaine » : on lit le journal par lots jusqu'à remplir la page.
    let mut items = Vec::new();
    let mut cursor = q.before;
    let mut exhausted = false;
    'scan: for _ in 0..20 {
        let rows = st.core.store.audit_list(500, cursor, None)?;
        if rows.is_empty() {
            exhausted = true;
            break;
        }
        for r in rows {
            cursor = Some(r.id);
            let what = describe::describe(&r, &names);
            let keep = severity_rank(what.severity) >= min
                && q.action.as_deref().is_none_or(|a| a == r.action)
                && q.category
                    .as_deref()
                    .is_none_or(|c| c.is_empty() || what.category == c)
                && (needle.is_empty()
                    || what.title.to_lowercase().contains(&needle)
                    || what.detail.to_lowercase().contains(&needle)
                    || what.actor_label.to_lowercase().contains(&needle));
            if keep {
                items.push(AuditView {
                    id: r.id,
                    ts: r.ts,
                    what,
                    action: r.action,
                    actor: r.actor,
                });
                if items.len() >= limit {
                    break 'scan;
                }
            }
        }
    }
    let next_before = if exhausted { None } else { cursor };
    Ok(Json(AuditPage { items, next_before }))
}

// ---------- Tableau de bord : KPIs, historique, temps réel ----------

async fn stats(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> ApiResult<Json<crate::overview::ClusterStats>> {
    authed(&st, &headers)?;
    Ok(Json(st.core.cluster_stats()?))
}

#[derive(Deserialize, Default)]
struct MetricsQuery {
    /// `1h`, `6h`, `24h`, `7d`, `30d`
    range: Option<String>,
}

async fn metrics(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    authed(&st, &headers)?;
    let secs: i64 = match q.range.as_deref() {
        Some("6h") => 6 * 3600,
        Some("24h") => 24 * 3600,
        Some("7d") => 7 * 24 * 3600,
        Some("30d") => 30 * 24 * 3600,
        _ => 3600,
    };
    // ~240 points au plus quelle que soit la période.
    let bucket =
        (secs / 240).max(i64::try_from(st.core.metrics_interval().as_secs()).unwrap_or(60));
    let pts: Vec<serde_json::Value> = st
        .core
        .store
        .metrics_since(unix_now() - secs, bucket)?
        .into_iter()
        .map(|m| {
            json!({
                "ts": m.ts,
                "nodes_online": m.nodes_online,
                "nodes_total": m.nodes_total,
                "torrents_total": m.torrents_total,
                "torrents_seeding": m.torrents_seeding,
                "torrents_downloading": m.torrents_downloading,
                "dl_rate": m.dl_rate,
                "ul_rate": m.ul_rate,
                "disk_free": m.disk_free,
            })
        })
        .collect();
    Ok(Json(json!({ "bucket_secs": bucket, "points": pts })))
}

/// Canal temps réel (WebSocket, serveur -> navigateur uniquement). Il ne transporte que des instantanés
/// agrégés (KPIs + nœuds) et un numéro de révision ; le navigateur recharge la page de torrents visible
/// quand la révision change. Protégé comme le reste : session valide + origine identique (sinon un site
/// tiers pourrait ouvrir ce canal avec le cookie de l'admin).
async fn live(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let (_, token) = authed(&st, &headers)?;
    let origin_ok = match (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        (Some(o), Some(h)) => o
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://").map(|(_, rest)| rest))
            .is_some_and(|o| Some(o) == h.to_str().ok()),
        (Some(_), None) => false,
        (None, _) => true,
    };
    if !origin_ok {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "origine refusée"));
    }
    Ok(ws
        .max_message_size(1024)
        .max_frame_size(1024)
        .on_upgrade(move |sock| live_loop(st, token, sock)))
}

async fn live_loop(st: Arc<AdminState>, token: String, mut sock: WebSocket) {
    let mut rx = st.core.subscribe_live();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut first = true;
    let mut quiet_ticks = 0u32;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if !st.sessions.is_valid(&token) {
                    let _ = sock.send(Message::Text(r#"{"type":"expired"}"#.into())).await;
                    break;
                }
                let changed = rx.has_changed().unwrap_or(false);
                quiet_ticks += 1;
                // Sans changement on envoie quand même un battement toutes les 10 s (garde la connexion vivante).
                if !(first || changed || quiet_ticks >= 10) {
                    continue;
                }
                first = false;
                quiet_ticks = 0;
                let rev = *rx.borrow_and_update();
                let (Ok(stats), Ok(nodes)) = (st.core.cluster_stats(), node_views(&st)) else {
                    continue;
                };
                let msg = json!({ "type": "tick", "rev": rev, "stats": stats, "nodes": nodes });
                if sock.send(Message::Text(msg.to_string().into())).await.is_err() {
                    break;
                }
            }
            m = sock.recv() => match m {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                _ => {} // rien n'est attendu du navigateur ; on ignore
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_account_creation_is_local_only_unless_explicitly_allowed() {
        let strict = AdminSettings::default();
        let open = AdminSettings {
            allow_remote_setup: true,
            ..AdminSettings::default()
        };
        for local in ["127.0.0.1", "::1", "127.0.0.2"] {
            assert!(setup_allowed(local.parse().unwrap(), &strict), "{local}");
        }
        for remote in ["172.18.0.1", "192.168.1.20", "203.0.113.9", "2001:db8::1"] {
            let ip: IpAddr = remote.parse().unwrap();
            assert!(
                !setup_allowed(ip, &strict),
                "{remote} doit être refusé par défaut"
            );
            assert!(setup_allowed(ip, &open), "{remote} autorisé avec l'option");
        }
    }
}
