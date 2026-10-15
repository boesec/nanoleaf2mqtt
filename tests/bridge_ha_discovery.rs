//! Home Assistant discovery while the bridge runs: switched on and off, devices started and
//! stopped, the broker switched or briefly unreachable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeSet;

use nanoleaf2mqtt::bridge::{Bridge, BridgeHandle, BridgeSetup};
use nanoleaf2mqtt::model::{BrokerConfig, ConfigChange, DeviceId};
use serde_json::{Value, json};
use support::{FakeDevice, Msg, Proxy, Watcher};

const HA: &str = "homeassistant";

/// The discovery topics of a device.
fn discovery_topics(id: &str) -> Vec<String> {
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

/// Waits until every discovery topic of `id` was published (non-empty) and returns the last
/// payload per topic, in the order of [`discovery_topics`].
async fn until_announced(watcher: &mut Watcher, id: &str) -> Vec<Value> {
    let wanted = discovery_topics(id);
    let mut missing: BTreeSet<String> = wanted.iter().cloned().collect();
    let seen = watcher
        .until(|m| {
            if !m.payload.is_empty() {
                missing.remove(&m.topic);
            }
            missing.is_empty()
        })
        .await;
    wanted
        .iter()
        .map(|topic| {
            seen.iter()
                .rev()
                .find(|m| &m.topic == topic && !m.payload.is_empty())
                .unwrap()
                .json()
        })
        .collect()
}

/// Waits for an empty message (a retained clear; the broker forwards it to live subscribers
/// without the retain flag) on every discovery topic of `id`.
async fn until_cleared(watcher: &mut Watcher, id: &str) -> Vec<Msg> {
    let mut missing: BTreeSet<String> = discovery_topics(id).into_iter().collect();
    watcher
        .until(|m| {
            if m.payload.is_empty() {
                missing.remove(&m.topic);
            }
            missing.is_empty()
        })
        .await
}

/// The topics a fresh subscriber gets retained (non-empty) below the discovery prefix.
async fn retained_discovery(port: u16) -> BTreeSet<String> {
    let mut late = Watcher::start(port, &format!("{HA}/#")).await;
    let seen = late.barrier(&format!("{HA}/zz-barrier")).await;
    late.stop().await;
    seen.into_iter()
        .filter(|m| !m.payload.is_empty())
        .map(|m| m.topic)
        .collect()
}

fn announced(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().flat_map(|id| discovery_topics(id)).collect()
}

/// A bridge following `devices` on a fresh broker, once every device is reachable.
async fn started(
    prefix: &str,
    devices: &[(&str, &FakeDevice)],
    ha_discovery: bool,
) -> (u16, Watcher, Bridge, BridgeHandle) {
    let port = support::start_broker();
    let watcher = Watcher::start(port, "#").await;
    let (bridge, handle) = launch(support::broker(port, prefix), devices, ha_discovery);
    let mut connected = Watcher::start(port, &format!("{prefix}/connected")).await;
    connected
        .until_text(&format!("{prefix}/connected"), "2")
        .await;
    connected.stop().await;
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
    // Off: nothing below the discovery prefix.
    let seen = watcher.barrier("zz/barrier").await;
    assert!(
        seen.iter().all(|m| !m.topic.starts_with(HA)),
        "nothing announced while off: {seen:?}"
    );

    handle.apply(ConfigChange::HaDiscovery(true)).await.unwrap();
    let payloads = until_announced(&mut watcher, "dev").await;
    let light = &payloads[0];
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
        light["device"],
        json!({
            "identifiers": ["nanoleaf2mqtt_dev"],
            "name": "Test Shapes",
            "manufacturer": "Nanoleaf",
            "model": "NL42",
            "sw_version": "12.0.3"
        })
    );
    assert_eq!(payloads[1]["state_topic"], "h1/status/dev/touch");
    assert_eq!(payloads[2]["command_topic"], "h1/set/dev/identify");
    // Nothing about how the device or the broker is reached.
    let all = format!("{payloads:?}");
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
    let light = discovery_topics("added").remove(0);
    let seen = watcher
        .until(|m| {
            m.topic == light
                && !m.payload.is_empty()
                && m.json()["effect_list"] == json!(["Blaze", "New One"])
        })
        .await;
    assert!(
        seen.iter().all(|m| !m.topic.contains("nanoleaf2mqtt_keep")),
        "the other device is not announced again: {seen:?}"
    );

    // Stopped: its discovery topics are cleared, the other device's stay.
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
    let payloads = until_announced(&mut new, "dev").await;
    for payload in &payloads {
        assert_eq!(payload["availability"][0]["topic"], "h4/connected");
        assert_eq!(
            payload["availability"][1]["topic"],
            "h4/status/dev/reachable"
        );
    }
    assert_eq!(payloads[0]["state_topic"], "h4/status/dev/on");
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
