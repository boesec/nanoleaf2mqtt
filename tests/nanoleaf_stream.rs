//! Following a device: reconnect, state re-read, reachability, backoff, health check.
//!
//! All tests run on tokio's paused clock: sleeps and intervals complete as soon as the runtime is
//! idle, without real waiting. The clients run without request timeout, so that the only timers
//! are the ones under test (a pending timeout would be advanced to while waiting for I/O).

// Test helpers outside `#[test]` functions panic on purpose, like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nanoleaf2mqtt::model::{DeviceEvent, StateChange};
use nanoleaf2mqtt::nanoleaf::{self, Client, StreamOptions};
use nanoleaf2mqtt::secret::Secret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "stream-test-token";

const ROOT_OFF: &str = r#"{"model":"NL42","state":{"on":{"value":false},"brightness":{"value":30,"min":0,"max":100}},"effects":{"select":"Blaze"}}"#;
const ROOT_ON: &str = r#"{"model":"NL42","state":{"on":{"value":true},"brightness":{"value":77,"min":0,"max":100}},"effects":{"select":"Prism"}}"#;

fn api(suffix: &str) -> String {
    format!("/api/v1/{TOKEN}/{suffix}")
}

fn client(address: SocketAddr) -> Client {
    Client::new(
        &address.ip().to_string(),
        address.port(),
        Secret::new(TOKEN.to_owned()),
    )
    .unwrap()
    .with_request_timeout(None)
}

