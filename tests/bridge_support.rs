//! Test support shared by the bridge and MQTT tests (included with `#[path]`): an in-process
//! broker, a subscriber that records what it sees, a fake device and a TCP proxy that can be
//! cut. Compiled on its own as well, with a self-test of the fake device.
//!
//! The fake device is a small hand-written HTTP server instead of `wiremock`, because the event
//! stream has to stay open (and be pushed to, and be cut) like the real device's; `wiremock`
//! always ends a response body.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    missing_docs,
    unreachable_pub
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nanoleaf2mqtt::bridge::BridgeOptions;
use nanoleaf2mqtt::model::{BrokerConfig, DeviceConfig, DeviceId};
use nanoleaf2mqtt::mqtt::Timing;
use nanoleaf2mqtt::nanoleaf::StreamOptions;
use nanoleaf2mqtt::secret::Secret;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

/// Upper bound for anything a test waits for; reaching it fails the test.
pub const WAIT: Duration = Duration::from_secs(20);

pub const TOKEN: &str = "bridge-test-token";

/// A complete device as `GET /` reports it (anonymised values).
pub fn root(on: bool, brightness: u16, effect: &str) -> Value {
    serde_json::json!({
        "name": "Test Shapes",
        "serialNo": "S0000TEST",
        "manufacturer": "Nanoleaf",
        "firmwareVersion": "12.0.3",
        "model": "NL42",
        "effects": {"select": effect, "effectsList": ["Blaze", "Prism", "Northern Lights"]},
        "panelLayout": {"layout": {"numPanels": 15}},
        "state": {
            "on": {"value": on},
            "brightness": {"value": brightness, "min": 0, "max": 100},
            "hue": {"value": 120, "min": 0, "max": 360},
            "sat": {"value": 80, "min": 0, "max": 100},
            "ct": {"value": 2700, "min": 1200, "max": 6500},
            "colorMode": "effect"
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Broker

/// Starts an in-process broker on a free port and returns the port once it completes an MQTT
/// handshake. A port taken in the meantime (it is free only when picked) means a new attempt on
/// another port, so a test never ends up on a broker that is not its own.
pub fn start_broker() -> u16 {
    for _ in 0..5 {
        let port = free_port();
        spawn_broker(port);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if mqtt_handshake(port) {
                return port;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    panic!("broker did not start");
}

/// Sends an MQTT 3.1.1 CONNECT and expects an accepting CONNACK.
fn mqtt_handshake(port: u16) -> bool {
    use std::io::{Read, Write};

    const CONNECT: [u8; 19] = [
        0x10, 17, 0, 4, b'M', b'Q', b'T', b'T', 4, 0x02, 0, 60, 0, 5, b'p', b'r', b'o', b'b', b'e',
    ];
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut ack = [0_u8; 4];
    let ok = stream.write_all(&CONNECT).is_ok()
        && stream.read_exact(&mut ack).is_ok()
        && ack == [0x20, 0x02, 0x00, 0x00];
    let _ = stream.write_all(&[0xE0, 0x00]);
    ok
}

fn spawn_broker(port: u16) {
    use rumqttd::{Broker, Config, ConnectionSettings, RouterConfig, ServerSettings};

    let server = ServerSettings {
        name: "v4".to_owned(),
        listen: SocketAddr::from(([127, 0, 0, 1], port)),
        tls: None,
        next_connection_delay_ms: 1,
        connections: ConnectionSettings {
            connection_timeout_ms: 60_000,
            max_payload_size: 1024 * 1024,
            max_inflight_count: 100,
            auth: None,
            external_auth: None,
            dynamic_filters: true,
        },
    };
    let config = Config {
        id: 0,
        router: RouterConfig {
            max_connections: 100,
            max_outgoing_packet_count: 200,
            max_segment_size: 100 * 1024 * 1024,
            max_segment_count: 10,
            ..RouterConfig::default()
        },
        v4: Some(HashMap::from([("v4".to_owned(), server)])),
        ..Config::default()
    };
    let mut broker = Broker::new(config);
    std::thread::spawn(move || {
        let _ = broker.start();
    });
}

/// A port that was free when picked and has not been handed out before in this process, so
/// that tests running in parallel never pick the same one.
pub fn free_port() -> u16 {
    static HANDED_OUT: Mutex<std::collections::BTreeSet<u16>> =
        Mutex::new(std::collections::BTreeSet::new());
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if HANDED_OUT.lock().unwrap().insert(port) {
            return port;
        }
    }
}

fn client_id(role: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("test-{role}-{}", NEXT.fetch_add(1, Ordering::SeqCst))
}

/// A message as a subscriber saw it.
#[derive(Debug, Clone)]
pub struct Msg {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retain: bool,
}

impl Msg {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.payload)
            .unwrap_or_else(|e| panic!("{}: not JSON ({e}): {}", self.topic, self.text()))
    }

    /// `val` of a `{val, ts, lc}` payload.
    pub fn val(&self) -> Value {
        self.json()["val"].clone()
    }
}

/// A subscriber that records every message it receives.
pub struct Watcher {
    rx: mpsc::UnboundedReceiver<Msg>,
    client: AsyncClient,
    task: JoinHandle<()>,
}

impl Watcher {
    /// Connects, subscribes to `filter` and returns once the subscription is confirmed.
    pub async fn start(port: u16, filter: &str) -> Self {
        let mut options = MqttOptions::new(client_id("watch"), "127.0.0.1", port);
        options.set_keep_alive(Duration::from_secs(30));
        options.set_max_packet_size(1024 * 1024, 1024 * 1024);
        let (client, mut eventloop) = AsyncClient::new(options, 64);
        client.subscribe(filter, QoS::AtLeastOnce).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let (subscribed_tx, mut subscribed) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::SubAck(_))) => {
                        subscribed_tx.send_replace(true);
                    }
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        let _ = tx.send(Msg {
                            topic: p.topic.clone(),
                            payload: p.payload.to_vec(),
                            retain: p.retain,
                        });
                    }
                    Ok(_) => {}
                    Err(e) => panic!("watcher connection failed: {e}"),
                }
            }
        });
        tokio::time::timeout(WAIT, subscribed.wait_for(|s| *s))
            .await
            .expect("subscription confirmed")
            .unwrap();
        Self { rx, client, task }
    }

    /// The next message.
    pub async fn next(&mut self) -> Msg {
        tokio::time::timeout(WAIT, self.rx.recv())
            .await
            .expect("a message within the wait limit")
            .expect("watcher running")
    }

    /// Every message up to and including the first one matching `pred`.
    pub async fn until(&mut self, mut pred: impl FnMut(&Msg) -> bool) -> Vec<Msg> {
        let mut seen = Vec::new();
        loop {
            let msg = self.next().await;
            let done = pred(&msg);
            seen.push(msg);
            if done {
                return seen;
            }
        }
    }

    /// Waits until every `(topic, text)` was seen, in any order; `None` accepts any payload.
    pub async fn until_all(&mut self, wanted: &[(&str, Option<&str>)]) -> Vec<Msg> {
        let mut missing: Vec<(String, Option<String>)> = wanted
            .iter()
            .map(|(t, x)| ((*t).to_owned(), x.map(str::to_owned)))
            .collect();
        self.until(|m| {
            missing.retain(|(topic, text)| {
                !(m.topic == *topic && text.as_ref().is_none_or(|t| m.text() == *t))
            });
            missing.is_empty()
        })
        .await
    }

    /// Waits for a message on `topic` whose text is `text`.
    pub async fn until_text(&mut self, topic: &str, text: &str) -> Vec<Msg> {
        self.until(|m| m.topic == topic && m.text() == text).await
    }

    /// Waits for a status message on `topic` whose `val` is `val`.
    pub async fn until_val(&mut self, topic: &str, val: Value) -> Vec<Msg> {
        self.until(|m| m.topic == topic && !m.payload.is_empty() && m.val() == val)
            .await
    }

    /// Publishes a marker on `topic` and returns everything received before it arrives back.
    /// Messages the broker sends in order before the marker are all included.
    pub async fn barrier(&mut self, topic: &str) -> Vec<Msg> {
        // Unique, so that another subscriber's marker on the same topic is not taken for it.
        let marker = client_id("barrier");
        self.client
            .publish(topic, QoS::AtLeastOnce, false, marker.clone())
            .await
            .unwrap();
        let mut seen = self.until_text(topic, &marker).await;
        seen.pop();
        seen
    }

    pub async fn stop(self) {
        let _ = self.client.disconnect().await;
        self.task.abort();
    }
}

