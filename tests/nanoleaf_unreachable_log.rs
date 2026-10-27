//! Losing a device is logged once at `warn`, not on every retry, and never with the token or
//! the URL.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nanoleaf2mqtt::model::DeviceEvent;
use nanoleaf2mqtt::nanoleaf::{self, Client, StreamOptions};
use nanoleaf2mqtt::secret::Secret;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "unreachable-log-token-5d1e";
const ROOT: &str = r#"{"model":"NL42","state":{"on":{"value":true}},"effects":{"select":"Blaze"}}"#;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn losing_the_device_logs_one_warning_not_one_per_retry() {
    let capture = Capture::default();
    let writer = capture.clone();
    // Thread-local default: the test runs on one thread, and so does everything it spawns.
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_env_filter("warn,wiremock=off")
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish(),
    );

    let server = MockServer::start().await;
    // One good connection; afterwards the mock server answers 404 to everything.
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/{TOKEN}/events")))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/{TOKEN}/")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ROOT, "application/json"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let client = Client::new(
        &server.address().ip().to_string(),
        server.address().port(),
        Secret::new(TOKEN.to_owned()),
    )
    .unwrap()
    .with_request_timeout(None);
    let (tx, mut rx) = mpsc::channel(16);
    let task = nanoleaf::spawn(client, StreamOptions::default(), tx);

    assert_eq!(rx.recv().await, Some(DeviceEvent::Reachable));
    assert!(matches!(rx.recv().await, Some(DeviceEvent::Snapshot(_))));
    assert_eq!(rx.recv().await, Some(DeviceEvent::Unreachable));
    // Many failed retries follow (backoff 1, 2, 4, ... seconds).
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert!(rx.try_recv().is_err(), "no further events while down");

    drop(rx);
    task.await.unwrap();

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("device unreachable"))
        .collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains("WARN"), "{}", lines[0]);
    assert!(
        lines[0].contains("error="),
        "error chain missing: {}",
        lines[0]
    );
    assert!(!log.contains(TOKEN), "token in log: {log}");
    assert!(!log.contains("/api/v1"), "URL path in log: {log}");
    assert!(!log.contains("http://"), "URL in log: {log}");
    assert!(lines[0].contains("device="), "device label missing");
}
