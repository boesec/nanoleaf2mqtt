//! Test support for the web UI tests (included with `#[path]`): the router over a fresh store,
//! driven in-process, with a cookie jar of one session cookie and a backend whose status and
//! pairing answer the test controls.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    missing_docs,
    unreachable_pub
)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, HOST, LOCATION, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use nanoleaf2mqtt::model::{BridgeStatus, ConfigChange, PairError};
use nanoleaf2mqtt::secret::Secret;
use nanoleaf2mqtt::store::Store;
use nanoleaf2mqtt::web::{AppState, AuthTiming, Backend, BoxFuture, router};
use tokio::sync::mpsc;
use tower::ServiceExt;

pub const HOST_NAME: &str = "ui.test:8080";
pub const ADMIN_PASSWORD: &str = "admin-password-5d1e";

/// What the fake backend answers to a pairing request.
#[derive(Debug, Clone)]
pub enum PairAnswer {
    Token(String),
    Refused,
    Failed,
}

/// A backend with a settable status and pairing answer.
pub struct FakeBackend {
    pub status: Mutex<BridgeStatus>,
    pub pair: Mutex<PairAnswer>,
    pub pair_requests: Mutex<Vec<(String, u16)>>,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            status: Mutex::new(BridgeStatus::default()),
            pair: Mutex::new(PairAnswer::Refused),
            pair_requests: Mutex::new(Vec::new()),
        }
    }
}

impl Backend for FakeBackend {
    fn status(&self) -> BridgeStatus {
        self.status.lock().unwrap().clone()
    }

    fn pair<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<Secret<String>, PairError>> {
        self.pair_requests
            .lock()
            .unwrap()
            .push((host.to_owned(), port));
        let answer = self.pair.lock().unwrap().clone();
        Box::pin(async move {
            match answer {
                PairAnswer::Token(token) => Ok(Secret::new(token)),
                PairAnswer::Refused => Err(PairError::NotInPairingMode),
                PairAnswer::Failed => Err(PairError::Failed("connection refused".to_owned())),
            }
        })
    }
}

/// An answer of the UI.
#[derive(Debug)]
pub struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Answer {
    pub fn location(&self) -> Option<&str> {
        self.headers.get(LOCATION).and_then(|v| v.to_str().ok())
    }

    /// Asserts a `303 See Other` to `to` and returns self.
    #[track_caller]
    pub fn assert_redirect(&self, to: &str) -> &Self {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.body);
        assert_eq!(self.location(), Some(to));
        self
    }

    /// Everything a browser could see: status line, headers and body.
    pub fn everything(&self) -> String {
        let headers: Vec<String> = self
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {}", String::from_utf8_lossy(v.as_bytes())))
            .collect();
        format!("{}\n{}\n{}", self.status, headers.join("\n"), self.body)
    }
}

/// The UI over a fresh store in a temporary directory.
pub struct Ui {
    pub router: Router,
    pub store: Store,
    pub backend: Arc<FakeBackend>,
    pub changes: mpsc::Receiver<ConfigChange>,
    /// The session cookie (`name=value`), once one was set.
    pub cookie: Option<String>,
    _dir: tempfile::TempDir,
}

impl Ui {
    pub async fn new() -> Self {
        Self::with_timing(AuthTiming::default()).await
    }

    pub async fn with_timing(timing: AuthTiming) -> Self {
        Self::build(timing, None).await
    }

    /// The UI with another backend; `backend` of the result is then unused.
    pub async fn with_backend(backend: Arc<dyn Backend>) -> Self {
        Self::build(AuthTiming::default(), Some(backend)).await
    }

    async fn build(timing: AuthTiming, other: Option<Arc<dyn Backend>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let backend = Arc::new(FakeBackend::default());
        let (tx, changes) = mpsc::channel(64);
        let used: Arc<dyn Backend> = other.unwrap_or_else(|| backend.clone());
        let state = AppState::new(store.clone(), tx, used, timing);
        Self {
            router: router(state),
            store,
            backend,
            changes,
            cookie: None,
            _dir: dir,
        }
    }

    /// A first start already done and logged in.
    pub async fn signed_in() -> Self {
        let mut ui = Self::new().await;
        ui.setup(ADMIN_PASSWORD).await;
        ui
    }

    async fn send(&mut self, request: Request<Body>) -> Answer {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        if let Some(cookie) = headers.get(SET_COOKIE).and_then(|v| v.to_str().ok()) {
            let pair = cookie.split(';').next().unwrap().trim().to_owned();
            self.cookie = if pair.ends_with('=') {
                None
            } else {
                Some(pair)
            };
        }
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        Answer {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    fn request(&self, method: &str, path: &str) -> axum::http::request::Builder {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, HOST_NAME);
        if let Some(cookie) = &self.cookie {
            builder = builder.header(COOKIE, cookie);
        }
        builder
    }

    pub async fn get(&mut self, path: &str) -> Answer {
        let request = self.request("GET", path).body(Body::empty()).unwrap();
        self.send(request).await
    }

    /// A GET as htmx sends it.
    pub async fn get_htmx(&mut self, path: &str) -> Answer {
        let request = self
            .request("GET", path)
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap();
        self.send(request).await
    }

    /// A form POST from a page of the UI (same origin).
    pub async fn post(&mut self, path: &str, form: &[(&str, &str)]) -> Answer {
        self.post_from(path, form, Some(&format!("http://{HOST_NAME}")))
            .await
    }

    /// A form POST with the given `Origin` (or none).
    pub async fn post_from(
        &mut self,
        path: &str,
        form: &[(&str, &str)],
        origin: Option<&str>,
    ) -> Answer {
        let mut builder = self
            .request("POST", path)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(origin) = origin {
            builder = builder.header(ORIGIN, origin);
        }
        let request = builder.body(Body::from(encode(form))).unwrap();
        self.send(request).await
    }

    /// Completes the first start with `password`; leaves the session open.
    pub async fn setup(&mut self, password: &str) {
        self.post("/setup", &[("password", password), ("confirm", password)])
            .await
            .assert_redirect("/");
        assert!(self.cookie.is_some());
    }

    pub async fn login(&mut self, password: &str) -> Answer {
        self.post("/login", &[("password", password)]).await
    }

    /// The configuration changes sent so far.
    pub fn take_changes(&mut self) -> Vec<ConfigChange> {
        let mut out = Vec::new();
        while let Ok(change) = self.changes.try_recv() {
            out.push(change);
        }
        out
    }
}

/// `application/x-www-form-urlencoded`.
pub fn encode(form: &[(&str, &str)]) -> String {
    form.iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[test]
fn form_encoding() {
    assert_eq!(
        encode(&[("a b", "x&y=z/ä"), ("n", "")]),
        "a+b=x%26y%3Dz%2F%C3%A4&n="
    );
}
