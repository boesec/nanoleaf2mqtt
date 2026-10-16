//! The panel layout on MQTT: published with the snapshot, kept current by layout events,
//! absent when the device reports no position data.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use nanoleaf2mqtt::bridge::Bridge;
use serde_json::{Value, json};
use support::{FakeDevice, Watcher};

fn root_with_layout(rotation: u16) -> Value {
    let mut root = support::root(true, 30, "Prism");
    root["panelLayout"] = json!({
        "globalOrientation": {"value": 120, "min": 0, "max": 360},
        "layout": {
            "numPanels": 3,
            "sideLength": 134,
            "positionData": [
                {"panelId": 1001, "x": 201, "y": 77, "o": rotation, "shapeType": 8},
                {"panelId": 1002, "x": 201, "y": 0, "o": 180, "shapeType": 99},
                {"panelId": 0, "x": 260, "y": 73, "o": 300, "shapeType": 12}
            ]
        }
    });
    root
}

async fn start(prefix: &str, root: Value) -> (u16, FakeDevice, Bridge, Watcher) {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, &format!("{prefix}/#")).await;
    let device = FakeDevice::start(root).await;
    let bridge = Bridge::start(
        &support::broker(port, prefix),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    watcher
        .until_all(&[
            (&format!("{prefix}/connected"), Some("2")),
            (&format!("{prefix}/status/dev/device"), None),
            (&format!("{prefix}/status/dev/reachable"), None),
        ])
        .await;
    (port, device, bridge, watcher)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_snapshot_publishes_the_layout_retained() {
    let (port, _device, bridge, watcher) = start("l1", root_with_layout(0)).await;
    watcher.stop().await;

    let mut late = Watcher::start(port, "l1/#").await;
    let retained = late.barrier("l1/zz-barrier").await;
    let message = retained
        .iter()
        .find(|m| m.topic == "l1/status/dev/layout")
        .expect("layout topic");
    assert!(message.retain);
    let payload = message.json();
    assert_eq!(payload.as_object().unwrap().len(), 3, "{payload}");
    assert!(payload["lc"].as_i64().unwrap() <= payload["ts"].as_i64().unwrap());
    assert_eq!(
        payload["val"],
        json!({
            "orientation": 120,
            "side_length": 134,
            "panels": [
                {"id": 1001, "x": 201, "y": 77, "rotation": 0, "shape": "triangle_shapes", "shape_type": 8},
                {"id": 1002, "x": 201, "y": 0, "rotation": 180, "shape": "unknown", "shape_type": 99},
                {"id": 0, "x": 260, "y": 73, "rotation": 300, "shape": "controller", "shape_type": 12}
            ]
        })
    );
    late.stop().await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_position_data_means_no_layout_topic() {
    let (port, _device, bridge, watcher) = start("l2", support::root(true, 30, "Prism")).await;
    watcher.stop().await;
    let mut late = Watcher::start(port, "l2/#").await;
    let retained = late.barrier("l2/zz-barrier").await;
    assert!(
        retained.iter().all(|m| m.topic != "l2/status/dev/layout"),
        "{retained:?}"
    );
    assert!(retained.iter().any(|m| m.topic == "l2/status/dev/device"));
    late.stop().await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_layout_event_republishes_only_a_changed_layout() {
    let (_port, device, bridge, mut watcher) = start("l3", root_with_layout(0)).await;
    watcher.barrier("l3/zz-barrier").await;

    let topic = "l3/status/dev/layout";

    // Layout event, nothing changed: re-read, no new message. The device's events are handled
    // in order, so the effect change that follows proves the re-read is done.
    device.push(2, json!({"events": []}));
    device.push(3, json!({"events": [{"attr": 1, "value": "Blaze"}]}));
    let seen = watcher
        .until_val("l3/status/dev/effect", json!("Blaze"))
        .await;
    assert!(seen.iter().all(|m| m.topic != topic), "{seen:?}");

    // A real change is published.
    device.set_root(root_with_layout(60));
    device.push(2, json!({"events": []}));
    let seen = watcher.until(|m| m.topic == topic).await;
    assert_eq!(
        seen.last().unwrap().val()["panels"][0]["rotation"],
        json!(60)
    );
    bridge.shutdown().await;
}
