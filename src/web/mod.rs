//! Configuration web UI: server-rendered pages (`askama`), plain forms, and `htmx` only to
//! refresh the overview and to suggest a device id. Every asset is embedded in the binary.
//!
//! The UI edits the [`Store`] and tells the running bridge about every change through a channel
//! of [`ConfigChange`] messages; what it shows about the running bridge and how it pairs a device
//! comes through the [`Backend`] it is given. It never shows a stored secret: password and token
//! fields are write-only.
//!
//! Every page but the login and the first-start page needs a session (see [`auth`]).

mod auth;
mod pages;

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, LOCATION, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::model::{BridgeStatus, ConfigChange, PairError};
use crate::secret::Secret;
use crate::store::{Store, StoreError};
pub use auth::AuthTiming;
use auth::{Sessions, Throttle};

/// A future the [`Backend`] returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the UI needs from the rest of the service besides the store.
pub trait Backend: Send + Sync + 'static {
    /// What the running bridge does right now.
    fn status(&self) -> BridgeStatus;

    /// Requests a new auth token from the device at `host:port` (pairing mode).
    fn pair<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<Secret<String>, PairError>>;
}

/// State shared by every request. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Shared>,
}

struct Shared {
    store: Store,
    changes: mpsc::Sender<ConfigChange>,
    backend: Arc<dyn Backend>,
    timing: AuthTiming,
    sessions: Mutex<Sessions>,
    throttle: Mutex<Throttle>,
    /// Once set, the admin password never goes away again; until then the store is asked.
    password_set: AtomicBool,
    /// Held from a store write until the matching change is sent, so the bridge receives the
    /// changes in the order the store saw them.
    config: tokio::sync::Mutex<()>,
}

impl AppState {
    /// State for the UI over `store`, telling the bridge about changes through `changes`.
    pub fn new(
        store: Store,
        changes: mpsc::Sender<ConfigChange>,
        backend: Arc<dyn Backend>,
        timing: AuthTiming,
    ) -> Self {
        Self {
            inner: Arc::new(Shared {
                store,
                changes,
                backend,
                timing,
                sessions: Mutex::new(Sessions::default()),
                throttle: Mutex::new(Throttle::default()),
                password_set: AtomicBool::new(false),
                config: tokio::sync::Mutex::new(()),
            }),
        }
    }

    fn store(&self) -> &Store {
        &self.inner.store
    }

    fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn throttle(&self) -> MutexGuard<'_, Throttle> {
        self.inner
            .throttle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    async fn password_set(&self) -> Result<bool, StoreError> {
        if self.inner.password_set.load(Ordering::SeqCst) {
            return Ok(true);
        }
        let set = self.store().admin_password_hash().await?.is_some();
        if set {
            self.inner.password_set.store(true, Ordering::SeqCst);
        }
        Ok(set)
    }

    /// Hands a change to the bridge. A stopped bridge (service shutting down) is only logged.
    async fn notify(&self, change: ConfigChange) {
        if self.inner.changes.send(change).await.is_err() {
            tracing::warn!("the bridge is not running, the change takes effect at the next start");
        }
    }
}

/// The UI's routes with authentication, same-origin check and security headers.
pub fn router(state: AppState) -> Router {
    pages::routes()
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(state)
}

/// Serves the UI on `bind` until `shutdown` resolves.
pub async fn serve(
    bind: SocketAddr,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    let bound = listener.local_addr()?;
    if !bound.ip().is_loopback() {
        tracing::warn!(
            address = %bound,
            "the configuration UI is reachable from the network without TLS; put a TLS proxy in front"
        );
    }
    tracing::info!(address = %bound, "configuration UI listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

/// The session a request was authenticated with.
#[derive(Clone)]
struct SessionId(String);

/// Paths reachable without a session.
fn public(path: &str) -> bool {
    path.starts_with("/assets/") || path == "/login" || path == "/setup"
}

/// Access rules, in this order: assets are public; a POST from another origin is refused; until
/// an admin password exists only the first-start page is served; then everything but the login
/// needs an open session.
async fn guard(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    if path.starts_with("/assets/") {
        return next.run(request).await;
    }
    if request.method() != Method::GET
        && request.method() != Method::HEAD
        && !auth::same_origin(request.headers())
    {
        tracing::warn!(path, "request from another origin refused");
        return (
            StatusCode::FORBIDDEN,
            "Request from another origin refused.",
        )
            .into_response();
    }
    let password_set = match state.password_set().await {
        Ok(set) => set,
        Err(error) => return pages::internal_error(&error),
    };
    if !password_set {
        return if path == "/setup" {
            next.run(request).await
        } else {
            redirect_for(&request, "/setup")
        };
    }
    if path == "/setup" {
        return redirect_for(&request, "/login");
    }
    let session = auth::session_cookie(request.headers())
        .filter(|id| state.sessions().touch(id, &state.inner.timing));
    match session {
        Some(id) => {
            request.extensions_mut().insert(SessionId(id));
            next.run(request).await
        }
        None if public(&path) => next.run(request).await,
        None => redirect_for(&request, "/login"),
    }
}

/// A redirect; for an htmx request one htmx follows with a full page load.
fn redirect_for(request: &Request, to: &'static str) -> Response {
    if request.headers().contains_key("hx-request") {
        let mut response = StatusCode::OK.into_response();
        response
            .headers_mut()
            .insert("hx-redirect", HeaderValue::from_static(to));
        return response;
    }
    see_other(to)
}

/// `303 See Other` to `to`.
fn see_other(to: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    match HeaderValue::from_str(to) {
        Ok(value) => {
            response.headers_mut().insert(LOCATION, value);
            response
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Headers on every answer: nothing is cached, framed or loaded from elsewhere.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
    );
    response
}
