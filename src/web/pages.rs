//! Pages and form handlers. Every action is a plain form POST answered with a redirect (or the
//! page again with an error), so the UI works without JavaScript.
//!
//! Templates never receive a secret: the view structs below have no field that could hold one,
//! only whether one is set.

// Helpers return the finished error page as `Err(Response)`; it is handed straight back to axum,
// so boxing it would only add an allocation.
#![allow(clippy::result_large_err)]

use askama::Template;
use axum::extract::{Extension, Path, Query, State};
use axum::http::header::{CONTENT_TYPE, SET_COOKIE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

use super::auth::{self, MIN_PASSWORD_LEN};
use super::{AppState, SessionId, see_other};
use crate::model::{
    BridgeStatus, ConfigChange, DEFAULT_DEVICE_PORT, DeviceId, PairError, parse_broker_url,
};
use crate::secret::Secret;
use crate::store::{
    BrokerUpdate, DeviceUpdate, NewDevice, SecretUpdate, Settings, StoreError, StoredDevice,
};

const STYLE: &str = include_str!("../../assets/style.css");
/// htmx 2.0.4 (<https://htmx.org>, Zero-Clause BSD licence), unmodified.
const HTMX: &str = include_str!("../../assets/htmx.min.js");

/// The text shown when the device refuses pairing.
const PAIR_HINT: &str = "The device is not in pairing mode. Hold its power button for 5–7 \
                         seconds until the lights flash, then click “Pair” within 30 seconds.";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/assets/style.css", get(style))
        .route("/assets/htmx.min.js", get(htmx))
        .route("/setup", get(setup_page).post(setup))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/", get(overview))
        .route("/fragments/overview", get(overview_fragment))
        .route("/broker", get(broker_page).post(broker_save))
        .route("/devices", get(devices_page).post(device_create))
        .route("/devices/new", get(device_new_page))
        .route("/devices/suggest-id", get(suggest_id))
        .route("/devices/{id}", get(device_page).post(device_save))
        .route("/devices/{id}/enabled", post(device_enabled))
        .route("/devices/{id}/pair", post(device_pair))
        .route(
            "/devices/{id}/remove",
            get(device_remove_page).post(device_remove),
        )
        .route("/settings", get(settings_page))
        .route("/settings/discovery", post(discovery_save))
        .route("/settings/password", post(password_change))
}

// ----- rendering --------------------------------------------------------------------------

/// What every page needs.
struct Chrome {
    version: &'static str,
    /// Which navigation entry is current; empty on pages outside the navigation.
    active: &'static str,
    /// Whether the navigation (and logout) is shown.
    signed_in: bool,
    /// An error to show at the top; empty for none.
    error: String,
    /// A confirmation to show at the top; empty for none.
    notice: String,
}

impl Chrome {
    fn new(active: &'static str) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            active,
            signed_in: true,
            error: String::new(),
            notice: String::new(),
        }
    }

    fn signed_out() -> Self {
        Self {
            signed_in: false,
            ..Self::new("")
        }
    }

    fn error(mut self, error: impl Into<String>) -> Self {
        self.error = error.into();
        self
    }

    fn notice(mut self, notice: Option<&str>) -> Self {
        let text = match notice {
            Some("saved") => "Saved.",
            Some("added") => "Device added.",
            Some("removed") => "Device removed.",
            Some("paired") => "Paired: the device's token is stored.",
            Some("enabled") => "Device enabled.",
            Some("disabled") => "Device disabled.",
            Some("password") => "Password changed. Every other session was logged out.",
            _ => "",
        };
        self.notice = text.to_owned();
        self
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct NoticeQuery {
    notice: Option<String>,
}

fn render<T: Template>(status: StatusCode, template: &T) -> Response {
    match template.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(error) => {
            tracing::error!(%error, "a page failed to render");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<h1>Something went wrong</h1><p>The page could not be rendered.</p>"),
            )
                .into_response()
        }
    }
}