/// Publishes one message from a separate client and waits until the broker acknowledged it.
pub async fn publish(port: u16, topic: &str, payload: &[u8], retain: bool) {
    let options = MqttOptions::new(client_id("pub"), "127.0.0.1", port);
    let (client, mut eventloop) = AsyncClient::new(options, 8);
    client
        .publish(topic, QoS::AtLeastOnce, retain, payload.to_vec())
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::PubAck(_))) => break,
                Ok(_) => {}
                Err(e) => panic!("publisher failed: {e}"),
            }
        }
    })
    .await
    .expect("publish acknowledged");
    let _ = client.disconnect().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), eventloop.poll()).await;
}

// ---------------------------------------------------------------------------------------------
// Configuration

pub fn broker(port: u16, prefix: &str) -> BrokerConfig {
    BrokerConfig {
        host: "127.0.0.1".to_owned(),
        port,
        username: None,
        password: None,
        client_id: client_id("bridge"),
        prefix: prefix.to_owned(),
    }
}

pub fn device(id: &str, address: SocketAddr) -> DeviceConfig {
    DeviceConfig {
        id: DeviceId::new(id).unwrap(),
        host: address.ip().to_string(),
        port: address.port(),
        token: Secret::new(TOKEN.to_owned()),
    }
}

/// Short delays, so that reconnects happen quickly in real time.
pub fn options() -> BridgeOptions {
    BridgeOptions {
        mqtt: Timing {
            keep_alive: Duration::from_secs(5),
            backoff_initial: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
        },
        stream: StreamOptions {
            backoff_initial: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            ..StreamOptions::default()
        },
        ..BridgeOptions::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Fake device

/// A command request the fake device received.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub method: String,
    /// Path below `/api/v1/<token>/`.
    pub path: String,
    /// The JSON body, `Null` if there was none.
    pub body: Value,
}

struct DeviceState {
    root: Mutex<Value>,
    down: AtomicBool,
    frames: broadcast::Sender<String>,
    cut: watch::Sender<u64>,
    requests: mpsc::UnboundedSender<Recorded>,
    streams: AtomicUsize,
}

/// A device that keeps its event stream open, accepts pushed events and records commands.
pub struct FakeDevice {
    pub address: SocketAddr,
    state: Arc<DeviceState>,
    requests: tokio::sync::Mutex<mpsc::UnboundedReceiver<Recorded>>,
    accept: JoinHandle<()>,
}

impl FakeDevice {
    pub async fn start(root: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = mpsc::unbounded_channel();
        let state = Arc::new(DeviceState {
            root: Mutex::new(root),
            down: AtomicBool::new(false),
            frames: broadcast::channel(64).0,
            cut: watch::channel(0).0,
            requests: requests_tx,
            streams: AtomicUsize::new(0),
        });
        let shared = Arc::clone(&state);
        let accept = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(Arc::clone(&shared), socket));
            }
        });
        Self {
            address,
            state,
            requests: tokio::sync::Mutex::new(requests_rx),
            accept,
        }
    }

    /// Sends one event frame to every open event stream.
    pub fn push(&self, id: u8, data: Value) {
        let frame = format!("id: {id}\ndata: {data}\n\n");
        // No open stream is a test error: the event would be lost.
        self.state.frames.send(frame).expect("an open event stream");
    }

    /// Replaces what `GET /` answers.
    pub fn set_root(&self, root: Value) {
        *self.state.root.lock().unwrap() = root;
    }

    /// Takes the device off the network (every open stream is cut, every request answers 503)
    /// or brings it back.
    pub fn set_down(&self, down: bool) {
        self.state.down.store(down, Ordering::SeqCst);
        if down {
            self.state.cut.send_modify(|generation| *generation += 1);
        }
    }

    /// Number of event streams opened so far.
    pub fn streams(&self) -> usize {
        self.state.streams.load(Ordering::SeqCst)
    }

    /// The next command (any request but `GET`) the device received.
    pub async fn next_command(&self) -> Recorded {
        let mut requests = self.requests.lock().await;
        tokio::time::timeout(WAIT, requests.recv())
            .await
            .expect("a command within the wait limit")
            .unwrap()
    }

    /// Commands received so far and not yet taken.
    pub async fn pending_commands(&self) -> Vec<Recorded> {
        let mut requests = self.requests.lock().await;
        let mut out = Vec::new();
        while let Ok(r) = requests.try_recv() {
            out.push(r);
        }
        out
    }
}

