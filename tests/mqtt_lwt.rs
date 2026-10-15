//! The broker side of availability: last will, reconnect with full republish, graceful shutdown,
//! clearing a removed device.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeSet;

use nanoleaf2mqtt::bridge::Bridge;
use nanoleaf2mqtt::model::DeviceId;
use nanoleaf2mqtt::mqtt::{Mqtt, StatusPath};
use serde_json::json;
use support::{FakeDevice, Proxy, Watcher};

const RETAINED: [&str; 10] = [
    "reachable",
    "on",
    "brightness",
    "hue",
    "sat",
    "ct",
    "colormode",
    "effect",
    "effects",
    "device",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_loss_leaves_the_last_will_and_the_return_republishes_everything() {
    let broker_port = support::start_broker();
    let proxy = Proxy::start(broker_port).await;
    let mut watcher = Watcher::start(broker_port, "l1/#").await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let bridge = Bridge::start(
        &support::broker(proxy.port, "l1"),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    watcher.until_text("l1/connected", "2").await;

    proxy.cut().await;
    watcher.until_text("l1/connected", "0").await;

    // Changed while the broker is away: must arrive after the return.
    device.push(1, json!({"events":[{"attr":2,"value":88}]}));
    proxy.open().await;
    let mut seen = watcher.until_text("l1/connected", "2").await;
    seen.extend(
        watcher
            .until_val("l1/status/dev/brightness", json!(88))
            .await,
    );
    seen.extend(watcher.barrier("l1/zz-barrier").await);
    let topics: BTreeSet<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert!(topics.contains("l1/info"), "{topics:?}");
    for path in RETAINED {
        assert!(
            topics.contains(format!("l1/status/dev/{path}").as_str()),
            "{path} republished: {topics:?}"
        );
    }
    assert!(bridge.mqtt().is_connected());

    bridge.shutdown().await;
    proxy.cut().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_leaves_the_last_will() {
    let broker_port = support::start_broker();
    let mut watcher = Watcher::start(broker_port, "l2/connected").await;
    let (mqtt, _commands, task) =
        Mqtt::spawn(&support::broker(broker_port, "l2"), support::options().mqtt).unwrap();
    watcher.until_text("l2/connected", "1").await;
    // Dropping the event loop without a disconnect is what a crash looks like to the broker.
    task.abort();
    let _ = task.await;
    watcher.until_text("l2/connected", "0").await;
    drop(mqtt);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_shutdown_publishes_connected_zero() {
    let broker_port = support::start_broker();
    let mut watcher = Watcher::start(broker_port, "l3/#").await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let bridge = Bridge::start(
        &support::broker(broker_port, "l3"),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    watcher.until_text("l3/connected", "2").await;
    let mqtt = bridge.mqtt().clone();
    bridge.shutdown().await;
    let seen = watcher.until_text("l3/connected", "0").await;
    // Nothing of the device is published after the stop, so `0` is the last word.
    let after = watcher.barrier("l3/zz-barrier").await;
    assert!(after.is_empty(), "{after:?}");
    assert!(seen.iter().all(|m| m.text() != "1"), "{seen:?}");
    assert!(!mqtt.is_connected());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_device_has_its_retained_topics_cleared() {
    let broker_port = support::start_broker();
    let (mqtt, _commands, task) =
        Mqtt::spawn(&support::broker(broker_port, "l4"), support::options().mqtt).unwrap();
    let mut ready = mqtt.ready();
    ready.wait_for(|r| *r).await.unwrap();
    let gone = DeviceId::new("gone").unwrap();
    let kept = DeviceId::new("kept").unwrap();
    let mut watcher = Watcher::start(broker_port, "l4/status/#").await;
    mqtt.publish_status(&gone, StatusPath::On, json!(true));
    mqtt.publish_status(&gone, StatusPath::Brightness, json!(5));
    mqtt.publish_status(&kept, StatusPath::On, json!(false));
    watcher.until_val("l4/status/kept/on", json!(false)).await;

    mqtt.clear_device(&gone);
    let retained_paths = StatusPath::ALL.iter().filter(|p| p.retained()).count();
    let mut cleared = Vec::new();
    while cleared.len() < retained_paths {
        cleared.push(watcher.next().await);
    }
    assert!(cleared.iter().all(|m| m.payload.is_empty()), "{cleared:?}");
    assert!(
        cleared
            .iter()
            .all(|m| m.topic.starts_with("l4/status/gone/"))
    );
    watcher.stop().await;

    let mut late = Watcher::start(broker_port, "l4/status/#").await;
    let retained = late.barrier("l4/status/zz-barrier").await;
    let topics: Vec<&str> = retained.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(topics, ["l4/status/kept/on"]);

    mqtt.shutdown().await;
    task.await.unwrap();
}
