//! Home Assistant discovery on the broker connection itself: the discovery prefix, its change,
//! an unusable prefix, and devices forgotten.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use nanoleaf2mqtt::model::{DeviceId, DeviceInfo, DeviceSnapshot};
use nanoleaf2mqtt::mqtt::{HaEntity, Mqtt, ha_discovery};
use support::Watcher;

fn snapshot() -> DeviceSnapshot {
    DeviceSnapshot {
        info: DeviceInfo {
            model: Some("NL42".to_owned()),
            ..DeviceInfo::default()
        },
        ..DeviceSnapshot::default()
    }
}

fn topics(prefix: &str, id: &DeviceId) -> Vec<String> {
    HaEntity::ALL
        .into_iter()
        .map(|entity| ha_discovery(prefix, id, entity))
        .collect()
}

/// Waits until every topic in `wanted` was seen, empty (`cleared`) or not.
async fn until_each(watcher: &mut Watcher, wanted: &[String], cleared: bool) {
    let mut missing: Vec<String> = wanted.to_vec();
    watcher
        .until(|m| {
            if m.payload.is_empty() == cleared {
                missing.retain(|t| *t != m.topic);
            }
            missing.is_empty()
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_discovery_prefix_moves_the_messages() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "#").await;
    let (mqtt, _commands, _task) =
        Mqtt::spawn(&support::broker(port, "d1"), support::options().mqtt).unwrap();
    mqtt.ready().wait_for(|r| *r).await.unwrap();
    let id = DeviceId::new("dev").unwrap();

    // Not reported yet: nothing to announce.
    mqtt.set_ha_discovery(Some("ha1")).unwrap();
    let quiet = watcher.barrier("zz/barrier").await;
    assert!(
        quiet.iter().all(|m| !m.topic.starts_with("ha1/")),
        "{quiet:?}"
    );
    mqtt.describe_device(&id, &snapshot());
    until_each(&mut watcher, &topics("ha1", &id), false).await;
    // The same snapshot again announces nothing.
    mqtt.describe_device(&id, &snapshot());
    let quiet = watcher.barrier("zz/barrier").await;
    assert!(quiet.is_empty(), "{quiet:?}");

    // Another prefix: cleared under the old one, announced under the new one.
    mqtt.set_ha_discovery(Some("ha2")).unwrap();
    until_each(&mut watcher, &topics("ha1", &id), true).await;
    until_each(&mut watcher, &topics("ha2", &id), false).await;

    // An unusable prefix is refused and changes nothing.
    assert!(mqtt.set_ha_discovery(Some("ha/#")).is_err());
    assert!(mqtt.set_ha_discovery(Some("")).is_err());
    assert_eq!(mqtt.ha_discovery().as_deref(), Some("ha2"));

    // A device forgotten is cleared.
    mqtt.clear_device(&id);
    until_each(&mut watcher, &topics("ha2", &id), true).await;
    mqtt.shutdown().await;
}
