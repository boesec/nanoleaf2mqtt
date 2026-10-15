//! End to end: the binary starts with an empty store, serves the UI and publishes nothing; the
//! broker and a device configured through the UI bring the device onto MQTT without a restart,
//! and the configuration is still there after one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, LOCATION, SET_COOKIE};
use serde_json::json;
use support::{FakeDevice, Watcher};

const PASSWORD: &str = "e2e-admin-password";

/// Kills the child when the test ends, also when it fails.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(data_dir: &Path, http_port: u16) -> KillOnDrop {
    let child = Command::new(env!("CARGO_BIN_EXE_nanoleaf2mqtt"))
        .env("NANOLEAF2MQTT_DATA_DIR", data_dir)
        .env("NANOLEAF2MQTT_HTTP_BIND", format!("127.0.0.1:{http_port}"))
        .env("NANOLEAF2MQTT_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    KillOnDrop(child)
}

struct Browser {
    http: reqwest::Client,
    base: String,
    cookie: Option<String>,
}

impl Browser {
    fn new(port: u16) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .unwrap(),
            base: format!("http://127.0.0.1:{port}"),
            cookie: None,
        }
    }

    /// Waits until the UI answers.
    async fn wait_until_up(&self) {
        tokio::time::timeout(support::WAIT, async {
            loop {
                if self
                    .http
                    .get(format!("{}/login", self.base))
                    .send()
                    .await
                    .is_ok()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the UI comes up");
    }

    fn remember(&mut self, response: &reqwest::Response) {
        if let Some(cookie) = response.headers().get(SET_COOKIE) {
            let pair = cookie.to_str().unwrap().split(';').next().unwrap();
            self.cookie = Some(pair.to_owned());
        }
    }

    async fn get(&mut self, path: &str) -> (StatusCode, Option<String>, String) {
        let mut request = self.http.get(format!("{}{path}", self.base));
        if let Some(cookie) = &self.cookie {
            request = request.header("cookie", cookie);
        }
        let response = request.send().await.unwrap();
        self.remember(&response);
        let status = response.status();
        let location = response
            .headers()
            .get(LOCATION)
            .map(|v| v.to_str().unwrap().to_owned());
        (status, location, response.text().await.unwrap())
    }

    /// A form POST as the browser sends it from a page of the UI.
    async fn post(&mut self, path: &str, form: &[(&str, &str)]) -> (StatusCode, Option<String>) {
        let body = form
            .iter()
            .map(|(k, v)| format!("{k}={}", v.replace('%', "%25").replace('&', "%26")))
            .collect::<Vec<_>>()
            .join("&");
        let mut request = self
            .http
            .post(format!("{}{path}", self.base))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("origin", &self.base)
            .body(body);
        if let Some(cookie) = &self.cookie {
            request = request.header("cookie", cookie);
        }
        let response = request.send().await.unwrap();
        self.remember(&response);
        let location = response
            .headers()
            .get(LOCATION)
            .map(|v| v.to_str().unwrap().to_owned());
        (response.status(), location)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_through_the_ui_without_a_restart() {
    let broker_port = support::start_broker();
    let mut watcher = Watcher::start(broker_port, "#").await;
    let device = FakeDevice::start(support::root(true, 64, "Prism")).await;
    let dir = tempfile::tempdir().unwrap();
    let http_port = support::free_port();
    let child = spawn(dir.path(), http_port);
    let mut browser = Browser::new(http_port);
    browser.wait_until_up().await;

    // Empty store: the first start page, nothing on MQTT.
    let (status, location, _) = browser.get("/").await;
    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/setup"))
    );
    let (status, location) = browser
        .post("/setup", &[("password", PASSWORD), ("confirm", PASSWORD)])
        .await;
    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/"))
    );
    let (status, _, page) = browser.get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("No broker configured yet"), "{page}");

    // A device first: followed, but still nothing is published.
    let device_port = device.address.port().to_string();
    let (status, location) = browser
        .post(
            "/devices",
            &[
                ("name", "Hall"),
                ("id", "dev"),
                ("host", "127.0.0.1"),
                ("port", &device_port),
                ("token", support::TOKEN),
            ],
        )
        .await;
    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/devices/dev?notice=added"))
    );
    let quiet = watcher.barrier("zz/barrier").await;
    assert!(
        quiet.is_empty(),
        "nothing published without a broker: {quiet:?}"
    );

    // Then the broker: the device appears on MQTT, no restart.
    let url = format!("mqtt://127.0.0.1:{broker_port}");
    let (status, _) = browser
        .post(
            "/broker",
            &[
                ("url", &url),
                ("username", ""),
                ("password", ""),
                ("prefix", "e2e"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let seen = watcher
        .until_all(&[
            ("e2e/connected", Some("2")),
            ("e2e/status/dev/brightness", None),
        ])
        .await;
    let brightness = seen
        .iter()
        .find(|m| m.topic == "e2e/status/dev/brightness")
        .unwrap();
    assert_eq!(brightness.val(), json!(64));
    let (_, _, page) = browser.get("/").await;
    assert!(page.contains("reachable"), "{page}");
    // Commands work as well.
    let mut late = Watcher::start(broker_port, "e2e/connected").await;
    late.until_text("e2e/connected", "2").await;
    support::publish(broker_port, "e2e/set/dev/on", b"off", false).await;
    assert_eq!(
        device.next_command().await.body,
        json!({"on":{"value":false}})
    );

    // Killed: the last will. Started again: the stored configuration is used at once.
    drop(child);
    watcher.until_text("e2e/connected", "0").await;
    let http_port = support::free_port();
    let _child = spawn(dir.path(), http_port);
    watcher.until_text("e2e/connected", "2").await;
    let mut browser = Browser::new(http_port);
    browser.wait_until_up().await;
    let (status, location, _) = browser.get("/").await;
    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/login")),
        "the password stays, the session does not"
    );
    let (status, location) = browser.post("/login", &[("password", PASSWORD)]).await;
    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/"))
    );
}
