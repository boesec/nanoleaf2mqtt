//! Configuration changes while the bridge runs: devices added, changed, disabled, removed; the
//! broker switched. The other device is never interrupted.
//!
//! "Not interrupted" is checked directly: its event stream stays the one it opened at the start
//! (the fake device counts the streams opened), and its events and commands keep flowing after
//! every change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeSet;

use nanoleaf2mqtt::bridge::{Bridge, BridgeHandle, BridgeOptions, BridgeSetup};
use nanoleaf2mqtt::model::{ConfigChange, DeviceId};
use serde_json::{Value, json};
use support::{BlackHole, FakeDevice, Watcher};

const RETAINED: [&str; 11] = [
    "online",
    "on",
    "brightness",
    "hue",
    "sat",
    "hs",
    "ct",
    "colormode",
    "effect",
    "effects",
    "device",
];

fn run(id: &str, address: std::net::SocketAddr) -> ConfigChange {
    ConfigChange::Device {
        id: DeviceId::new(id).unwrap(),
        run: Some(support::device(id, address)),
    }
}

fn stop(id: &str) -> ConfigChange {
    ConfigChange::Device {
        id: DeviceId::new(id).unwrap(),
        run: None,
    }
}

/// The kept device still delivers events and executes commands, on the event stream it opened
/// at the start. Returns once the state read back after the command is published (the fake
/// device reports brightness 30 then), so no read-back is left over for the next step.
async fn kept_device_is_live(
    port: u16,
    prefix: &str,
    keep: &FakeDevice,
    watcher: &mut Watcher,
    brightness: u16,
) {
    keep.push(1, json!({"events":[{"attr":2,"value":brightness}]}));
    watcher
        .until_val(
            &format!("{prefix}/status/keep/brightness"),
            json!(brightness),
        )
        .await;
    support::publish(port, &format!("{prefix}/set/keep/hue"), b"10", false).await;
    assert_eq!(keep.next_command().await.body, json!({"hue":{"value":10}}));
    watcher
        .until_val(&format!("{prefix}/status/keep/brightness"), json!(30))
        .await;
    assert_eq!(
        keep.streams(),
        1,
        "the kept device's stream was never reopened"
    );
}

/// Waits for an empty retained message on every status topic of `id`.
async fn until_cleared(watcher: &mut Watcher, prefix: &str, id: &str) {
    let mut missing: BTreeSet<String> = RETAINED
        .iter()
        .map(|p| format!("{prefix}/status/{id}/{p}"))
        .collect();
    watcher
        .until(|m| {
            if m.payload.is_empty() {
                missing.remove(&m.topic);
            }
            missing.is_empty()
        })
        .await;
}

/// What a fresh subscriber gets retained below `filter`.
async fn retained_topics(port: u16, filter: &str, barrier: &str) -> Vec<String> {
    let mut late = Watcher::start(port, filter).await;
    let seen = late.barrier(barrier).await;
    late.stop().await;
    seen.into_iter()
        .filter(|m| !m.payload.is_empty())
        .map(|m| m.topic)
        .collect()
}

