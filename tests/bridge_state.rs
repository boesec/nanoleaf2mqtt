//! Device state on MQTT: initial publish, event updates, reachability and republish.
//!
//! Real time: the broker runs on its own threads, so tokio's paused clock cannot be used. Every
//! wait is for the message it asserts; the wait limit only turns a hang into a failure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::collections::BTreeMap;

use nanoleaf2mqtt::bridge::Bridge;
use serde_json::{Value, json};
use support::{FakeDevice, Msg, Watcher};

const STATUS: [&str; 11] = [
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

fn topic(prefix: &str, path: &str) -> String {
    format!("{prefix}/status/dev/{path}")
}

/// Starts broker, device and bridge and waits until the bridge reports the device reachable.
async fn running(prefix: &str) -> (u16, FakeDevice, Bridge, Watcher) {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, &format!("{prefix}/#")).await;
    let device = FakeDevice::start(support::root(true, 30, "Prism")).await;
    let bridge = Bridge::start(
        &support::broker(port, prefix),
        &[support::device("dev", device.address)],
        support::options(),
    )
    .unwrap();
    // After a connect `connected` may come before or after the device's values; wait for all.
    let connected = format!("{prefix}/connected");
    let status: Vec<String> = STATUS.iter().map(|p| topic(prefix, p)).collect();
    let mut wanted = vec![(connected.as_str(), Some("2"))];
    wanted.extend(status.iter().map(|t| (t.as_str(), None)));
    watcher.until_all(&wanted).await;
    (port, device, bridge, watcher)
}

