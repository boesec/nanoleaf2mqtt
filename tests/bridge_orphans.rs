//! Retained topics a previous run left behind: read back on every connect, and every one that
//! does not belong to a configured device and a status path of the contract is cleared. The
//! topics of a configured device that is not reachable yet stay.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeSet;

use nanoleaf2mqtt::bridge::Bridge;
use serde_json::json;
use support::{FakeDevice, Proxy, Watcher};

/// A status payload as a previous run published it.
fn status(val: serde_json::Value) -> Vec<u8> {
    json!({"val": val, "ts": 1_700_000_000_000_i64, "lc": 1_700_000_000_000_i64})
        .to_string()
        .into_bytes()
}

/// What a fresh subscriber gets retained (non-empty) below `filter`.
async fn retained(port: u16, filter: &str, barrier: &str) -> BTreeSet<String> {
    let mut late = Watcher::start(port, filter).await;
    let seen = late.barrier(barrier).await;
    late.stop().await;
    seen.into_iter()
        .filter(|m| !m.payload.is_empty())
        .map(|m| m.topic)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphans_are_cleared_at_start_and_configured_devices_kept() {
    let port = support::start_broker();
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let orphans = [
        // Contract version 1.
        "o1/status/dev/reachable",
        // A device removed while the service was down.
        "o1/status/gone/on",
        "o1/status/gone/online",
        // Paths the contract does not have.
        "o1/status/dev/colour",
        "o1/status/dev/on/extra",
        "o1/status/dev",
        "o1/status/Bad/on",
    ];
    // A configured device that is switched off: what it reported last stays.
    let kept_offline = ["o1/status/off/on", "o1/status/off/brightness"];
    for topic in orphans.iter().chain(&kept_offline) {
        support::publish(port, topic, &status(json!(true)), true).await;
    }
    // Another instance's topics are not this one's business.
    support::publish(port, "o2/status/gone/on", &status(json!(true)), true).await;

    let mut watcher = Watcher::start(port, "o1/#").await;
    // The retained messages first, up to the barrier.
    watcher.barrier("o1/zz-barrier").await;
    let bridge = Bridge::start(
        &support::broker(port, "o1"),
        &[
            support::device("dev", device.address),
            support::device("off", support::refused_address()),
        ],
        support::options(),
    )
    .unwrap();

    // Every orphan cleared, and both devices reported in, in whatever order.
    let mut missing: BTreeSet<&str> = orphans.into_iter().collect();
    let (mut off_reported, mut dev_reported) = (false, false);
    let seen = watcher
        .until(|m| {
            if m.payload.is_empty() {
                missing.remove(m.topic.as_str());
            } else if m.topic == "o1/status/off/online" {
                off_reported |= m.val() == json!(false);
            } else if m.topic == "o1/status/dev/online" {
                dev_reported |= m.val() == json!(true);
            }
            missing.is_empty() && off_reported && dev_reported
        })
        .await;
    tokio::time::timeout(support::WAIT, bridge.mqtt().ready().wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    let mut cleared: Vec<String> = seen
        .iter()
        .chain(&watcher.barrier("o1/zz-barrier").await)
        .filter(|m| m.payload.is_empty())
        .map(|m| m.topic.clone())
        .collect();
    cleared.sort();
    let mut expected: Vec<String> = orphans.iter().map(|t| (*t).to_owned()).collect();
    expected.sort();
    assert_eq!(cleared, expected, "exactly the orphans are cleared");

    // The device that is reachable is complete; the one that is not keeps its last values.
    let left = retained(port, "o1/#", "o1/zz-barrier").await;
    for topic in kept_offline {
        assert!(left.contains(topic), "{topic} kept: {left:?}");
    }
    for orphan in orphans {
        assert!(!left.contains(orphan), "{orphan} cleared: {left:?}");
    }
    assert!(left.contains("o1/status/dev/brightness"));
    assert_eq!(
        retained(port, "o2/#", "o2/zz-barrier").await,
        BTreeSet::from(["o2/status/gone/on".to_owned()])
    );
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphans_left_while_the_broker_was_away_are_cleared_on_reconnect() {
    let port = support::start_broker();
    let proxy = Proxy::start(port).await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let mut watcher = Watcher::start(port, "o3/#").await;
    let bridge = Bridge::start(
        &support::broker(proxy.port, "o3"),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    watcher.until_val("o3/status/dev/online", json!(true)).await;
    tokio::time::timeout(support::WAIT, bridge.mqtt().ready().wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();

    proxy.cut().await;
    watcher.until_text("o3/connected", "0").await;
    // Left by someone else meanwhile, e.g. an older version started by mistake.
    support::publish(port, "o3/status/dev/reachable", &status(json!(true)), true).await;
    proxy.open().await;
    watcher
        .until(|m| m.topic == "o3/status/dev/reachable" && m.payload.is_empty())
        .await;
    tokio::time::timeout(support::WAIT, bridge.mqtt().ready().wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    let left = retained(port, "o3/#", "o3/zz-barrier").await;
    assert!(!left.contains("o3/status/dev/reachable"), "{left:?}");
    assert!(left.contains("o3/status/dev/on"), "{left:?}");
    bridge.shutdown().await;
    proxy.cut().await;
}