fn snapshot_on(event: &DeviceEvent) -> Option<bool> {
    match event {
        DeviceEvent::Snapshot(snapshot) => snapshot.state.on,
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn stream_end_reconnects_and_rereads_state() {
    let server = MockServer::start().await;
    // First connection: one state event, then the device closes the stream.
    Mock::given(method("GET"))
        .and(path(api("events")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "id: 1\ndata: {\"events\":[{\"attr\":1,\"value\":true}]}\n\n",
            "text/event-stream",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Every later connection: one effect event, then closed again.
    Mock::given(method("GET"))
        .and(path(api("events")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "id: 3\ndata: {\"events\":[{\"attr\":1,\"value\":\"Prism\"}]}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    // The first read sees the light off, every later one on: the re-read must show it.
    Mock::given(method("GET"))
        .and(path(api("")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ROOT_OFF, "application/json"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(api("")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ROOT_ON, "application/json"))
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::channel(16);
    let task = nanoleaf::spawn(client(*server.address()), StreamOptions::default(), tx);

    assert_eq!(rx.recv().await, Some(DeviceEvent::Reachable));
    assert_eq!(snapshot_on(&rx.recv().await.unwrap()), Some(false));
    assert_eq!(
        rx.recv().await,
        Some(DeviceEvent::StateChanged(vec![StateChange::On(true)]))
    );
    assert_eq!(rx.recv().await, Some(DeviceEvent::Unreachable));
    let gone = Instant::now();
    assert_eq!(rx.recv().await, Some(DeviceEvent::Reachable));
    assert_eq!(
        gone.elapsed(),
        Duration::from_secs(1),
        "first backoff delay"
    );
    let snapshot = rx.recv().await.unwrap();
    assert_eq!(snapshot_on(&snapshot), Some(true), "state re-read");
    let DeviceEvent::Snapshot(snapshot) = snapshot else {
        unreachable!()
    };
    assert_eq!(snapshot.state.brightness, Some(77));
    assert_eq!(snapshot.state.effect.as_deref(), Some("Prism"));
    assert_eq!(
        rx.recv().await,
        Some(DeviceEvent::EffectChanged("Prism".to_owned()))
    );
    assert_eq!(rx.recv().await, Some(DeviceEvent::Unreachable));

    let requests = server.received_requests().await.unwrap();
    let streams = requests
        .iter()
        .filter(|r| r.url.path() == api("events"))
        .count();
    assert!(streams >= 2, "stream reopened, got {streams}");
    let request = requests
        .iter()
        .find(|r| r.url.path() == api("events"))
        .unwrap();
    assert_eq!(request.url.query(), Some("id=1,2,3,4"));

    // Dropping the receiver ends the task.
    drop(rx);
    task.await.unwrap();
}

/// Reports every request it answers, so the test can stamp it with the (paused) clock.
struct Recorder(mpsc::UnboundedSender<()>);

impl Respond for Recorder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        // Sent before the answer goes out, so the test sees it before the client can go on.
        self.0.send(()).unwrap();
        ResponseTemplate::new(503)
    }
}

#[tokio::test(start_paused = true)]
async fn device_down_at_start_retries_with_growing_delay() {
    let server = MockServer::start().await;
    let (seen_tx, mut seen) = mpsc::unbounded_channel();
    Mock::given(method("GET"))
        .respond_with(Recorder(seen_tx))
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::channel(16);
    let options = StreamOptions {
        backoff_max: Duration::from_secs(8),
        ..StreamOptions::default()
    };
    let task = nanoleaf::spawn(client(*server.address()), options, tx);

    let mut attempts = Vec::new();
    for _ in 0..7 {
        seen.recv().await.unwrap();
        attempts.push(Instant::now());
    }
    let delays: Vec<u64> = attempts
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs())
        .collect();
    assert_eq!(delays, [1, 2, 4, 8, 8, 8]);

    // Unreachable is reported once, not per attempt, and nothing else.
    assert_eq!(rx.recv().await, Some(DeviceEvent::Unreachable));
    assert!(rx.try_recv().is_err());
    drop(rx);
    task.await.unwrap();
}

/// A device that keeps the event stream open (wiremock always ends a body) and answers
/// `GET /` with 503 exactly once, on the second request.
struct FakeDevice {
    roots: AtomicUsize,
    streams: AtomicUsize,
}

impl FakeDevice {
    async fn start() -> (SocketAddr, Arc<Self>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let device = Arc::new(Self {
            roots: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
        });
        let shared = Arc::clone(&device);
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                tokio::spawn(Arc::clone(&shared).serve(socket));
            }
        });
        (address, device)
    }

    async fn serve(self: Arc<Self>, mut socket: TcpStream) {
        let mut head = Vec::new();
        let mut buf = [0_u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            head.extend_from_slice(&buf[..n]);
        }
        let head = String::from_utf8_lossy(&head);
        let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
        if target.starts_with(&api("events")) {
            self.streams.fetch_add(1, Ordering::SeqCst);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
                      id: 1\ndata: {\"events\":[{\"attr\":2,\"value\":10}]}\n\n",
                )
                .await
                .unwrap();
            // Keep the stream open and silent, like the real device.
            std::future::pending::<()>().await;
        } else if target == api("") {
            let n = self.roots.fetch_add(1, Ordering::SeqCst);
            let response = if n == 1 {
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned()
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{ROOT_ON}",
                    ROOT_ON.len()
                )
            };
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn failed_health_check_marks_unreachable_and_restarts_stream() {
    let (address, device) = FakeDevice::start().await;
    let (tx, mut rx) = mpsc::channel(16);
    let task = nanoleaf::spawn(client(address), StreamOptions::default(), tx);

    assert_eq!(rx.recv().await, Some(DeviceEvent::Reachable));
    let connected = Instant::now();
    assert_eq!(snapshot_on(&rx.recv().await.unwrap()), Some(true));
    assert_eq!(
        rx.recv().await,
        Some(DeviceEvent::StateChanged(vec![StateChange::Brightness(10)]))
    );
    // The stream stays silent; silence is fine until the health check fails after 30 s.
    assert_eq!(rx.recv().await, Some(DeviceEvent::Unreachable));
    assert_eq!(connected.elapsed(), Duration::from_secs(30));
    assert_eq!(device.streams.load(Ordering::SeqCst), 1);

    assert_eq!(rx.recv().await, Some(DeviceEvent::Reachable));
    assert_eq!(connected.elapsed(), Duration::from_secs(31));
    assert_eq!(snapshot_on(&rx.recv().await.unwrap()), Some(true));
    assert_eq!(
        rx.recv().await,
        Some(DeviceEvent::StateChanged(vec![StateChange::Brightness(10)]))
    );
    assert_eq!(device.streams.load(Ordering::SeqCst), 2, "stream reopened");
    assert_eq!(device.roots.load(Ordering::SeqCst), 3);

    drop(rx);
    task.await.unwrap();
}

/// The device only delivers every event kind when the id list reaches it as `id=1,2,3,4`; with
/// the commas percent-encoded it sends the first kind only. A decoding matcher cannot see the
/// difference, so this reads the request line exactly as it arrives on the wire.
#[tokio::test(start_paused = true)]
async fn event_stream_request_carries_the_id_list_unencoded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (line_tx, mut lines) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let line_tx = line_tx.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buffer[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let first = text.lines().next().unwrap_or_default().to_owned();
                let _ = line_tx.send(first);
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            });
        }
    });

    let (tx, rx) = mpsc::channel(16);
    let task = nanoleaf::spawn(client(address), StreamOptions::default(), tx);
    let line = loop {
        let line = lines.recv().await.unwrap();
        if line.contains("/events") {
            break line;
        }
    };
    assert_eq!(line, format!("GET {} HTTP/1.1", api("events?id=1,2,3,4")));

    drop(rx);
    task.abort();
}
