//! Home Assistant discovery while the bridge runs: switched on and off, devices started and
//! stopped, the broker switched or briefly unreachable, and what a previous run left behind
//! (version 1 messages migrated, messages of devices gone cleared).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeSet;

use nanoleaf2mqtt::bridge::{Bridge, BridgeHandle, BridgeSetup};
use nanoleaf2mqtt::model::{BrokerConfig, ConfigChange, DeviceId};
use serde_json::{Value, json};
use support::{FakeDevice, Msg, Proxy, Watcher};

const HA: &str = "homeassistant";

/// The discovery topic of a device.
fn discovery_topic(id: &str) -> String {
    format!("{HA}/device/nanoleaf2mqtt_{id}/config")
}

/// The version 1 discovery topics of a device.
fn legacy_topics(id: &str) -> Vec<String> {
    ["light/{n}/light", "event/{n}/touch", "button/{n}/identify"]
        .iter()
        .map(|t| {
            format!(
                "{HA}/{}/config",
                t.replace("{n}", &format!("nanoleaf2mqtt_{id}"))
            )
        })
        .collect()
}

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

/// Waits until the discovery topic of `id` was published (non-empty) and returns its payload.
async fn until_announced(watcher: &mut Watcher, id: &str) -> Value {
    let topic = discovery_topic(id);
    let seen = watcher
        .until(|m| m.topic == topic && !m.payload.is_empty())
        .await;
    seen.last().unwrap().json()
}

/// Waits for an empty message (a retained clear; the broker forwards it to live subscribers
/// without the retain flag) on every topic in `topics`.
async fn until_all_cleared(watcher: &mut Watcher, topics: &[String]) -> Vec<Msg> {
    let mut missing: BTreeSet<String> = topics.iter().cloned().collect();
    watcher
        .until(|m| {
            if m.payload.is_empty() {
                missing.remove(&m.topic);
            }
            missing.is_empty()
        })
        .await
}

async fn until_cleared(watcher: &mut Watcher, id: &str) -> Vec<Msg> {
    until_all_cleared(watcher, &[discovery_topic(id)]).await
}

/// What a fresh subscriber gets retained (non-empty) below the discovery prefix.
async fn retained_discovery_messages(port: u16) -> Vec<Msg> {
    let mut late = Watcher::start(port, &format!("{HA}/#")).await;
    let seen = late.barrier(&format!("{HA}/zz-barrier")).await;
    late.stop().await;
    seen.into_iter().filter(|m| !m.payload.is_empty()).collect()
}

async fn retained_discovery(port: u16) -> BTreeSet<String> {
    retained_discovery_messages(port)
        .await
        .into_iter()
        .map(|m| m.topic)
        .collect()
}

fn announced(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| discovery_topic(id)).collect()
}

