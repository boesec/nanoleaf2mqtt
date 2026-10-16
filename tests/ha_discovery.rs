//! The Home Assistant discovery payloads, pinned by golden files under `tests/golden/ha/`.
//!
//! The golden files hold `<version>` where the service version goes. A changed payload fails
//! here; the message shows the new payload, to be checked and copied into the golden file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nanoleaf2mqtt::model::{DeviceId, DeviceInfo, DeviceSnapshot, LightState, Ranges, ValueRange};
use nanoleaf2mqtt::mqtt::discovery::{self, DiscoveryDevice};
use nanoleaf2mqtt::mqtt::{HaEntity, Topics, ha_discovery};
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
fn built(entity: HaEntity) -> Value {
    let (id, about) = sample();
    let topics = Topics::new("nanoleaf").unwrap();
    let mut payload = discovery::config(&topics, &id, &about, entity);
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

fn check(entity: HaEntity, name: &str) {
    let payload = built(entity);
    assert_eq!(
        payload,
        golden(name),
        "{name} changed; new payload:\n{}",
        serde_json::to_string_pretty(&payload).unwrap()
    );
}

#[test]
fn light_payload() {
    check(HaEntity::Light, "light");
}

#[test]
fn event_payload() {
    check(HaEntity::Touch, "event");
}

#[test]
fn button_payload() {
    check(HaEntity::Identify, "button");
}

#[test]
fn discovery_topics() {
    let (id, about) = sample();
    let topics = Topics::new("nanoleaf").unwrap();
    let messages = discovery::messages(&topics, "homeassistant", &id, &about);
    let names: Vec<&str> = messages.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        names,
        [
            "homeassistant/light/nanoleaf2mqtt_living-room/light/config",
            "homeassistant/event/nanoleaf2mqtt_living-room/touch/config",
            "homeassistant/button/nanoleaf2mqtt_living-room/identify/config",
        ]
    );
    assert_eq!(
        ha_discovery("ha/test", &id, HaEntity::Light),
        "ha/test/light/nanoleaf2mqtt_living-room/light/config"
    );
    assert_eq!(discovery::topics("homeassistant", &id), names);
}

#[test]
fn every_payload_follows_the_prefix_and_identifies_the_device() {
    let (id, about) = sample();
    let topics = Topics::new("home/nl").unwrap();
    for entity in HaEntity::ALL {
        let payload = discovery::config(&topics, &id, &about, entity);
        let text = payload.to_string();
        assert!(!text.contains("\"nanoleaf/"), "{entity:?}: {text}");
        assert!(text.contains("home/nl/status/living-room/"), "{entity:?}");
        assert_eq!(
            payload["availability"][0]["topic"], "home/nl/connected",
            "{entity:?}"
        );
        assert_eq!(payload["availability_mode"], "all");
        assert_eq!(
            payload["device"]["identifiers"],
            serde_json::json!(["nanoleaf2mqtt_living-room"])
        );
        let unique = payload["unique_id"].as_str().unwrap();
        assert!(unique.starts_with("nanoleaf2mqtt_living-room_"), "{unique}");
        // Nothing the device reported for itself only: no serial or hardware number.
        assert!(
            !text.contains("S0000TEST") && !text.contains("\"1.0\""),
            "{text}"
        );
    }
}