impl Drop for FakeDevice {
    fn drop(&mut self) {
        self.accept.abort();
        self.state.cut.send_modify(|generation| *generation += 1);
    }
}

async fn serve(state: Arc<DeviceState>, mut socket: TcpStream) {
    let Some((method, target, body)) = read_request(&mut socket).await else {
        return;
    };
    let base = format!("/api/v1/{TOKEN}/");
    let Some(path) = target.strip_prefix(&base) else {
        let _ = socket.write_all(&response(401, "")).await;
        return;
    };
    let path = path.split('?').next().unwrap_or_default().to_owned();
    if state.down.load(Ordering::SeqCst) {
        let _ = socket.write_all(&response(503, "")).await;
        return;
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "events") => {
            // Subscribed before the answer goes out: nothing pushed after it is lost.
            let mut frames = state.frames.subscribe();
            let mut cut = state.cut.subscribe();
            state.streams.fetch_add(1, Ordering::SeqCst);
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            loop {
                tokio::select! {
                    frame = frames.recv() => match frame {
                        Ok(frame) => {
                            if socket.write_all(frame.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    _ = cut.changed() => return,
                }
            }
        }
        ("GET", "") => {
            let root = state.root.lock().unwrap().to_string();
            let _ = socket.write_all(&response(200, &root)).await;
        }
        ("GET", "effects/effectsList") => {
            let list = state.root.lock().unwrap()["effects"]["effectsList"].to_string();
            let _ = socket.write_all(&response(200, &list)).await;
        }
        (_, _) => {
            let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let identify = path == "identify";
            let _ = state.requests.send(Recorded { method, path, body });
            let status = if identify { 200 } else { 204 };
            let _ = socket.write_all(&response(status, "")).await;
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut data = Vec::new();
    let mut buf = [0_u8; 4096];
    let head_end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = socket.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut words = head.split_whitespace();
    let method = words.next()?.to_owned();
    let target = words.next()?.to_owned();
    let length = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = data[head_end..].to_vec();
    while body.len() < length {
        let n = socket.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some((method, target, body))
}

fn response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        _ => "Service Unavailable",
    };
    let mut out = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n");
    if status != 204 {
        out.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out.into_bytes()
}

/// An address where connections are refused: a device that is switched off.
pub fn refused_address() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], free_port()))
}