/// A bridge following `devices` on a fresh broker, once its session is set up.
async fn started(
    prefix: &str,
    devices: &[(&str, &FakeDevice)],
    ha_discovery: bool,
) -> (u16, Watcher, Bridge, BridgeHandle) {
    let port = support::start_broker();
    let watcher = Watcher::start(port, "#").await;
    let (bridge, handle) = launch(support::broker(port, prefix), devices, ha_discovery);
    tokio::time::timeout(support::WAIT, bridge.mqtt().ready().wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    (port, watcher, bridge, handle)
}

fn launch(
    broker: BrokerConfig,
    devices: &[(&str, &FakeDevice)],
    ha_discovery: bool,
) -> (Bridge, BridgeHandle) {
    let bridge = Bridge::launch(BridgeSetup {
        broker: Some(broker),
        devices: devices
            .iter()
            .map(|(id, device)| support::device(id, device.address))
            .collect(),
        ha_discovery,
        options: support::options(),
        ..BridgeSetup::default()
    });
    let handle = bridge.handle();
    (bridge, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_follows_the_switch_at_runtime() {
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let (port, mut watcher, bridge, handle) = started("h1", &[("dev", &device)], false).await;
    watcher.until_val("h1/status/dev/online", json!(true)).await;
    // Off: nothing below the discovery prefix.
    let seen = watcher.barrier("zz/barrier").await;
    assert!(
        seen.iter().all(|m| !m.topic.starts_with(HA)),
        "nothing announced while off: {seen:?}"
    );

    handle.apply(ConfigChange::HaDiscovery(true)).await.unwrap();
    let payload = until_announced(&mut watcher, "dev").await;
    let light = &payload["components"]["light"];
    assert_eq!(light["p"], "light");
    assert_eq!(light["unique_id"], "nanoleaf2mqtt_dev_light");
    assert_eq!(light["state_topic"], "h1/status/dev/on");
    assert_eq!(light["hs_command_topic"], "h1/set/dev/hs");
    assert_eq!(
        light["effect_list"],
        json!(["Blaze", "Prism", "Northern Lights"])
    );
    assert_eq!(
        (light["min_kelvin"].clone(), light["max_kelvin"].clone()),
        (json!(1200), json!(6500))
    );
    assert_eq!(
        payload["device"],
        json!({
            "identifiers": ["nanoleaf2mqtt_dev"],
            "name": "Test Shapes",
            "manufacturer": "Nanoleaf",
            "model": "NL42",
            "sw_version": "12.0.3"
        })
    );
    let touch = &payload["components"]["touch"];
    assert_eq!(touch["p"], "event");
    assert_eq!(touch["state_topic"], "h1/status/dev/touch");
    let identify = &payload["components"]["identify"];
    assert_eq!(identify["p"], "button");
    assert_eq!(identify["command_topic"], "h1/set/dev/identify");
    // The device is online before the message names its `online` topic.
    assert_eq!(payload["availability"][1]["topic"], "h1/status/dev/online");
    // Nothing about how the device or the broker is reached.
    let all = payload.to_string();
    for secret in [
        support::TOKEN,
        "127.0.0.1",
        &device.address.port().to_string(),
    ] {
        assert!(!all.contains(secret), "{secret} in {all}");
    }
    assert_eq!(retained_discovery(port).await, announced(&["dev"]));

    // The same setting again changes nothing.
    watcher.barrier("zz/barrier").await;
    handle.apply(ConfigChange::HaDiscovery(true)).await.unwrap();
    // Changes are applied in order: once the next one shows, this one was applied.
    handle
        .apply(ConfigChange::HaDiscovery(false))
        .await
        .unwrap();
    let seen = until_cleared(&mut watcher, "dev").await;
    assert!(
        seen.iter()
            .all(|m| !m.topic.starts_with(HA) || m.payload.is_empty()),
        "no republish for an unchanged setting: {seen:?}"
    );
    assert_eq!(retained_discovery(port).await, BTreeSet::new());

    // On again: back at once, without waiting for the device.
    handle.apply(ConfigChange::HaDiscovery(true)).await.unwrap();
    until_announced(&mut watcher, "dev").await;
    handle
        .apply(ConfigChange::HaDiscovery(false))
        .await
        .unwrap();
    until_cleared(&mut watcher, "dev").await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn devices_started_and_stopped_are_announced_and_cleared() {
    let keep = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let added = FakeDevice::start(support::root(true, 50, "Blaze")).await;
    let (port, mut watcher, bridge, handle) = started("h2", &[("keep", &keep)], true).await;
    until_announced(&mut watcher, "keep").await;

    // Started: announced once it has reported.
    handle.apply(run("added", added.address)).await.unwrap();
    until_announced(&mut watcher, "added").await;
    assert_eq!(
        retained_discovery(port).await,
        announced(&["keep", "added"])
    );

    // A new effect list (read back after a command) is announced again.
    let mut root = support::root(true, 50, "Blaze");
    root["effects"]["effectsList"] = json!(["Blaze", "New One"]);
    added.set_root(root);
    support::publish(port, "h2/set/added/on", b"true", false).await;
    added.next_command().await;
    let topic = discovery_topic("added");
    let seen = watcher
        .until(|m| {
            m.topic == topic
                && !m.payload.is_empty()
                && m.json()["components"]["light"]["effect_list"] == json!(["Blaze", "New One"])
        })
        .await;
    assert!(
        seen.iter().all(|m| !m.topic.contains("nanoleaf2mqtt_keep")),
        "the other device is not announced again: {seen:?}"
    );

    // Stopped: its discovery topic is cleared, the other device's stays.
    handle.apply(stop("added")).await.unwrap();
    until_cleared(&mut watcher, "added").await;
    assert_eq!(retained_discovery(port).await, announced(&["keep"]));
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_follows_a_broker_switch() {
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let (_old_port, mut old, bridge, handle) = started("h3", &[("dev", &device)], true).await;
    until_announced(&mut old, "dev").await;

    let new_port = support::start_broker();
    let mut new = Watcher::start(new_port, "#").await;
    handle
        .apply(ConfigChange::Broker(Some(support::broker(new_port, "h4"))))
        .await
        .unwrap();
    // Republished on the new broker, pointing at the new prefix.
    let payload = until_announced(&mut new, "dev").await;
    assert_eq!(payload["availability"][0]["topic"], "h4/connected");
    assert_eq!(payload["availability"][1]["topic"], "h4/status/dev/online");
    assert_eq!(
        payload["components"]["light"]["state_topic"],
        "h4/status/dev/on"
    );
    assert_eq!(retained_discovery(new_port).await, announced(&["dev"]));
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switched_off_while_the_broker_is_unreachable_is_cleared_on_reconnect() {
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let port = support::start_broker();
    let proxy = Proxy::start(port).await;
    let mut watcher = Watcher::start(port, "#").await;
    let (bridge, handle) = launch(support::broker(proxy.port, "h5"), &[("dev", &device)], true);
    until_announced(&mut watcher, "dev").await;

    proxy.cut().await;
    // The session is gone before the switch, so the clear cannot go out directly.
    tokio::time::timeout(support::WAIT, async {
        while handle.status().broker_connected {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session lost");
    handle
        .apply(ConfigChange::HaDiscovery(false))
        .await
        .unwrap();
    tokio::time::timeout(support::WAIT, async {
        while bridge.mqtt().ha_discovery().is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("switched off");
    assert!(!handle.status().broker_connected, "still disconnected");
    proxy.open().await;
    until_cleared(&mut watcher, "dev").await;
    assert_eq!(retained_discovery(port).await, BTreeSet::new());
    bridge.shutdown().await;
}

/// A version 1 message as an instance under `prefix` published it (shortened to what matters
/// here: the availability that tells whose it is).
fn legacy_payload(prefix: &str, id: &str, key: &str) -> Vec<u8> {
    json!({
        "unique_id": format!("nanoleaf2mqtt_{id}_{key}"),
        "availability": [
            {"topic": format!("{prefix}/connected"), "value_template": "{{ value }}"},
            {"topic": format!("{prefix}/status/{id}/reachable")},
        ],
    })
    .to_string()
    .into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_previous_run_is_migrated_and_cleaned_up() {
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let port = support::start_broker();
    // Left by version 1 of this instance (prefix h6) for the configured device and for one
    // removed while the service was down; by a version 2 run for another removed one; and by
    // others: another instance on the same broker (prefix h7) and another service.
    let mut left: Vec<(String, Vec<u8>)> = Vec::new();
    for (id, prefix) in [("dev", "h6"), ("gone", "h6"), ("theirs", "h7")] {
        for (topic, key) in legacy_topics(id)
            .into_iter()
            .zip(["light", "touch", "identify"])
        {
            left.push((topic, legacy_payload(prefix, id, key)));
        }
    }
    let device_config = |prefix: &str| {
        json!({"availability": [{"topic": format!("{prefix}/connected")}]})
            .to_string()
            .into_bytes()
    };
    left.push((discovery_topic("old"), device_config("h6")));
    left.push((discovery_topic("other"), device_config("h7")));
    let foreign = format!("{HA}/light/zigbee2mqtt_lamp/light/config");
    left.push((foreign.clone(), br#"{"unique_id":"lamp"}"#.to_vec()));
    for (topic, payload) in &left {
        support::publish(port, topic, payload, true).await;
    }

    let mut watcher = Watcher::start(port, &format!("{HA}/#")).await;
    // The retained messages first, up to the barrier.
    watcher.barrier(&format!("{HA}/zz-barrier")).await;
    let (bridge, _handle) = launch(support::broker(port, "h6"), &[("dev", &device)], true);

    // The configured device: Home Assistant's migration request on every version 1 topic, then
    // the device-based message, then the version 1 topics cleared, in this order.
    let legacy = legacy_topics("dev");
    let seen = until_all_cleared(&mut watcher, &legacy).await;
    let ours: Vec<&Msg> = seen
        .iter()
        .filter(|m| legacy.contains(&m.topic) || m.topic == discovery_topic("dev"))
        .collect();
    let steps: Vec<String> = ours
        .iter()
        .map(|m| {
            let what = if m.payload.is_empty() {
                "clear".to_owned()
            } else if m.topic == discovery_topic("dev") {
                "device".to_owned()
            } else {
                m.text()
            };
            format!("{} {what}", m.topic)
        })
        .collect();
    let mut sorted = legacy.clone();
    sorted.sort();
    let mut expected: Vec<String> = sorted
        .iter()
        .map(|t| format!("{t} {{\"migrate_discovery\":true}}"))
        .collect();
    expected.push(format!("{} device", discovery_topic("dev")));
    expected.extend(sorted.iter().map(|t| format!("{t} clear")));
    assert_eq!(steps, expected);
    let payload = ours
        .iter()
        .find(|m| m.topic == discovery_topic("dev"))
        .unwrap()
        .json();
    let unique: Vec<&str> = ["light", "touch", "identify"]
        .iter()
        .map(|k| payload["components"][k]["unique_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        unique,
        [
            "nanoleaf2mqtt_dev_light",
            "nanoleaf2mqtt_dev_touch",
            "nanoleaf2mqtt_dev_identify"
        ],
        "the entities keep their unique ids"
    );

    // What is left: this instance's current message and the others' untouched.
    tokio::time::timeout(support::WAIT, bridge.mqtt().ready().wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    let mut expected: BTreeSet<String> = legacy_topics("theirs").into_iter().collect();
    expected.extend([discovery_topic("dev"), discovery_topic("other"), foreign]);
    assert_eq!(retained_discovery(port).await, expected);
    bridge.shutdown().await;
}
