//! REST side of the Nanoleaf OpenAPI.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::Error;
use super::parse::{parse_effects_list, parse_root};
use crate::model::{Command, DeviceSnapshot, Ranges};
use crate::secret::Secret;

/// Port of the Nanoleaf OpenAPI.
pub const DEFAULT_PORT: u16 = 16021;

/// Timeout for one request (answer and body; for the event stream only the answer).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout of the pairing request.
const PAIR_TIMEOUT: Duration = Duration::from_secs(10);

/// Client for one Nanoleaf device. Cheap to clone; clones share the connection settings and the
/// value ranges last read from the device.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
    request_timeout: Option<Duration>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    /// `http://host:port/api/v1/`, without the token.
    base: String,
    /// `host:port`, for log messages.
    label: String,
    token: Secret<String>,
    ranges: Mutex<Option<Ranges>>,
}

impl Client {
    /// A client for the device at `host:port` using `token`. Nothing is sent yet.
    pub fn new(host: &str, port: u16, token: Secret<String>) -> Result<Self, Error> {
        let label = authority(host, port);
        Ok(Self {
            inner: Arc::new(Inner {
                http: http_client()?,
                base: format!("http://{label}/api/v1/"),
                label,
                token,
                ranges: Mutex::new(None),
            }),
            request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
        })
    }

    /// Replaces the request timeout; `None` waits forever (meant for tests on a paused clock).
    pub fn with_request_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// The request timeout in use.
    pub fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    /// `host:port` of the device, safe to log.
    pub fn label(&self) -> &str {
        &self.inner.label
    }

    /// Reads everything (`GET /`): identity, state, effects. Remembers the value ranges for the
    /// range check of later commands.
    pub async fn snapshot(&self) -> Result<DeviceSnapshot, Error> {
        const OP: &str = "GET /";
        let body = self
            .fetch(OP, self.request(reqwest::Method::GET, ""))
            .await?;
        let snapshot = parse_root(&body).map_err(|_| Error::UnexpectedResponse {
            operation: OP,
            detail: "body is not JSON",
        })?;
        *self.inner.ranges.lock().unwrap_or_else(|e| e.into_inner()) = Some(snapshot.state.ranges);
        Ok(snapshot)
    }

    /// Reads the names of the effects the device offers.
    pub async fn effects_list(&self) -> Result<Vec<String>, Error> {
        const OP: &str = "GET /effects/effectsList";
        let body = self
            .fetch(
                OP,
                self.request(reqwest::Method::GET, "effects/effectsList"),
            )
            .await?;
        parse_effects_list(&body).map_err(|_| Error::UnexpectedResponse {
            operation: OP,
            detail: "body is not JSON",
        })
    }

    /// Executes a command. Values are checked against the ranges the device reported (read
    /// first if not known yet); a value out of range is rejected before anything is sent.
    pub async fn send(&self, command: &Command) -> Result<(), Error> {
        let ranges = match self.known_ranges() {
            Some(ranges) => ranges,
            None => self.snapshot().await?.state.ranges,
        };
        command.check(&ranges)?;
        let (operation, path, body): (&'static str, &str, Option<Value>) = match command {
            Command::On(on) => ("PUT /state", "state", Some(json!({"on": {"value": on}}))),
            Command::Brightness { value, transition } => {
                let mut brightness = json!({"value": value});
                if let Some(seconds) = transition {
                    brightness["duration"] = json!(seconds);
                }
                (
                    "PUT /state",
                    "state",
                    Some(json!({"brightness": brightness})),
                )
            }
            Command::Hue(value) => (
                "PUT /state",
                "state",
                Some(json!({"hue": {"value": value}})),
            ),
            Command::Saturation(value) => (
                "PUT /state",
                "state",
                Some(json!({"sat": {"value": value}})),
            ),
            Command::ColorTemperature(value) => {
                ("PUT /state", "state", Some(json!({"ct": {"value": value}})))
            }
            Command::Effect(name) => ("PUT /effects", "effects", Some(json!({"select": name}))),
            Command::Identify => ("PUT /identify", "identify", None),
        };
        let mut request = self.request(reqwest::Method::PUT, path);
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.fetch(operation, request).await?;
        Ok(())
    }