fn page<T: Template>(template: &T) -> Response {
    render(StatusCode::OK, template)
}

/// Answers 500 without showing the cause, which is logged.
pub(super) fn internal_error(error: &StoreError) -> Response {
    tracing::error!(%error, "configuration store failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Html("<h1>Something went wrong</h1><p>The configuration store failed; see the log.</p>"),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Html("<h1>Not found</h1><p><a href=\"/devices\">Back to the devices</a></p>"),
    )
        .into_response()
}

/// Turns a store error into a message for the form, or a 500.
fn form_error(error: StoreError) -> Result<String, Response> {
    match error {
        StoreError::Invalid(e) => Ok(e.to_string()),
        StoreError::InvalidValue(text) => Ok(text.to_owned()),
        StoreError::DuplicateDevice(id) => {
            Ok(format!("A device with the id `{id}` exists already."))
        }
        other => Err(internal_error(&other)),
    }
}

/// A form field: blank means not given.
fn given(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn parse_port(value: &str) -> Result<u16, String> {
    match value.trim().parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err("The port must be a number 1-65535.".to_owned()),
    }
}

// ----- assets -----------------------------------------------------------------------------

async fn style() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

async fn htmx() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/javascript; charset=utf-8")], HTMX)
}

// ----- first start, login, logout ---------------------------------------------------------

#[derive(Template)]
#[template(path = "setup.html")]
struct SetupTemplate {
    chrome: Chrome,
    min_len: usize,
}

#[derive(Deserialize)]
pub(super) struct NewPasswordForm {
    password: String,
    confirm: String,
}

async fn setup_page() -> Response {
    page(&SetupTemplate {
        chrome: Chrome::signed_out(),
        min_len: MIN_PASSWORD_LEN,
    })
}

/// Checks a new password and its confirmation.
fn new_password(password: String, confirm: &str) -> Result<Secret<String>, &'static str> {
    if password != confirm {
        return Err("The two passwords differ.");
    }
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err("The password needs at least 8 characters.");
    }
    Ok(Secret::new(password))
}

async fn setup(State(state): State<AppState>, Form(form): Form<NewPasswordForm>) -> Response {
    let NewPasswordForm { password, confirm } = form;
    let password = match new_password(password, &confirm) {
        Ok(password) => password,
        Err(error) => {
            return render(
                StatusCode::UNPROCESSABLE_ENTITY,
                &SetupTemplate {
                    chrome: Chrome::signed_out().error(error),
                    min_len: MIN_PASSWORD_LEN,
                },
            );
        }
    };
    let hash = match auth::hash_password(password).await {
        Ok(hash) => hash,
        Err(error) => {
            tracing::error!(%error, "cannot hash the admin password");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    match state.store().set_initial_admin_password_hash(&hash).await {
        // Someone else was faster: their password stands.
        Ok(false) => see_other("/login"),
        Ok(true) => {
            tracing::info!("admin password set");
            signed_in(&state, "/")
        }
        Err(error) => internal_error(&error),
    }
}

/// Opens a session and redirects to `to` with its cookie.
fn signed_in(state: &AppState, to: &str) -> Response {
    let id = state.sessions().create(&state.inner.timing);
    let mut response = see_other(to);
    match HeaderValue::from_str(&auth::session_set_cookie(&id)) {
        Ok(cookie) => {
            response.headers_mut().insert(SET_COOKIE, cookie);
            response
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    chrome: Chrome,
}

#[derive(Deserialize)]
pub(super) struct LoginForm {
    password: String,
}

async fn login_page(session: Option<Extension<SessionId>>) -> Response {
    if session.is_some() {
        return see_other("/");
    }
    page(&LoginTemplate {
        chrome: Chrome::signed_out(),
    })
}

/// Checks a password against the stored hash, with the login throttle: `Err` is the answer to
/// give (locked out, wrong password, or failure).
async fn check_password(state: &AppState, password: String) -> Result<(), (StatusCode, String)> {
    if let Err(remaining) = state.throttle().check() {
        let seconds = remaining.as_secs().max(1);
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many failed attempts. Try again in {seconds} s."),
        ));
    }
    let internal = || {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The password could not be checked; see the log.".to_owned(),
        )
    };
    let hash = match state.store().admin_password_hash().await {
        Ok(Some(hash)) => hash,
        Ok(None) => return Err(internal()),
        Err(error) => {
            tracing::error!(%error, "cannot read the admin password hash");
            return Err(internal());
        }
    };
    match auth::verify_password(Secret::new(password), hash).await {
        Ok(true) => {
            state.throttle().succeeded();
            Ok(())
        }
        Ok(false) => {
            state.throttle().failed(&state.inner.timing);
            tracing::warn!("wrong admin password");
            tokio::time::sleep(state.inner.timing.failure_delay).await;
            Err((StatusCode::UNAUTHORIZED, "Wrong password.".to_owned()))
        }
        Err(error) => {
            tracing::error!(%error, "cannot check the admin password");
            Err(internal())
        }
    }
}

async fn login(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    match check_password(&state, form.password).await {
        Ok(()) => signed_in(&state, "/"),
        Err((status, message)) => render(
            status,
            &LoginTemplate {
                chrome: Chrome::signed_out().error(message),
            },
        ),
    }
}

async fn logout(State(state): State<AppState>, session: Option<Extension<SessionId>>) -> Response {
    if let Some(Extension(SessionId(id))) = session {
        state.sessions().remove(&id);
    }
    let mut response = see_other("/login");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&auth::session_clear_cookie())
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    response
}

