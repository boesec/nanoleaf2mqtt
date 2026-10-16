//! Home Assistant availability: the bridge entry (`connected`) and the per-device entry
//! (`online`), as mqtt-smarthome 2.0 section 8 describes them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nanoleaf2mqtt::model::DeviceId;
use nanoleaf2mqtt::mqtt::Topics;
use nanoleaf2mqtt::mqtt::discovery::{self, DiscoveryDevice};

/// The template maps `connected` to availability: available at `2` (and above), not at `0`,
/// `1`, a cleared topic or anything else. Home Assistant renders it with Jinja; the template is
/// pinned here, and its rule checked on the values `connected` can take.
#[test]
fn connected_is_available_from_two() {
    let id = DeviceId::new("living-room").unwrap();
    let topics = Topics::new("nanoleaf").unwrap();
    let payload = discovery::config(&topics, &id, &DiscoveryDevice::default(), true);
    let entry = &payload["availability"][0];
    assert_eq!(entry["topic"], "nanoleaf/connected");
    assert_eq!(
        entry["value_template"],
        "{{ 'online' if value | int(0) >= 2 else 'offline' }}"
    );
    assert_eq!(entry["payload_available"], "online");
    assert_eq!(entry["payload_not_available"], "offline");
    // `int(0)` turns anything that is no number into 0.
    let rule = |value: &str| value.trim().parse::<i64>().unwrap_or(0) >= 2;
    for (value, available) in [
        ("0", false),
        ("1", false),
        ("2", true),
        ("", false),
        ("x", false),
    ] {
        assert_eq!(rule(value), available, "connected = {value:?}");
    }
}

/// Two devices, one unreachable: `connected` stays `2`, each device's availability also
/// follows its own `online` topic, so one device's state does not affect the other.
#[test]
fn availability_is_per_device() {
    let topics = Topics::new("nanoleaf").unwrap();
    let about = DiscoveryDevice::default();
    let a = DeviceId::new("living-room").unwrap();
    let b = DeviceId::new("bedroom").unwrap();
    let pa = discovery::config(&topics, &a, &about, true);
    let pb = discovery::config(&topics, &b, &about, true);
    assert_eq!(pa["availability"][0], pb["availability"][0]);
    assert_eq!(
        pa["availability"][1]["topic"],
        "nanoleaf/status/living-room/online"
    );
    assert_eq!(
        pb["availability"][1]["topic"],
        "nanoleaf/status/bedroom/online"
    );
    assert_eq!(
        pa["availability"][1]["value_template"],
        "{{ 'online' if value_json is defined and value_json.val == true else 'offline' }}"
    );
    assert_eq!(pa["availability_mode"], "all");
}

/// An availability topic without a value leaves an entity unavailable in Home Assistant for
/// good: the `online` entry is only named once the topic has a value.
#[test]
fn online_is_named_only_once_known() {
    let topics = Topics::new("nanoleaf").unwrap();
    let id = DeviceId::new("bedroom").unwrap();
    let payload = discovery::config(&topics, &id, &DiscoveryDevice::default(), false);
    let entries = payload["availability"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["topic"], "nanoleaf/connected");
}
