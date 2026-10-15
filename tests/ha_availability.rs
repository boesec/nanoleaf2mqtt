//! Home Assistant availability: the service-up entry and the per-device entry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nanoleaf2mqtt::model::DeviceId;
use nanoleaf2mqtt::mqtt::discovery::{self, DiscoveryDevice};
use nanoleaf2mqtt::mqtt::{HaEntity, Topics};

/// The template maps `connected` to availability. Home Assistant renders it; here the accepted
/// values are read from the template itself: `1` and `2` online, `0` and anything else offline.
#[test]
fn connected_template_accepts_one_and_two_only() {
    let id = DeviceId::new("living-room").unwrap();
    let topics = Topics::new("nanoleaf").unwrap();
    let about = DiscoveryDevice::default();
    for entity in HaEntity::ALL {
        let payload = discovery::config(&topics, &id, &about, entity);
        let template = payload["availability"][0]["value_template"]
            .as_str()
            .unwrap();
        assert_eq!(
            template,
            "{{ 'online' if value in ['1', '2'] else 'offline' }}"
        );
        let list = template
            .split('[')
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap();
        let accepted: Vec<&str> = list
            .split(',')
            .map(|v| v.trim().trim_matches('\''))
            .collect();
        assert_eq!(accepted, ["1", "2"]);
        for (value, online) in [
            ("0", false),
            ("1", true),
            ("2", true),
            ("3", false),
            ("", false),
        ] {
            assert_eq!(accepted.contains(&value), online, "connected = {value:?}");
        }
        assert!(template.ends_with("else 'offline' }}"));
    }
}

/// Two devices, one unreachable (`connected` is `1`): the reachable device's inputs are the
/// shared `connected` topic and its own `reachable` topic, so it stays available.
#[test]
fn availability_is_per_device() {
    let topics = Topics::new("nanoleaf").unwrap();
    let about = DiscoveryDevice::default();
    let a = DeviceId::new("living-room").unwrap();
    let b = DeviceId::new("bedroom").unwrap();
    for entity in HaEntity::ALL {
        let pa = discovery::config(&topics, &a, &about, entity);
        let pb = discovery::config(&topics, &b, &about, entity);
        assert_eq!(pa["availability"][0], pb["availability"][0], "{entity:?}");
        assert_eq!(
            pa["availability"][1]["topic"],
            "nanoleaf/status/living-room/reachable"
        );
        assert_eq!(
            pb["availability"][1]["topic"],
            "nanoleaf/status/bedroom/reachable"
        );
        assert_eq!(
            pa["availability"][1]["value_template"],
            "{{ 'online' if value_json is defined and value_json.val == true else 'offline' }}"
        );
        assert_eq!(pa["availability_mode"], "all");
    }
}
