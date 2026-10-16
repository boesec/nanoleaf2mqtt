//! The republish after a connect keeps the promise of `online`: it goes out after the values
//! of its device. Forced independent of timing: everything is settled before any broker is
//! configured, so all of it goes through the republish.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeSet;

use nanoleaf2mqtt::bridge::{Bridge, BridgeSetup};
use nanoleaf2mqtt::model::{ConfigChange, DeviceId};
use nanoleaf2mqtt::mqtt::{Mqtt, StatusPath};
use serde_json::json;
use support::{FakeDevice, Msg, Watcher};

/// For each device, `online` comes after every other status value of that device.
fn assert_online_last(prefix: &str, seen: &[Msg], devices: &[&str]) {
    for device in devices {
        let start = format!("{prefix}/status/{device}/");
        let order: Vec<&str> = seen
            .iter()
            .filter_map(|m| m.topic.strip_prefix(&start))
            .collect();
        assert!(order.len() > 1, "{device}: {order:?}");
        assert_eq!(order.last(), Some(&"online"), "{device}: {order:?}");
        assert_eq!(
            order.iter().filter(|p| **p == "online").count(),
            1,
            "{device}: {order:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_republish_sends_online_after_the_values() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "p1/#").await;
    let (mqtt, _commands) = Mqtt::new(support::options().mqtt);
    // Reported before any broker exists, `online` even first.
    for device in ["a", "b"] {
        let id = DeviceId::new(device).unwrap();
        mqtt.publish_status(&id, StatusPath::Online, json!(true));
        mqtt.publish_status(&id, StatusPath::On, json!(true));
        mqtt.publish_status(&id, StatusPath::Brightness, json!(30));
        mqtt.publish_status(&id, StatusPath::Device, json!({"model": "NL42"}));
    }
    mqtt.switch(Some(&support::broker(port, "p1")))
        .await
        .unwrap();
    mqtt.ready().wait_for(|r| *r).await.unwrap();
    let mut wanted: BTreeSet<String> = ["a", "b"]
        .iter()
        .flat_map(|d| {
            ["online", "on", "brightness", "device"].map(|p| format!("p1/status/{d}/{p}"))
        })
        .collect();
    let seen = watcher
        .until(|m| {
            wanted.remove(&m.topic);
            wanted.is_empty()
        })
        .await;
    assert_online_last("p1", &seen, &["a", "b"]);
    mqtt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_settled_before_the_broker_reports_online_last() {
    let device = FakeDevice::start(support::root(true, 42, "Prism")).await;
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "p2/#").await;
    let bridge = Bridge::launch(BridgeSetup {
        broker: None,
        devices: vec![support::device("dev", device.address)],
        options: support::options(),
        ..BridgeSetup::default()
    });
    let handle = bridge.handle();
    let id = DeviceId::new("dev").unwrap();
    tokio::time::timeout(support::WAIT, async {
        while handle.status().device(&id).and_then(|d| d.reachable) != Some(true) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("device settled without a broker");

    handle
        .apply(ConfigChange::Broker(Some(support::broker(port, "p2"))))
        .await
        .unwrap();
    let seen = watcher.until(|m| m.topic == "p2/status/dev/online").await;
    for path in [
        "on",
        "brightness",
        "hue",
        "sat",
        "hs",
        "ct",
        "effect",
        "device",
    ] {
        let topic = format!("p2/status/dev/{path}");
        assert!(
            seen.iter().any(|m| m.topic == topic),
            "{path} before online"
        );
    }
    assert_online_last("p2", &seen, &["dev"]);
    bridge.shutdown().await;
}