/// The last message per topic.
fn last_per_topic(messages: &[Msg]) -> BTreeMap<String, Msg> {
    messages
        .iter()
        .map(|m| (m.topic.clone(), m.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_state_is_published_retained_with_val_ts_lc() {
    let (port, _device, bridge, watcher) = running("s1").await;
    watcher.stop().await;

    // A late subscriber gets everything from the broker's retained store.
    let mut late = Watcher::start(port, "s1/#").await;
    let retained = late.barrier("s1/zz-barrier").await;
    let by_topic = last_per_topic(&retained);
    assert!(retained.iter().all(|m| m.retain), "{retained:?}");

    assert_eq!(by_topic["s1/connected"].text(), "2");
    let info = by_topic["s1/info"].json();
    assert_eq!(info["name"], "nanoleaf2mqtt");
    assert_eq!(info["contract_version"], 2);
    assert_eq!(info["spec"], "2.0");
    assert_eq!(info["maintenance"], false);
    assert!(
        info["pid"].as_u64().is_some() && info["started"].is_string(),
        "{info}"
    );

    let mut expected = vec!["s1/connected".to_owned(), "s1/info".to_owned()];
    expected.extend(STATUS.iter().map(|p| topic("s1", p)));
    expected.sort();
    assert_eq!(by_topic.keys().cloned().collect::<Vec<_>>(), expected);

    for path in STATUS {
        let payload = by_topic[&topic("s1", path)].json();
        let object = payload.as_object().unwrap();
        assert_eq!(object.len(), 3, "{path}: {payload}");
        let (ts, lc) = (
            payload["ts"].as_i64().unwrap(),
            payload["lc"].as_i64().unwrap(),
        );
        assert!(lc <= ts && lc > 1_700_000_000_000, "{path}: {payload}");
    }
    let val = |path: &str| by_topic[&topic("s1", path)].val();
    assert_eq!(val("online"), json!(true));
    assert_eq!(val("on"), json!(true));
    assert_eq!(val("brightness"), json!(30));
    assert_eq!(val("hue"), json!(120));
    assert_eq!(val("sat"), json!(80));
    assert_eq!(val("hs"), json!([120, 80]));
    assert_eq!(val("ct"), json!(2700));
    assert_eq!(val("colormode"), json!("effect"));
    assert_eq!(val("effect"), json!("Prism"));
    assert_eq!(val("effects"), json!(["Blaze", "Prism", "Northern Lights"]));
    assert_eq!(
        val("device"),
        json!({"model":"NL42","firmware":"12.0.3","name":"Test Shapes","panels":15})
    );

    late.stop().await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_event_publishes_only_the_changed_topic() {
    let (_port, device, bridge, mut watcher) = running("s2").await;
    // Everything published so far is out of the way.
    watcher.barrier("s2/zz-barrier").await;

    device.push(1, json!({"events":[{"attr":2,"value":55}]}));
    device.push(3, json!({"events":[{"attr":1,"value":"Blaze"}]}));
    let seen = watcher
        .until_val(&topic("s2", "effect"), json!("Blaze"))
        .await;
    let topics: Vec<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(topics, [topic("s2", "brightness"), topic("s2", "effect")]);
    let brightness = seen[0].json();
    assert_eq!(brightness["val"], 55);
    assert_eq!(brightness["lc"], brightness["ts"], "changed now");

    // Several attributes in one frame: each its own topic.
    device.push(
        1,
        json!({"events":[{"attr":1,"value":false},{"attr":2,"value":0}]}),
    );
    let seen = watcher
        .until_val(&topic("s2", "brightness"), json!(0))
        .await;
    let topics: Vec<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(topics, [topic("s2", "on"), topic("s2", "brightness")]);
    assert_eq!(seen[0].val(), json!(false));

    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_colour_mode_change_reads_the_effect_that_goes_with_it() {
    let (_port, device, bridge, mut watcher) = running("s6").await;
    watcher.barrier("s6/zz-barrier").await;

    // Switched to a colour elsewhere: the device reports hue and mode, but not the effect.
    let mut root = support::root(true, 30, "*Solid*");
    root["state"]["colorMode"] = json!("hs");
    root["state"]["hue"]["value"] = json!(10);
    device.set_root(root);
    device.push(
        1,
        json!({"events":[{"attr":3,"value":10},{"attr":6,"value":"hs"}]}),
    );
    let seen = watcher
        .until_val(&topic("s6", "effect"), json!("*Solid*"))
        .await;
    let last = last_per_topic(&seen);
    assert_eq!(last[&topic("s6", "colormode")].val(), json!("hs"));
    assert_eq!(last[&topic("s6", "hue")].val(), json!(10));
    assert_eq!(last[&topic("s6", "hs")].val(), json!([10, 80]));

    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_device_is_unreachable_and_comes_back_with_a_full_republish() {
    let (port, device, bridge, mut watcher) = running("s3").await;
    watcher.barrier("s3/zz-barrier").await;
    let mut retained = Watcher::start(port, "s3/#").await;
    let before = last_per_topic(&retained.barrier("s3/zz-barrier").await);
    retained.stop().await;
    // The other subscriber's barrier is out of the way.
    watcher.barrier("s3/zz-barrier").await;

    device.set_down(true);
    let seen = watcher
        .until_val(&topic("s3", "online"), json!(false))
        .await;
    // The last values stay: nothing but `online` is published, and `connected` stays `2`: the
    // bridge itself is up.
    let topics: Vec<&str> = seen.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(topics, [topic("s3", "online").as_str()]);

    // Changed while away: the re-read must show it.
    device.set_root(support::root(true, 77, "Prism"));
    device.set_down(false);
    let seen = watcher.until_val(&topic("s3", "online"), json!(true)).await;
    let back = last_per_topic(&seen);
    for path in STATUS {
        assert!(back.contains_key(&topic("s3", path)), "{path} republished");
    }
    assert_eq!(back[&topic("s3", "brightness")].val(), json!(77));
    // `online` follows the values; `connected` did not move.
    assert!(seen.iter().all(|m| m.topic != "s3/connected"), "{seen:?}");
    let mut retained = Watcher::start(port, "s3/connected").await;
    retained.until_text("s3/connected", "2").await;
    retained.stop().await;

    // `lc` moves only for values that changed.
    let lc = |m: &Msg| m.json()["lc"].as_i64().unwrap();
    let ts = |m: &Msg| m.json()["ts"].as_i64().unwrap();
    let hue_before = &before[&topic("s3", "hue")];
    let hue_after = &back[&topic("s3", "hue")];
    assert_eq!(lc(hue_after), lc(hue_before));
    assert!(ts(hue_after) >= ts(hue_before));
    let brightness = &back[&topic("s3", "brightness")];
    assert_eq!(lc(brightness), ts(brightness));
    assert!(device.streams() >= 2, "stream reopened");

    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn touch_is_published_but_not_retained() {
    let (port, device, bridge, mut watcher) = running("s4").await;
    watcher.barrier("s4/zz-barrier").await;

    device.push(4, json!({"events":[{"panelId":1234,"gesture":2}]}));
    device.push(4, json!({"events":[{"panelId":-1,"gesture":4}]}));
    let seen = watcher
        .until(|m| m.topic == topic("s4", "touch") && m.val()["gesture"] == "swipe_left")
        .await;
    let touches: Vec<Value> = seen
        .iter()
        .filter(|m| m.topic == topic("s4", "touch"))
        .map(Msg::json)
        .collect();
    assert_eq!(touches.len(), 2);
    assert_eq!(
        touches[0]["val"],
        json!({"panel":1234,"gesture":"swipe_up"})
    );
    assert_eq!(touches[1]["val"], json!({"gesture":"swipe_left"}));
    for touch in &touches {
        let object = touch.as_object().unwrap();
        assert!(
            object.contains_key("ts") && !object.contains_key("lc"),
            "{touch}"
        );
    }

    let mut late = Watcher::start(port, "s4/#").await;
    let retained = late.barrier("s4/zz-barrier").await;
    assert!(!retained.is_empty());
    assert!(
        retained.iter().all(|m| m.topic != topic("s4", "touch")),
        "touch retained"
    );

    late.stop().await;
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_devices_connected_is_2() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "s5/#").await;
    let bridge = Bridge::start(&support::broker(port, "s5"), &[], support::options()).unwrap();
    let seen = watcher.until(|m| m.topic == "s5/info").await;
    assert_eq!(seen[0].topic, "s5/connected");
    assert_eq!(seen[0].text(), "2");
    bridge.shutdown().await;
    watcher.until_text("s5/connected", "0").await;
}
