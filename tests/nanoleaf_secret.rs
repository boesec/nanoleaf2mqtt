//! The auth token never appears in errors or log output, even at trace level and including
//! records of dependencies that log through the `log` crate.
//!
//! One test per file: it installs the global subscriber.

// Test helpers outside `#[test]` functions panic on purpose, like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nanoleaf2mqtt::model::{Command, DeviceEvent};
use nanoleaf2mqtt::nanoleaf::{self, Client, Error, StreamOptions};
use nanoleaf2mqtt::secret::Secret;
use tokio::sync::mpsc;
use tracing_subscriber::util::SubscriberInitExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "tOkEn-MuSt-NeVeR-LeAk-42";

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

fn client(address: SocketAddr) -> Client {
    Client::new(
        &address.ip().to_string(),
        address.port(),
        Secret::new(TOKEN.to_owned()),
    )
    .unwrap()
}

fn assert_clean(err: &Error, texts: &mut Vec<String>) {
    for text in [err.to_string(), format!("{err:?}"), err.chain()] {
        assert!(!text.contains(TOKEN), "token in error text: {text}");
        texts.push(text);
    }
}

/// An address on which nothing listens.
fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

#[tokio::test]
async fn secret_never_in_errors() {
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        // Everything at trace level, except the mock server, which is the device's side and
        // logs the requests it receives.
        .with_env_filter("trace,wiremock=off")
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish()
        .try_init()
        .unwrap();

    let mut texts = Vec::new();

    // Transport error: nobody listening.
    let down = client(closed_port());
    let err = down.snapshot().await.unwrap_err();
    assert!(matches!(err, Error::Http { .. }), "{err:?}");
    assert_clean(&err, &mut texts);

    // HTTP errors and a timeout.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/{TOKEN}/")))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/{TOKEN}/effects/effectsList")))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;
    let device = client(*server.address());
    let err = device.snapshot().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 401, .. }), "{err}");
    assert_clean(&err, &mut texts);
    let err = device.send(&Command::On(true)).await.unwrap_err();
    assert_clean(&err, &mut texts);
    let err = device
        .clone()
        .with_request_timeout(Some(Duration::from_millis(50)))
        .effects_list()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err}");
    assert_clean(&err, &mut texts);

    // The stream supervisor logs its failures; run it against the rejecting device briefly.
    let (tx, mut rx) = mpsc::channel(4);
    let options = StreamOptions {
        backoff_initial: Duration::from_millis(1),
        backoff_max: Duration::from_millis(1),
        ..StreamOptions::default()
    };
    let task = nanoleaf::spawn(device, options, tx);
    assert_eq!(rx.recv().await, Some(DeviceEvent::Unreachable));
    drop(rx);
    task.await.unwrap();

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("device unreachable"),
        "log capture works: {logs}"
    );
    assert!(!logs.contains(TOKEN), "token in log output:\n{logs}");
    assert!(texts.iter().any(|t| t.contains("401")));
}