/// Broker, watcher and a bridge following `keep`, once everything of `keep` is published.
/// Also returns the `on` message seen.
async fn started(prefix: &str, keep: &FakeDevice) -> (u16, Watcher, Bridge, BridgeHandle, Value) {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, &format!("{prefix}/#")).await;
    let bridge = Bridge::start(
        &support::broker(port, prefix),
        &[support::device("keep", keep.address)],
        support::options(),
    )
    .unwrap();
    let on = format!("{prefix}/status/keep/on");
    let seen = watcher
        .until_all(&[
            (&format!("{prefix}/connected"), Some("2")),
            (&format!("{prefix}/status/keep/device"), None),
            (&on, None),
        ])
        .await;
    let on = seen.iter().find(|m| m.topic == on).unwrap().json();
    bridge.mqtt().ready().wait_for(|r| *r).await.unwrap();
    let handle = bridge.handle();
    (port, watcher, bridge, handle, on)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn devices_come_and_go_at_runtime_without_interrupting_the_other() {
    let keep = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let first = FakeDevice::start(support::root(false, 0, "Blaze")).await;
    let second = FakeDevice::start(support::root(true, 77, "Blaze")).await;
    let (port, mut watcher, bridge, handle, _) = started("r1", &keep).await;

    // Added: followed at once.
    handle.apply(run("added", first.address)).await.unwrap();
    let seen = watcher
        .until_val("r1/status/added/online", json!(true))
        .await;
    assert!(
        seen.iter().all(|m| m.topic != "r1/connected"),
        "connected stays 2 while the new device has not reported: {seen:?}"
    );
    kept_device_is_live(port, "r1", &keep, &mut watcher, 31).await;
    let status = handle.status();
    assert_eq!(
        status
            .devices
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        ["added", "keep"]
    );

    // Changed (another address): only this device is restarted.
    handle.apply(run("added", second.address)).await.unwrap();
    watcher
        .until_val("r1/status/added/brightness", json!(77))
        .await;
    assert_eq!(second.streams(), 1);
    kept_device_is_live(port, "r1", &keep, &mut watcher, 32).await;

    // The same configuration again changes nothing (checked once the next change is applied:
    // changes are applied in order).
    handle.apply(run("added", second.address)).await.unwrap();
    kept_device_is_live(port, "r1", &keep, &mut watcher, 33).await;

    // Disabled (or removed – the same message): stopped, retained topics cleared.
    handle.apply(stop("added")).await.unwrap();
    until_cleared(&mut watcher, "r1", "added").await;
    assert_eq!(second.streams(), 1, "an unchanged device is not restarted");
    kept_device_is_live(port, "r1", &keep, &mut watcher, 34).await;
    assert_eq!(
        retained_topics(port, "r1/status/added/#", "r1/status/added/zz").await,
        Vec::<String>::new()
    );
    // Commands for it go nowhere; the kept device still gets its own.
    support::publish(port, "r1/set/added/on", b"on", false).await;
    kept_device_is_live(port, "r1", &keep, &mut watcher, 35).await;
    assert!(second.pending_commands().await.is_empty());
    assert!(
        handle
            .status()
            .device(&DeviceId::new("added").unwrap())
            .is_none()
    );

    // Enabled again, then removed for good.
    handle.apply(run("added", second.address)).await.unwrap();
    watcher
        .until_val("r1/status/added/online", json!(true))
        .await;
    handle.apply(stop("added")).await.unwrap();
    until_cleared(&mut watcher, "r1", "added").await;
    // The bridge is still up.
    let mut late = Watcher::start(port, "r1/connected").await;
    late.until_text("r1/connected", "2").await;
    late.stop().await;
    kept_device_is_live(port, "r1", &keep, &mut watcher, 36).await;

    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanging_device_is_stopped_without_waiting_for_it() {
    let keep = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let hanging = BlackHole::start().await;
    let late = FakeDevice::start(support::root(true, 50, "Prism")).await;
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "r2/#").await;
    // Without request timeout the hanging device's requests never end.
    let bridge = Bridge::start(
        &support::broker(port, "r2"),
        &[
            support::device("keep", keep.address),
            support::device("hang", hanging.address),
        ],
        BridgeOptions {
            request_timeout: None,
            ..support::options()
        },
    )
    .unwrap();
    watcher
        .until_val("r2/status/keep/online", json!(true))
        .await;
    bridge.mqtt().ready().wait_for(|r| *r).await.unwrap();
    let handle = bridge.handle();

    // Stopping the hanging device and adding another one go through at once.
    handle.apply(stop("hang")).await.unwrap();
    handle.apply(run("late", late.address)).await.unwrap();
    watcher
        .until_val("r2/status/late/online", json!(true))
        .await;
    kept_device_is_live(port, "r2", &keep, &mut watcher, 40).await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_switch_moves_everything_and_says_goodbye_on_the_old_one() {
    let keep = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let (old_port, mut old, bridge, handle, old_on) = started("s1", &keep).await;

    let new_port = support::start_broker();
    let mut new = Watcher::start(new_port, "s2/#").await;
    handle
        .apply(ConfigChange::Broker(Some(support::broker(new_port, "s2"))))
        .await
        .unwrap();

    // Goodbye on the old broker ...
    old.until_text("s1/connected", "0").await;
    // ... everything on the new one, `lc` kept.
    let mut wanted: Vec<(String, Option<&str>)> = RETAINED
        .iter()
        .map(|p| (format!("s2/status/keep/{p}"), None))
        .collect();
    wanted.push(("s2/connected".to_owned(), Some("2")));
    wanted.push(("s2/info".to_owned(), None));
    let wanted_refs: Vec<(&str, Option<&str>)> =
        wanted.iter().map(|(t, x)| (t.as_str(), *x)).collect();
    let seen = new.until_all(&wanted_refs).await;
    let new_on = seen
        .iter()
        .find(|m| m.topic == "s2/status/keep/on")
        .unwrap()
        .json();
    assert_eq!(new_on["lc"], old_on["lc"], "lc survives the switch");
    assert_eq!(new_on["val"], old_on["val"]);
    bridge.mqtt().ready().wait_for(|r| *r).await.unwrap();

    // The device was not touched, and talks to the new broker only.
    kept_device_is_live(new_port, "s2", &keep, &mut new, 55).await;
    let after = old.barrier("s1/zz-barrier").await;
    assert!(
        after.is_empty(),
        "nothing more on the old broker: {after:?}"
    );
    // The read-back after the command reports 29: seeing it means no read-back is pending.
    keep.set_root(support::root(true, 29, "Prism"));
    support::publish(old_port, "s1/set/keep/on", b"off", false).await;
    support::publish(new_port, "s2/set/keep/on", b"on", false).await;
    assert_eq!(
        keep.next_command().await.body,
        json!({"on":{"value":true}}),
        "only the new broker's command arrives"
    );
    new.until_val("s2/status/keep/brightness", json!(29)).await;

    // Removing the broker says goodbye as well; values are still followed.
    handle.apply(ConfigChange::Broker(None)).await.unwrap();
    new.until_text("s2/connected", "0").await;
    assert!(!handle.status().broker_connected);
    keep.push(1, json!({"events":[{"attr":2,"value":66}]}));
    // Back to the old broker: the value read meanwhile is published.
    handle
        .apply(ConfigChange::Broker(Some(support::broker(old_port, "s1"))))
        .await
        .unwrap();
    old.until_val("s1/status/keep/brightness", json!(66)).await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_broker_nothing_is_published_until_one_is_configured() {
    let device = FakeDevice::start(support::root(true, 42, "Prism")).await;
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "#").await;
    let bridge = Bridge::launch(BridgeSetup {
        broker: None,
        devices: vec![support::device("dev", device.address)],
        options: support::options(),
        ..BridgeSetup::default()
    });
    let handle = bridge.handle();
    // The device is followed: its values are known before any broker is.
    tokio::time::timeout(support::WAIT, async {
        while handle
            .status()
            .device(&DeviceId::new("dev").unwrap())
            .and_then(|d| d.reachable)
            != Some(true)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("device followed without a broker");
    let quiet = watcher.barrier("zz/barrier").await;
    assert!(quiet.is_empty(), "nothing published: {quiet:?}");

    handle
        .apply(ConfigChange::Broker(Some(support::broker(port, "n1"))))
        .await
        .unwrap();
    let seen = watcher
        .until_all(&[
            ("n1/connected", Some("2")),
            ("n1/status/dev/brightness", None),
        ])
        .await;
    let brightness = seen
        .iter()
        .find(|m| m.topic == "n1/status/dev/brightness")
        .unwrap();
    assert_eq!(brightness.val(), Value::from(42));
    bridge.shutdown().await;
}
