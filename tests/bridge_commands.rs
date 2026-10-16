//! Commands from MQTT to the device: exact requests, retained and invalid commands ignored, and
//! nothing published that the device did not report.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use nanoleaf2mqtt::bridge::Bridge;
use serde_json::{Value, json};
use support::{FakeDevice, Recorded, Watcher};

async fn running(port: u16, prefix: &str) -> (FakeDevice, Bridge) {
    running_with(port, prefix, support::root(true, 30, "Prism")).await
}

/// A bridge following a device that reports `root`, once the device was read (ranges and
/// effect list known: `online` follows them) and the command subscription stands.
async fn running_with(port: u16, prefix: &str, root: Value) -> (FakeDevice, Bridge) {
    let online = format!("{prefix}/status/dev/online");
    let mut watcher = Watcher::start(port, &online).await;
    let device = FakeDevice::start(root).await;
    let bridge = Bridge::start(
        &support::broker(port, prefix),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    watcher.until_val(&online, json!(true)).await;
    watcher.stop().await;
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
    watcher.stop().await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_command_the_device_report_is_published_not_what_was_sent() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "c5/status/#").await;
    let (device, bridge) = running(port, "c5").await;
    // `online` comes after the initial values. Only then is the barrier behind every one.
    watcher.until_val("c5/status/dev/online", json!(true)).await;
    watcher.barrier("c5/status/zz-barrier").await;

    // The device ignores the command, but its hue was changed elsewhere and not reported yet:
    // the state read back after the command shows the hue, and no brightness 20.
    let mut root = support::root(true, 30, "Prism");
    root["state"]["hue"]["value"] = json!(99);
    device.set_root(root);
    set(port, "c5", "brightness", "20").await;
    device.next_command().await;
    let seen = watcher.until_val("c5/status/dev/hs", json!([99, 80])).await;
    let topics: Vec<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(
        topics,
        ["c5/status/dev/hue", "c5/status/dev/hs"],
        "only what changed (hs follows hue)"
    );

    // The device follows the command: the new value is there without waiting for its event
    // stream (nothing is pushed on it here), and only that value.
    let mut root = support::root(true, 20, "Prism");
    root["state"]["hue"]["value"] = json!(99);
    device.set_root(root);
    set(port, "c5", "brightness", "20").await;
    device.next_command().await;
    let seen = watcher
        .until_val("c5/status/dev/brightness", json!(20))
        .await;
    let topics: Vec<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(topics, ["c5/status/dev/brightness"]);

    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_event_stream_echo_of_a_command_publishes_nothing_more() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "c7/status/#").await;
    let (device, bridge) = running(port, "c7").await;
    watcher.barrier("c7/status/zz-barrier").await;

    // The command: the read-back after it publishes the new brightness.
    device.set_root(support::root(true, 55, "Prism"));
    set(port, "c7", "brightness", "55").await;
    device.next_command().await;
    watcher
        .until_val("c7/status/dev/brightness", json!(55))
        .await;
    // The device reports the same change on its event stream about 1.5 s later, and the
    // effect, unchanged, too. A real change in the same frame shows that the frame was handled.
    device.push(
        1,
        json!({"events":[{"attr":2,"value":55},{"attr":3,"value":200}]}),
    );
    device.push(3, json!({"events":[{"attr":1,"value":"Prism"}]}));
    device.push(1, json!({"events":[{"attr":4,"value":10}]}));
    let mut seen = watcher
        .until_val("c7/status/dev/hs", json!([200, 10]))
        .await;
    seen.extend(watcher.barrier("c7/status/zz-barrier").await);
    let topics: Vec<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(
        topics,
        [
            "c7/status/dev/hue",
            "c7/status/dev/hs",
            "c7/status/dev/sat",
            "c7/status/dev/hs"
        ],
        "one message per changed topic, none for the echo"
    );

    // A reconnect of the device still publishes everything.
    device.set_down(true);
    watcher
        .until_val("c7/status/dev/online", json!(false))
        .await;
    device.set_down(false);
    let seen = watcher.until_val("c7/status/dev/online", json!(true)).await;
    for path in ["on", "brightness", "hue", "sat", "hs", "ct", "effect"] {
        let topic = format!("c7/status/dev/{path}");
        assert!(seen.iter().any(|m| m.topic == topic), "{path} republished");
    }
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
        ("c3/set/dev/brightness", "bright"),
        ("c3/set/dev/brightness", r#"{"val":50,"transition":"slow"}"#),
        ("c3/set/dev/brightness", r#"{"value":50}"#),
        ("c3/set/dev/hue", "true"),
        ("c3/set/dev/sat", "[5]"),
        ("c3/set/dev/ct", "warm"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hs_becomes_hue_then_saturation_and_is_clamped_per_component() {
    let port = support::start_broker();
    // The device accepts saturation up to 50 only.
    let mut root = support::root(true, 30, "Prism");
    root["state"]["sat"]["max"] = json!(50);
    let (device, bridge) = running_with(port, "c6", root).await;

    for (payload, hue, sat) in [
        ("120,40", 120, 40),
        (r#"{"val":[10,20]}"#, 10, 20),
        // Saturation outside the device's range, hue outside the contract's: each clamped.
        ("120,80", 120, 50),
        ("361,10", 360, 10),
        ("400,150", 360, 50),
    ] {
        set(port, "c6", "hs", payload).await;
        assert_eq!(
            device.next_command().await,
            put("state", json!({"hue":{"value":hue}})),
            "{payload}"
        );
        assert_eq!(
            device.next_command().await,
            put("state", json!({"sat":{"value":sat}})),
            "{payload}"
        );
    }

    // Unparsable: not even the hue goes out.
    for payload in ["120,red", "120", "[1,2,3]", "red"] {
        set(port, "c6", "hs", payload).await;
    }
    set(port, "c6", "on", "true").await;
    assert_eq!(
        device.next_command().await,
        put("state", json!({"on":{"value":true}})),
        "the first request is the valid command sent last"
    );
    assert!(device.pending_commands().await.is_empty());
    bridge.shutdown().await;
}
