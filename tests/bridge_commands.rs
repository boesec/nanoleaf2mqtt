//! Commands from MQTT to the device: exact requests, retained and invalid commands ignored, and
//! nothing published that the device did not report.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use nanoleaf2mqtt::bridge::Bridge;
use serde_json::{Value, json};
use support::{FakeDevice, Recorded, Watcher};

async fn running(port: u16, prefix: &str) -> (FakeDevice, Bridge) {
    let mut connected = Watcher::start(port, &format!("{prefix}/connected")).await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let bridge = Bridge::start(
        &support::broker(port, prefix),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    // The device is read (ranges and effect list known) ...
    connected
        .until_text(&format!("{prefix}/connected"), "2")
        .await;
    connected.stop().await;
    // ... and the command subscription stands.
    let mut ready = bridge.mqtt().ready();
    tokio::time::timeout(support::WAIT, ready.wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    (device, bridge)
}

fn put(path: &str, body: Value) -> Recorded {
    Recorded {
        method: "PUT".to_owned(),
        path: path.to_owned(),
        body,
    }
}

async fn set(port: u16, prefix: &str, path: &str, payload: &str) {
    support::publish(
        port,
        &format!("{prefix}/set/dev/{path}"),
        payload.as_bytes(),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_command_becomes_the_exact_device_request() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "c1/status/#").await;
    let (device, bridge) = running(port, "c1").await;
    watcher.barrier("c1/status/zz-barrier").await;

    let cases = [
        ("on", "off", put("state", json!({"on":{"value":false}}))),
        (
            "on",
            r#"{"val":true}"#,
            put("state", json!({"on":{"value":true}})),
        ),
        (
            "brightness",
            r#"{"val":50,"transition":2}"#,
            put("state", json!({"brightness":{"value":50,"duration":2}})),
        ),
        (
            "brightness",
            "20",
            put("state", json!({"brightness":{"value":20}})),
        ),
        ("hue", "240", put("state", json!({"hue":{"value":240}}))),
        (
            "sat",
            r#"{"val":60}"#,
            put("state", json!({"sat":{"value":60}})),
        ),
        ("ct", "4000", put("state", json!({"ct":{"value":4000}}))),
        (
            "effect",
            "Northern Lights",
            put("effects", json!({"select":"Northern Lights"})),
        ),
        ("identify", "", put("identify", Value::Null)),
    ];
    for (path, payload, expected) in cases {
        set(port, "c1", path, payload).await;
        assert_eq!(device.next_command().await, expected, "{path} {payload}");
    }

    // The device is the truth: a command publishes nothing by itself.
    let published = watcher.barrier("c1/status/zz-barrier").await;
    assert!(published.is_empty(), "{published:?}");

    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_commands_are_ignored() {
    let port = support::start_broker();
    // Left on the broker before the bridge starts, e.g. by a client that published with retain.
    support::publish(port, "c2/set/dev/on", b"false", true).await;
    let (device, bridge) = running(port, "c2").await;

    set(port, "c2", "brightness", "42").await;
    set(port, "c2", "hue", "100").await;
    assert_eq!(
        device.next_command().await,
        put("state", json!({"brightness":{"value":42}}))
    );
    assert_eq!(
        device.next_command().await,
        put("state", json!({"hue":{"value":100}}))
    );
    assert!(device.pending_commands().await.is_empty());

    support::publish(port, "c2/set/dev/on", b"", true).await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_commands_send_nothing() {
    let port = support::start_broker();
    let (device, bridge) = running(port, "c3").await;

    for (topic, payload) in [
        ("c3/set/dev/on", "maybe"),
        ("c3/set/dev/on", ""),
        ("c3/set/dev/brightness", "101"),
        ("c3/set/dev/brightness", "bright"),
        ("c3/set/dev/brightness", r#"{"val":50,"transition":-1}"#),
        ("c3/set/dev/hue", "361"),
        ("c3/set/dev/sat", "-5"),
        // Outside the range the device reports (1200-6500).
        ("c3/set/dev/ct", "9000"),
        // Not in the device's effect list, not even after re-reading it.
        ("c3/set/dev/effect", "No Such Effect"),
        ("c3/set/dev/blink", "true"),
        ("c3/set/dev/on/extra", "true"),
        ("c3/set/other/on", "true"),
        ("c3/set/Dev/on", "true"),
    ] {
        support::publish(port, topic, payload.as_bytes(), false).await;
    }
    set(port, "c3", "on", "true").await;

    assert_eq!(
        device.next_command().await,
        put("state", json!({"on":{"value":true}})),
        "the first request is the valid command sent last"
    );
    assert!(device.pending_commands().await.is_empty());
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_effect_added_since_the_last_read_is_accepted() {
    let port = support::start_broker();
    let (device, bridge) = running(port, "c4").await;
    let mut root = support::root(true, 30, "Prism");
    root["effects"]["effectsList"] = json!(["Blaze", "Prism", "Northern Lights", "New One"]);
    device.set_root(root);

    set(port, "c4", "effect", r#"{"val":"New One"}"#).await;
    assert_eq!(
        device.next_command().await,
        put("effects", json!({"select":"New One"}))
    );
    bridge.shutdown().await;
}
