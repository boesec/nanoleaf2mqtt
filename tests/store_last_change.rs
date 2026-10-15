//! `lc` is persisted in the store (in batches) and survives a restart of the bridge.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::time::Duration;

use nanoleaf2mqtt::bridge::{Bridge, BridgeOptions, BridgeSetup};
use nanoleaf2mqtt::model::{BrokerConfig, DeviceId};
use nanoleaf2mqtt::secret::Secret;
use nanoleaf2mqtt::store::{NewDevice, Store};
use serde_json::{Value, json};
use support::{FakeDevice, Watcher};

async fn store_with_device(device: &FakeDevice) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store
        .insert_device(&NewDevice {
            id: DeviceId::new("dev").unwrap(),
            name: "Test".to_owned(),
            host: device.address.ip().to_string(),
            port: device.address.port(),
            token: Some(Secret::new(support::TOKEN.to_owned())),
            enabled: true,
        })
        .await
        .unwrap();
    (dir, store)
}

fn launch(
    broker: &BrokerConfig,
    device: &FakeDevice,
    store: &Store,
    options: BridgeOptions,
) -> Bridge {
    Bridge::launch(BridgeSetup {
        broker: Some(broker.clone()),
        devices: vec![support::device("dev", device.address)],
        ha_discovery: false,
        store: Some(store.clone()),
        options,
    })
}

/// The `{val, ts, lc}` payload of the first message on `topic` published after `after` (ms).
async fn next_payload(watcher: &mut Watcher, topic: &str, after: i64) -> Value {
    let seen = watcher
        .until(|m| {
            m.topic == topic && !m.payload.is_empty() && m.json()["ts"].as_i64().unwrap() > after
        })
        .await;
    seen.last().unwrap().json()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lc_survives_a_restart() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "lc1/#").await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let (_dir, store) = store_with_device(&device).await;
    let broker = support::broker(port, "lc1");

    let bridge = launch(&broker, &device, &store, support::options());
    let on = next_payload(&mut watcher, "lc1/status/dev/on", 0).await;
    let brightness = next_payload(&mut watcher, "lc1/status/dev/brightness", 0).await;
    let first_lc = on["lc"].as_i64().unwrap();
    bridge.shutdown().await;

    // Written at shutdown at the latest.
    let stored = store
        .last_changes_of(&DeviceId::new("dev").unwrap())
        .await
        .unwrap();
    let on_record = stored.iter().find(|r| r.path == "on").unwrap();
    assert_eq!((on_record.val.as_str(), on_record.lc), ("true", first_lc));

    // Time passes; the brightness changes while the bridge is down.
    tokio::time::sleep(Duration::from_millis(20)).await;
    device.set_root(support::root(true, 45, "Prism"));
    let restarted_at = chrono::Utc::now().timestamp_millis();
    let bridge = launch(&broker, &device, &store, support::options());
    let on_again = next_payload(&mut watcher, "lc1/status/dev/on", restarted_at - 1).await;
    let brightness_again =
        next_payload(&mut watcher, "lc1/status/dev/brightness", restarted_at - 1).await;
    // Unchanged value: new ts, the old lc.
    assert!(on_again["ts"].as_i64().unwrap() > first_lc);
    assert_eq!(on_again["lc"], json!(first_lc), "{on_again}");
    // Changed value: lc is now.
    assert_eq!(brightness_again["val"], json!(45));
    assert!(
        brightness_again["lc"].as_i64().unwrap() > brightness["lc"].as_i64().unwrap(),
        "{brightness} -> {brightness_again}"
    );
    assert_eq!(brightness_again["lc"], brightness_again["ts"]);
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lc_is_written_in_batches_while_running() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "lc2/#").await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let (_dir, store) = store_with_device(&device).await;
    let options = BridgeOptions {
        lc_flush: Duration::from_millis(50),
        ..support::options()
    };
    let bridge = launch(&support::broker(port, "lc2"), &device, &store, options);
    watcher
        .until_val("lc2/status/dev/reachable", json!(true))
        .await;
    device.push(1, json!({"events":[{"attr":2,"value":61}]}));
    let published = watcher
        .until_val("lc2/status/dev/brightness", json!(61))
        .await;
    let lc = published.last().unwrap().json()["lc"].as_i64().unwrap();

    // Without a shutdown: the periodic flush writes it.
    let id = DeviceId::new("dev").unwrap();
    tokio::time::timeout(support::WAIT, async {
        loop {
            let stored = store.last_changes_of(&id).await.unwrap();
            if stored
                .iter()
                .any(|r| r.path == "brightness" && r.val == "61" && r.lc == lc)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("lc persisted by the periodic flush");
    bridge.shutdown().await;
}
