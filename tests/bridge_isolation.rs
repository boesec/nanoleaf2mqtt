//! One failing device never blocks another.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::sync::atomic::Ordering;

use nanoleaf2mqtt::bridge::{Bridge, BridgeOptions};
use serde_json::json;
use support::{BlackHole, FakeDevice, Watcher};

async fn check_good_device_unaffected(
    port: u16,
    prefix: &str,
    good: &FakeDevice,
    watcher: &mut Watcher,
) {
    // State flows ...
    good.push(1, json!({"events":[{"attr":2,"value":66}]}));
    watcher
        .until_val(&format!("{prefix}/status/good/brightness"), json!(66))
        .await;
    // ... and commands too, even right after one for the failing device.
    support::publish(port, &format!("{prefix}/set/bad/on"), b"true", false).await;
    support::publish(port, &format!("{prefix}/set/good/hue"), b"10", false).await;
    let request = good.next_command().await;
    assert_eq!(request.body, json!({"hue":{"value":10}}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_device_does_not_stop_the_other() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "i1/#").await;
    let good = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let bridge = Bridge::start(
        &support::broker(port, "i1"),
        &[
            support::device("bad", support::refused_address()),
            support::device("good", good.address),
        ],
        support::options(),
    )
    .unwrap();

    // The two devices report in either order.
    let (mut good_up, mut bad_down) = (false, false);
    let all = watcher
        .until(|m| {
            good_up |= m.topic == "i1/status/good/reachable" && m.val() == json!(true);
            bad_down |= m.topic == "i1/status/bad/reachable" && m.val() == json!(false);
            good_up && bad_down
        })
        .await;
    assert!(
        all.iter().any(|m| m.topic == "i1/status/good/brightness"),
        "the good device's state is published"
    );
    assert!(
        all.iter()
            .all(|m| m.topic != "i1/connected" || m.text() == "1"),
        "connected never 2 with one device down"
    );
    let mut ready = bridge.mqtt().ready();
    ready.wait_for(|r| *r).await.unwrap();

    check_good_device_unaffected(port, "i1", &good, &mut watcher).await;

    let later = watcher.barrier("i1/zz-barrier").await;
    assert!(later.iter().all(|m| m.topic != "i1/connected"), "{later:?}");
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanging_device_does_not_block_the_other() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "i2/#").await;
    let hanging = BlackHole::start().await;
    let good = FakeDevice::start(support::root(true, 30, "Prism")).await;
    // Without request timeout the hanging device's requests never end.
    let options = BridgeOptions {
        request_timeout: None,
        ..support::options()
    };
    let bridge = Bridge::start(
        &support::broker(port, "i2"),
        &[
            support::device("bad", hanging.address),
            support::device("good", good.address),
        ],
        options,
    )
    .unwrap();

    watcher
        .until_val("i2/status/good/reachable", json!(true))
        .await;
    let mut ready = bridge.mqtt().ready();
    ready.wait_for(|r| *r).await.unwrap();

    check_good_device_unaffected(port, "i2", &good, &mut watcher).await;
    // The hanging device got its requests (event stream; maybe the command) and answered none.
    assert!(hanging.requests.load(Ordering::SeqCst) >= 1);

    let later = watcher.barrier("i2/zz-barrier").await;
    assert!(
        later.iter().all(|m| !m.topic.starts_with("i2/status/bad/")),
        "nothing known about the hanging device: {later:?}"
    );
    bridge.shutdown().await;
}
