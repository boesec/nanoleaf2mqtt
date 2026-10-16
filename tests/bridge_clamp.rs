//! Command values outside the device's range are clamped into it and logged at `warn` with
//! topic and payload; payloads that are no number and unknown effects are still rejected.
//!
//! One test per file: it installs the global log subscriber.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use nanoleaf2mqtt::bridge::Bridge;
use serde_json::{Value, json};
use support::{Recorded, Watcher};
use tracing_subscriber::util::SubscriberInitExt;

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

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn put(body: Value) -> Recorded {
    Recorded {
        method: "PUT".to_owned(),
        path: "state".to_owned(),
        body,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn out_of_range_values_are_clamped_and_logged() {
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish()
        .try_init()
        .unwrap();

    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "k1/status/dev/online").await;
    // Colour temperature 1200-6500 K as the device reports it.
    let device = support::FakeDevice::start(support::root(true, 30, "Prism")).await;
    let bridge = Bridge::start(
        &support::broker(port, "k1"),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    watcher.until_val("k1/status/dev/online", json!(true)).await;
    bridge.mqtt().ready().wait_for(|r| *r).await.unwrap();

    let set = |path: &str, payload: &'static str| {
        let topic = format!("k1/set/dev/{path}");
        async move { support::publish(port, &topic, payload.as_bytes(), false).await }
    };

    set("ct", "99999").await;
    assert_eq!(
        device.next_command().await,
        put(json!({"ct":{"value":6500}}))
    );
    set("ct", "-5").await;
    assert_eq!(
        device.next_command().await,
        put(json!({"ct":{"value":1200}}))
    );
    set("hue", "400").await;
    assert_eq!(
        device.next_command().await,
        put(json!({"hue":{"value":360}}))
    );
    set("brightness", "-5").await;
    assert_eq!(
        device.next_command().await,
        put(json!({"brightness":{"value":0}}))
    );
    set("hs", "400,150").await;
    assert_eq!(
        device.next_command().await,
        put(json!({"hue":{"value":360}}))
    );
    assert_eq!(
        device.next_command().await,
        put(json!({"sat":{"value":100}}))
    );

    // Not a number, an unknown effect: nothing is sent.
    set("ct", "warm").await;
    set("effect", "No Such Effect").await;
    set("on", "true").await;
    assert_eq!(
        device.next_command().await,
        put(json!({"on":{"value":true}})),
        "the first request is the valid command sent last"
    );
    assert!(device.pending_commands().await.is_empty());
    bridge.shutdown().await;

    let logs = capture.text();
    let clamped: Vec<&str> = logs
        .lines()
        .filter(|l| l.contains("value out of range, clamped"))
        .collect();
    assert_eq!(clamped.len(), 5, "{logs}");
    for (line, (topic, payload)) in clamped.iter().zip([
        ("k1/set/dev/ct", "99999"),
        ("k1/set/dev/ct", "-5"),
        ("k1/set/dev/hue", "400"),
        ("k1/set/dev/brightness", "-5"),
        ("k1/set/dev/hs", "400,150"),
    ]) {
        assert!(line.contains(" WARN "), "{line}");
        assert!(line.contains(&format!("topic=\"{topic}\"")), "{line}");
        assert!(line.contains(&format!("payload={payload}")), "{line}");
    }
    let invalid = logs
        .lines()
        .find(|l| l.contains("invalid command ignored"))
        .expect("the unparsable command is logged");
    assert!(
        invalid.contains("topic=\"k1/set/dev/ct\"") && invalid.contains("payload=warm"),
        "{invalid}"
    );
    let effect = logs
        .lines()
        .find(|l| l.contains("unknown effect ignored"))
        .expect("the unknown effect is logged");
    assert!(effect.contains("payload=No Such Effect"), "{effect}");
}