// ----- overview ---------------------------------------------------------------------------

/// One device as the overview and the device list show it.
struct DeviceRow {
    id: String,
    name: String,
    address: String,
    enabled: bool,
    token_set: bool,
    /// Short state text and its colour class.
    state: &'static str,
    tone: &'static str,
    /// Last published values (path, JSON text), without the effect list.
    values: Vec<(String, String)>,
}

impl DeviceRow {
    fn new(device: &StoredDevice, status: &BridgeStatus) -> Self {
        let running = status.device(&device.id);
        let (state, tone) = if !device.enabled {
            ("disabled", "muted")
        } else if device.token.is_none() {
            ("not paired", "warn")
        } else {
            match running.map(|r| r.reachable) {
                None => ("starting", "muted"),
                Some(None) => ("waiting", "muted"),
                Some(Some(true)) => ("reachable", "good"),
                Some(Some(false)) => ("unreachable", "bad"),
            }
        };
        Self {
            id: device.id.as_str().to_owned(),
            name: device.name.clone(),
            address: format!("{}:{}", device.host, device.port),
            enabled: device.enabled,
            token_set: device.token.is_some(),
            state,
            tone,
            values: running
                .map(|r| {
                    r.values
                        .iter()
                        .filter(|(path, _)| path != "effects")
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

struct OverviewView {
    broker_configured: bool,
    broker_target: String,
    prefix: String,
    broker_connected: bool,
    ha_discovery: bool,
    devices: Vec<DeviceRow>,
}

#[derive(Template)]
#[template(path = "overview.html")]
struct OverviewTemplate {
    chrome: Chrome,
    view: OverviewView,
}

#[derive(Template)]
#[template(path = "overview_fragment.html")]
struct OverviewFragmentTemplate {
    view: OverviewView,
}

fn broker_target(settings: &Settings) -> String {
    settings
        .broker_url
        .as_deref()
        .and_then(|url| parse_broker_url(url).ok())
        .map(|(host, port)| format!("{host}:{port}"))
        .unwrap_or_default()
}

async fn overview_view(state: &AppState) -> Result<OverviewView, StoreError> {
    let settings = state.store().settings().await?;
    let devices = state.store().devices().await?;
    let status = state.inner.backend.status();
    Ok(OverviewView {
        broker_configured: settings.broker_url.is_some(),
        broker_target: broker_target(&settings),
        prefix: settings.topic_prefix.clone(),
        broker_connected: status.broker_connected,
        ha_discovery: settings.ha_discovery,
        devices: devices.iter().map(|d| DeviceRow::new(d, &status)).collect(),
    })
}

async fn overview(State(state): State<AppState>) -> Response {
    match overview_view(&state).await {
        Ok(view) => page(&OverviewTemplate {
            chrome: Chrome::new("overview"),
            view,
        }),
        Err(error) => internal_error(&error),
    }
}

async fn overview_fragment(State(state): State<AppState>) -> Response {
    match overview_view(&state).await {
        Ok(view) => page(&OverviewFragmentTemplate { view }),
        Err(error) => internal_error(&error),
    }
}

// ----- broker -----------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "broker.html")]
struct BrokerTemplate {
    chrome: Chrome,
    url: String,
    username: String,
    password_set: bool,
    prefix: String,
    connected: bool,
}

#[derive(Deserialize)]
pub(super) struct BrokerForm {
    url: String,
    username: String,
    password: String,
    clear_password: Option<String>,
    prefix: String,
}

async fn broker_page(State(state): State<AppState>, Query(query): Query<NoticeQuery>) -> Response {
    let settings = match state.store().settings().await {
        Ok(settings) => settings,
        Err(error) => return internal_error(&error),
    };
    page(&BrokerTemplate {
        chrome: Chrome::new("broker").notice(query.notice.as_deref()),
        url: settings.broker_url.clone().unwrap_or_default(),
        username: settings.broker_username.clone().unwrap_or_default(),
        password_set: settings.broker_password.is_some(),
        prefix: settings.topic_prefix.clone(),
        connected: state.inner.backend.status().broker_connected,
    })
}

async fn broker_save(State(state): State<AppState>, Form(form): Form<BrokerForm>) -> Response {
    let BrokerForm {
        url,
        username,
        password,
        clear_password,
        prefix,
    } = form;
    let password = if clear_password.is_some() {
        SecretUpdate::Clear
    } else if password.is_empty() {
        SecretUpdate::Keep
    } else {
        SecretUpdate::Set(Secret::new(password))
    };
    let update = BrokerUpdate {
        url: given(&url).map(str::to_owned),
        username: given(&username).map(str::to_owned),
        password,
        prefix: prefix.trim().to_owned(),
    };
    let _order = state.inner.config.lock().await;
    let saved = async {
        state.store().save_broker(&update).await?;
        state.store().broker_config().await
    }
    .await;
    match saved {
        Ok(broker) => {
            state.notify(ConfigChange::Broker(broker)).await;
            see_other("/broker?notice=saved")
        }
        Err(error) => match form_error(error) {
            Ok(message) => {
                let password_set = match state.store().settings().await {
                    Ok(settings) => settings.broker_password.is_some(),
                    Err(error) => return internal_error(&error),
                };
                render(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &BrokerTemplate {
                        chrome: Chrome::new("broker").error(message),
                        url,
                        username,
                        password_set,
                        prefix,
                        connected: state.inner.backend.status().broker_connected,
                    },
                )
            }
            Err(response) => response,
        },
    }
}

// ----- devices ----------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "devices.html")]
struct DevicesTemplate {
    chrome: Chrome,
    devices: Vec<DeviceRow>,
}

async fn devices_page(State(state): State<AppState>, Query(query): Query<NoticeQuery>) -> Response {
    let devices = match state.store().devices().await {
        Ok(devices) => devices,
        Err(error) => return internal_error(&error),
    };
    let status = state.inner.backend.status();
    page(&DevicesTemplate {
        chrome: Chrome::new("devices").notice(query.notice.as_deref()),
        devices: devices.iter().map(|d| DeviceRow::new(d, &status)).collect(),
    })
}

#[derive(Template)]
#[template(path = "device_new.html")]
struct DeviceNewTemplate {
    chrome: Chrome,
    name: String,
    id: String,
    host: String,
    port: String,
}

#[derive(Deserialize)]
pub(super) struct DeviceNewForm {
    name: String,
    id: String,
    host: String,
    port: String,
    token: String,
}

async fn device_new_page() -> Response {
    page(&DeviceNewTemplate {
        chrome: Chrome::new("devices"),
        name: String::new(),
        id: String::new(),
        host: String::new(),
        port: DEFAULT_DEVICE_PORT.to_string(),
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct SuggestQuery {
    #[serde(default)]
    name: String,
}

/// The id suggested for a name, as plain text (only `a-z0-9-`, safe to insert).
async fn suggest_id(Query(query): Query<SuggestQuery>) -> Response {
    let id = DeviceId::suggest(&query.name)
        .map(|id| id.as_str().to_owned())
        .unwrap_or_default();
    ([(CONTENT_TYPE, "text/plain; charset=utf-8")], id).into_response()
}

async fn device_create(State(state): State<AppState>, Form(form): Form<DeviceNewForm>) -> Response {
    let DeviceNewForm {
        name,
        id,
        host,
        port,
        token,
    } = form;
    let checked = (|| {
        let id = match given(&id) {
            Some(id) => DeviceId::new(id).map_err(|e| e.to_string())?,
            None => DeviceId::suggest(&name).ok_or_else(|| {
                "Enter an id: the name gives none (use a-z, 0-9 and -).".to_owned()
            })?,
        };
        let port = parse_port(&port)?;
        Ok::<_, String>((id, port))
    })();
    let form_again = |chrome: Chrome| DeviceNewTemplate {
        chrome,
        name: name.clone(),
        id: id.clone(),
        host: host.clone(),
        port: port.clone(),
    };
    let (device_id, port_number) = match checked {
        Ok(checked) => checked,
        Err(message) => {
            return render(
                StatusCode::UNPROCESSABLE_ENTITY,
                &form_again(Chrome::new("devices").error(message)),
            );
        }
    };
    let new = NewDevice {
        id: device_id.clone(),
        name: name.trim().to_owned(),
        host: host.trim().to_owned(),
        port: port_number,
        token: given(&token).map(|t| Secret::new(t.to_owned())),
        enabled: true,
    };
    let _order = state.inner.config.lock().await;
    if let Err(error) = state.store().insert_device(&new).await {
        return match form_error(error) {
            Ok(message) => render(
                StatusCode::UNPROCESSABLE_ENTITY,
                &form_again(Chrome::new("devices").error(message)),
            ),
            Err(response) => response,
        };
    }
    tracing::info!(device = device_id.as_str(), "device added");
    notify_device(&state, &device_id).await;
    see_other(&format!("/devices/{device_id}?notice=added"))
}

/// Tells the bridge what to do with a device as it is stored now. Call with the config lock held.
async fn notify_device(state: &AppState, id: &DeviceId) {
    match state.store().device(id).await {
        Ok(Some(device)) => state.notify(device.change()).await,
        Ok(None) => {
            state
                .notify(ConfigChange::Device {
                    id: id.clone(),
                    run: None,
                })
                .await;
        }
        Err(error) => tracing::error!(%error, "cannot read the device back"),
    }
}

#[derive(Template)]
#[template(path = "device.html")]
struct DeviceTemplate {
    chrome: Chrome,
    row: DeviceRow,
    host: String,
    port: String,
}

fn device_id(raw: &str) -> Option<DeviceId> {
    DeviceId::new(raw).ok()
}

async fn load_device(state: &AppState, raw: &str) -> Result<StoredDevice, Response> {
    let Some(id) = device_id(raw) else {
        return Err(not_found());
    };
    match state.store().device(&id).await {
        Ok(Some(device)) => Ok(device),
        Ok(None) => Err(not_found()),
        Err(error) => Err(internal_error(&error)),
    }
}

fn device_template(state: &AppState, device: &StoredDevice, chrome: Chrome) -> DeviceTemplate {
    DeviceTemplate {
        chrome,
        row: DeviceRow::new(device, &state.inner.backend.status()),
        host: device.host.clone(),
        port: device.port.to_string(),
    }
}

async fn device_page(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Query(query): Query<NoticeQuery>,
) -> Response {
    match load_device(&state, &raw).await {
        Ok(device) => page(&device_template(
            &state,
            &device,
            Chrome::new("devices").notice(query.notice.as_deref()),
        )),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub(super) struct DeviceEditForm {
    name: String,
    host: String,
    port: String,
    token: String,
    clear_token: Option<String>,
}

async fn device_save(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Form(form): Form<DeviceEditForm>,
) -> Response {
    let DeviceEditForm {
        name,
        host,
        port,
        token,
        clear_token,
    } = form;
    let device = match load_device(&state, &raw).await {
        Ok(device) => device,
        Err(response) => return response,
    };
    let form_again = |message: String| {
        let mut template = device_template(&state, &device, Chrome::new("devices").error(message));
        template.row.name = name.clone();
        template.host = host.clone();
        template.port = port.clone();
        render(StatusCode::UNPROCESSABLE_ENTITY, &template)
    };
    let port_number = match parse_port(&port) {
        Ok(port) => port,
        Err(message) => return form_again(message),
    };
    let token = if clear_token.is_some() {
        SecretUpdate::Clear
    } else {
        match given(&token) {
            Some(token) => SecretUpdate::Set(Secret::new(token.to_owned())),
            None => SecretUpdate::Keep,
        }
    };
    let update = DeviceUpdate {
        name: name.trim().to_owned(),
        host: host.trim().to_owned(),
        port: port_number,
        token,
    };
    let _order = state.inner.config.lock().await;
    match state.store().update_device(&device.id, &update).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => {
            return match form_error(error) {
                Ok(message) => form_again(message),
                Err(response) => response,
            };
        }
    }
    notify_device(&state, &device.id).await;
    see_other(&format!("/devices/{}?notice=saved", device.id))
}

#[derive(Deserialize)]
pub(super) struct EnabledForm {
    enabled: String,
}

async fn device_enabled(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Form(form): Form<EnabledForm>,
) -> Response {
    let Some(id) = device_id(&raw) else {
        return not_found();
    };
    let enabled = form.enabled == "true";
    let _order = state.inner.config.lock().await;
    match state.store().set_device_enabled(&id, enabled).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => return internal_error(&error),
    }
    tracing::info!(device = id.as_str(), enabled, "device switched");
    notify_device(&state, &id).await;
    let notice = if enabled { "enabled" } else { "disabled" };
    see_other(&format!("/devices/{id}?notice={notice}"))
}

async fn device_pair(State(state): State<AppState>, Path(raw): Path<String>) -> Response {
    let device = match load_device(&state, &raw).await {
        Ok(device) => device,
        Err(response) => return response,
    };
    let token = match state.inner.backend.pair(&device.host, device.port).await {
        Ok(token) => token,
        Err(error) => {
            let (status, message) = match &error {
                PairError::NotInPairingMode => (StatusCode::CONFLICT, PAIR_HINT.to_owned()),
                PairError::Failed(_) => (StatusCode::BAD_GATEWAY, error.to_string()),
            };
            tracing::warn!(device = device.id.as_str(), %error, "pairing failed");
            return render(
                status,
                &device_template(&state, &device, Chrome::new("devices").error(message)),
            );
        }
    };
    let _order = state.inner.config.lock().await;
    match state.store().set_device_token(&device.id, &token).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => return internal_error(&error),
    }
    tracing::info!(device = device.id.as_str(), "device paired, token stored");
    notify_device(&state, &device.id).await;
    see_other(&format!("/devices/{}?notice=paired", device.id))
}

#[derive(Template)]
#[template(path = "device_remove.html")]
struct DeviceRemoveTemplate {
    chrome: Chrome,
    id: String,
    name: String,
}

async fn device_remove_page(State(state): State<AppState>, Path(raw): Path<String>) -> Response {
    match load_device(&state, &raw).await {
        Ok(device) => page(&DeviceRemoveTemplate {
            chrome: Chrome::new("devices"),
            id: device.id.as_str().to_owned(),
            name: device.name.clone(),
        }),
        Err(response) => response,
    }
}

async fn device_remove(State(state): State<AppState>, Path(raw): Path<String>) -> Response {
    let Some(id) = device_id(&raw) else {
        return not_found();
    };
    let _order = state.inner.config.lock().await;
    match state.store().delete_device(&id).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => return internal_error(&error),
    }
    tracing::info!(device = id.as_str(), "device removed");
    state.notify(ConfigChange::Device { id, run: None }).await;
    see_other("/devices?notice=removed")
}

// ----- settings ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    chrome: Chrome,
    ha_discovery: bool,
    min_len: usize,
}

async fn settings_template(state: &AppState, chrome: Chrome) -> Result<SettingsTemplate, Response> {
    match state.store().settings().await {
        Ok(settings) => Ok(SettingsTemplate {
            chrome,
            ha_discovery: settings.ha_discovery,
            min_len: MIN_PASSWORD_LEN,
        }),
        Err(error) => Err(internal_error(&error)),
    }
}

async fn settings_page(
    State(state): State<AppState>,
    Query(query): Query<NoticeQuery>,
) -> Response {
    match settings_template(
        &state,
        Chrome::new("settings").notice(query.notice.as_deref()),
    )
    .await
    {
        Ok(template) => page(&template),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub(super) struct DiscoveryForm {
    ha_discovery: Option<String>,
}

async fn discovery_save(
    State(state): State<AppState>,
    Form(form): Form<DiscoveryForm>,
) -> Response {
    let on = form.ha_discovery.is_some();
    let _order = state.inner.config.lock().await;
    if let Err(error) = state.store().set_ha_discovery(on).await {
        return internal_error(&error);
    }
    state.notify(ConfigChange::HaDiscovery(on)).await;
    see_other("/settings?notice=saved")
}

#[derive(Deserialize)]
pub(super) struct PasswordChangeForm {
    current: String,
    password: String,
    confirm: String,
}

async fn password_change(
    State(state): State<AppState>,
    Form(form): Form<PasswordChangeForm>,
) -> Response {
    let PasswordChangeForm {
        current,
        password,
        confirm,
    } = form;
    let shared = &state;
    let failed = |status: StatusCode, message: String| async move {
        match settings_template(shared, Chrome::new("settings").error(message)).await {
            Ok(template) => render(status, &template),
            Err(response) => response,
        }
    };
    let password = match new_password(password, &confirm) {
        Ok(password) => password,
        Err(message) => {
            return failed(StatusCode::UNPROCESSABLE_ENTITY, message.to_owned()).await;
        }
    };
    if let Err((status, message)) = check_password(&state, current).await {
        let message = if status == StatusCode::UNAUTHORIZED {
            "The current password is wrong.".to_owned()
        } else {
            message
        };
        return failed(status, message).await;
    }
    let hash = match auth::hash_password(password).await {
        Ok(hash) => hash,
        Err(error) => {
            tracing::error!(%error, "cannot hash the admin password");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(error) = state.store().replace_admin_password_hash(&hash).await {
        return internal_error(&error);
    }
    tracing::info!("admin password changed, every session logged out");
    state.sessions().clear();
    signed_in(&state, "/settings?notice=password")
}