/// A device that accepts connections and reads requests but never answers (a hanging device).
pub struct BlackHole {
    pub address: SocketAddr,
    pub requests: Arc<AtomicUsize>,
    accept: JoinHandle<()>,
}

impl BlackHole {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let accept = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    if read_request(&mut socket).await.is_some() {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    std::future::pending::<()>().await;
                });
            }
        });
        Self {
            address,
            requests,
            accept,
        }
    }
}

impl Drop for BlackHole {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

// ---------------------------------------------------------------------------------------------
// Proxy

/// A TCP proxy in front of the broker that can be cut, to make the broker unreachable.
pub struct Proxy {
    pub port: u16,
    broker_port: u16,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Proxy {
    pub async fn start(broker_port: u16) -> Self {
        let proxy = Self {
            port: free_port(),
            broker_port,
            tasks: Arc::new(Mutex::new(Vec::new())),
        };
        proxy.open().await;
        proxy
    }

    /// Starts accepting on the proxy port (again).
    pub async fn open(&self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", self.port)).await {
                Ok(listener) => break listener,
                Err(e) => {
                    assert!(tokio::time::Instant::now() < deadline, "proxy bind: {e}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        let broker_port = self.broker_port;
        let tasks = Arc::clone(&self.tasks);
        let accept = tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                let pipe = tokio::spawn(async move {
                    let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", broker_port)).await
                    else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
                tasks.lock().unwrap().push(pipe);
            }
        });
        self.tasks.lock().unwrap().push(accept);
    }

    /// Stops accepting and drops every connection through the proxy.
    pub async fn cut(&self) {
        let tasks: Vec<_> = self.tasks.lock().unwrap().drain(..).collect();
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Self-test

#[tokio::test]
async fn fake_device_serves_state_events_and_records_commands() {
    let device = FakeDevice::start(root(true, 30, "Prism")).await;
    let http = reqwest::Client::new();
    let base = format!("http://{}/api/v1/{TOKEN}/", device.address);

    let root: Value = http.get(&base).send().await.unwrap().json().await.unwrap();
    assert_eq!(root["state"]["brightness"]["value"], 30);

    let mut stream = http.get(format!("{base}events?id=1")).send().await.unwrap();
    assert_eq!(device.streams(), 1);
    device.push(1, serde_json::json!({"events":[{"attr":2,"value":40}]}));
    let chunk = stream.chunk().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("\"value\":40"));

    let status = http
        .put(format!("{base}state"))
        .json(&serde_json::json!({"on":{"value":false}}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 204);
    assert_eq!(
        device.next_command().await,
        Recorded {
            method: "PUT".to_owned(),
            path: "state".to_owned(),
            body: serde_json::json!({"on":{"value":false}}),
        }
    );

    device.set_down(true);
    assert!(stream.chunk().await.unwrap().is_none(), "stream cut");
    assert_eq!(http.get(&base).send().await.unwrap().status(), 503);
}
