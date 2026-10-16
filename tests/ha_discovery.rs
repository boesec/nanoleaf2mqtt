//! The Home Assistant discovery payload, pinned by a golden file under `tests/golden/ha/`.
//!
//! The golden file holds `<version>` where the service version goes. A changed payload fails
//! here; the message shows the new payload, to be checked and copied into the golden file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nanoleaf2mqtt::model::{DeviceId, DeviceInfo, DeviceSnapshot, LightState, Ranges, ValueRange};
use nanoleaf2mqtt::mqtt::discovery::{self, DiscoveryDevice};
use nanoleaf2mqtt::mqtt::{HaEntity, Topics, ha_discovery, ha_legacy_discovery};
use serde_json::Value;

/// The sample device: its id has a `-`, so the entity id conversion shows.
fn sample() -> (DeviceId, DiscoveryDevice) {
    let mut about = DiscoveryDevice::default();
    about.update(&DeviceSnapshot {
        info: DeviceInfo {
            name: Some("Test Shapes".to_owned()),
            serial: Some("S0000TEST".to_owned()),
            manufacturer: Some("Nanoleaf".to_owned()),
            model: Some("NL42".to_owned()),
            firmware: Some("12.0.3".to_owned()),
            hardware: Some("1.0".to_owned()),
            panel_count: Some(15),
            layout: None,
        },
        state: LightState {
            ranges: Ranges {
                color_temperature: Some(ValueRange {
                    min: 1200,
                    max: 6500,
                }),
                ..Ranges::default()
            },
            ..LightState::default()
        },
        effects: Some(vec![
            "Blaze".to_owned(),
            "Northern Lights".to_owned(),
            "Prism".to_owned(),
        ]),
    });
    (DeviceId::new("living-room").unwrap(), about)
}

fn golden(name: &str) -> Value {
    let path = format!("{}/tests/golden/ha/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The payload as published, with the version replaced by `<version>`.
fn built(topics: &Topics, online_known: bool) -> Value {
    let (id, about) = sample();
    let mut payload = discovery::config(topics, &id, &about, online_known);
    // Published as JSON text: it must survive the round trip.
    let text = serde_json::to_string(&payload).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), payload);
    assert_eq!(
        payload["origin"]["sw_version"],
        env!("CARGO_PKG_VERSION"),
        "origin carries the service version"
    );
    payload["origin"]["sw_version"] = Value::from("<version>");
    payload
}

#[test]
fn device_payload() {
    let payload = built(&Topics::new("nanoleaf").unwrap(), true);
    assert_eq!(
        payload,
        golden("device"),
        "device payload changed; new payload:\n{}",
        serde_json::to_string_pretty(&payload).unwrap()
    );
}

#[test]
fn every_component_names_its_platform_and_keeps_its_unique_id() {
    let payload = built(&Topics::new("nanoleaf").unwrap(), true);
    let components = payload["components"].as_object().unwrap();
    let keys: Vec<&str> = components.keys().map(String::as_str).collect();
    assert_eq!(keys, ["identify", "light", "touch"]);
    for entity in HaEntity::ALL {
        let component = &components[entity.key()];
        assert_eq!(component["p"], entity.component(), "{entity:?}");
        // The unique ids of contract version 1: Home Assistant keeps the entities.
        assert_eq!(
            component["unique_id"],
            format!("nanoleaf2mqtt_living-room_{}", entity.key())
        );
        // Device, origin and availability are shared; never set per component.
        for key in ["device", "origin", "availability", "availability_mode"] {
            assert!(component.get(key).is_none(), "{entity:?} {key}");
        }
    }
}

/// Only the keys mqtt-smarthome 2.0 allows in an availability entry: Home Assistant rejects the
/// whole device message over one unknown key there.
#[test]
fn availability_entries_carry_only_the_allowed_keys() {
    let payload = built(&Topics::new("nanoleaf").unwrap(), true);
    let allowed = [
        "payload_available",
        "payload_not_available",
        "topic",
        "value_template",
    ];
    for entry in payload["availability"].as_array().unwrap() {
        let keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, allowed, "{entry}");
    }
    assert_eq!(payload["availability_mode"], "all");
    assert_eq!(payload["qos"], 0);
}

#[test]
fn discovery_topics() {
    let (id, about) = sample();
    let topics = Topics::new("nanoleaf").unwrap();
    let (topic, _) = discovery::message(&topics, "homeassistant", &id, &about, true);
    assert_eq!(
        topic,
        "homeassistant/device/nanoleaf2mqtt_living-room/config"
    );
    assert_eq!(
        ha_discovery("ha/test", &id),
        "ha/test/device/nanoleaf2mqtt_living-room/config"
    );
    assert_eq!(discovery::topics("homeassistant", &id), [topic]);
    // Where version 1 announced the entities: migrated and cleared, never published.
    assert_eq!(
        HaEntity::ALL.map(|e| ha_legacy_discovery("homeassistant", &id, e)),
        [
            "homeassistant/light/nanoleaf2mqtt_living-room/light/config",
            "homeassistant/event/nanoleaf2mqtt_living-room/touch/config",
            "homeassistant/button/nanoleaf2mqtt_living-room/identify/config",
        ]
    );
}

#[test]
fn the_payload_follows_the_prefix_and_identifies_the_device() {
    let topics = Topics::new("home/nl").unwrap();
    let payload = built(&topics, true);
    let text = payload.to_string();
    assert!(!text.contains("\"nanoleaf/"), "{text}");
    assert!(text.contains("home/nl/status/living-room/"));
    assert_eq!(payload["availability"][0]["topic"], "home/nl/connected");
    assert_eq!(
        payload["availability"][1]["topic"],
        "home/nl/status/living-room/online"
    );
    assert_eq!(
        payload["device"]["identifiers"],
        serde_json::json!(["nanoleaf2mqtt_living-room"])
    );
    // Nothing the device reported for itself only: no serial or hardware number.
    assert!(
        !text.contains("S0000TEST") && !text.contains("\"1.0\""),
        "{text}"
    );
}