    /// Opens the event stream for the given comma-separated event ids. The timeout covers the
    /// answer only; the body is an endless stream.
    pub(crate) async fn open_events(&self, ids: &str) -> Result<reqwest::Response, Error> {
        const OP: &str = "GET /events";
        let request = self
            .request(reqwest::Method::GET, "events")
            .query(&[("id", ids)]);
        let response = with_timeout(OP, self.request_timeout, async {
            request.send().await.map_err(|e| Error::http(OP, e))
        })
        .await?;
        check_status(OP, response.status())?;
        Ok(response)
    }

    fn known_ranges(&self) -> Option<Ranges> {
        *self.inner.ranges.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}/{}", self.inner.base, self.inner.token.expose(), path);
        self.inner.http.request(method, url)
    }

    /// Sends the request and reads the whole body, both within the request timeout.
    async fn fetch(
        &self,
        operation: &'static str,
        request: reqwest::RequestBuilder,
    ) -> Result<Vec<u8>, Error> {
        with_timeout(operation, self.request_timeout, async {
            let response = request
                .send()
                .await
                .map_err(|e| Error::http(operation, e))?;
            check_status(operation, response.status())?;
            let body = response
                .bytes()
                .await
                .map_err(|e| Error::http(operation, e))?;
            Ok(body.to_vec())
        })
        .await
    }
}

/// Asks the device at `host:port` for a new auth token (`POST /api/v1/new`). The device must be
/// in pairing mode: hold its power button for 5-7 seconds first.
pub async fn pair(host: &str, port: u16) -> Result<Secret<String>, Error> {
    const OP: &str = "POST /api/v1/new";
    let url = format!("http://{}/api/v1/new", authority(host, port));
    let request = http_client()?.post(url);
    let body = with_timeout(OP, Some(PAIR_TIMEOUT), async {
        let response = request.send().await.map_err(|e| Error::http(OP, e))?;
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::PairingRefused);
        }
        check_status(OP, response.status())?;
        response.bytes().await.map_err(|e| Error::http(OP, e))
    })
    .await?;
    let value: Value = serde_json::from_slice(&body).map_err(|_| Error::UnexpectedResponse {
        operation: OP,
        detail: "body is not JSON",
    })?;
    match value["auth_token"].as_str() {
        Some(token) if !token.is_empty() => Ok(Secret::new(token.to_owned())),
        _ => Err(Error::UnexpectedResponse {
            operation: OP,
            detail: "no auth_token in the answer",
        }),
    }
}

fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        // The device is on the local network; a proxy from the environment would receive the
        // token in the request path.
        .no_proxy()
        // Redirects would be logged with their URLs.
        .redirect(reqwest::redirect::Policy::none())
        // No idle pool: few requests, and the device has limited connection slots.
        .pool_max_idle_per_host(0)
        .build()
        .map_err(Error::Setup)
}

fn authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn check_status(operation: &'static str, status: reqwest::StatusCode) -> Result<(), Error> {
    if status.is_success() {
        Ok(())
    } else {
        Err(Error::Status {
            operation,
            status: status.as_u16(),
        })
    }
}

async fn with_timeout<T>(
    operation: &'static str,
    timeout: Option<Duration>,
    future: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    match timeout {
        Some(after) => tokio::time::timeout(after, future)
            .await
            .unwrap_or(Err(Error::Timeout { operation, after })),
        None => future.await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_formats() {
        assert_eq!(authority("192.0.2.1", 16021), "192.0.2.1:16021");
        assert_eq!(authority("panels.lan", 1), "panels.lan:1");
        assert_eq!(authority("fe80::1", 16021), "[fe80::1]:16021");
        assert_eq!(authority("[fe80::1]", 16021), "[fe80::1]:16021");
    }

    #[test]
    fn debug_hides_token() {
        let client = Client::new("192.0.2.1", 16021, Secret::new("tok-SECRET".to_owned())).unwrap();
        let out = format!("{client:?}");
        assert!(!out.contains("tok-SECRET"), "{out}");
    }
}
