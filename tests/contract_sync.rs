//! Keeps `CONTRACT.md` and the code that builds topics and payloads in sync.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use chrono::Utc;
use nanoleaf2mqtt::model::{
    ColorMode, DeviceId, DeviceInfo, DeviceSnapshot, Gesture, Layout, LightState, PanelPosition,
    Ranges,
};
use nanoleaf2mqtt::mqtt::{
    CONTRACT_VERSION, CommandPath, HaEntity, InfoPayload, StatusPath, Topics, ha_discovery, map,
};
use serde_json::Value;

fn contract() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/CONTRACT.md")).unwrap()
}

/// The first backticked cell of every table row in the section starting with `heading`.
fn first_cells(heading: &str) -> Vec<String> {
    rows(heading)
        .into_iter()
        .map(|cells| cells[0].trim_matches('`').to_owned())
        .collect()
}

/// The cells of every table row (header and separator excluded) in the section.
fn rows(heading: &str) -> Vec<Vec<String>> {
    let doc = contract();
    let mut inside = false;
    let mut rows = Vec::new();
    for line in doc.lines() {
        if line.starts_with("## ") {
            inside = line.starts_with(heading);
            continue;
        }
        if inside && line.starts_with("| `") {
            rows.push(
                line.trim_matches('|')
                    .split(" | ")
                    .map(|c| c.trim().to_owned())
                    .collect(),
            );
        }
    }
    assert!(!rows.is_empty(), "no table under {heading}");
    rows
}

#[test]
fn contract_version_matches() {
    assert!(
        contract().contains(&format!("Contract version **{CONTRACT_VERSION}**")),
        "CONTRACT.md states another version than {CONTRACT_VERSION}"
    );
}

#[test]
fn service_topics_match_the_builder() {
    let topics = Topics::new("nanoleaf").unwrap();
    let documented: BTreeSet<String> = first_cells("## Service topics")
        .into_iter()
        .map(|t| t.replace("<prefix>", "nanoleaf"))
        .collect();
    let built: BTreeSet<String> = [topics.connected(), topics.info()].into();
    assert_eq!(documented, built);
}

#[test]
fn info_payload_matches_the_documented_example() {
    let row = rows("## Service topics")
        .into_iter()
        .find(|r| r[0] == "`<prefix>/info`")
        .unwrap();
    let example: Value = serde_json::from_str(row[2].trim_matches('`')).unwrap();
    let actual = serde_json::to_value(InfoPayload::for_process(
        Utc::now(),
        Some("host".to_owned()),
        1,
    ))
    .unwrap();
    let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&example), keys(&actual));
    for fixed in ["name", "spec", "maintenance", "contract_version"] {
        assert_eq!(example[fixed], actual[fixed], "{fixed}");
    }
}

#[test]
fn status_paths_match_the_builder() {
    let documented: BTreeSet<String> = first_cells("## Device status topics").into_iter().collect();
    let built: BTreeSet<String> = StatusPath::ALL
        .iter()
        .map(|p| p.as_str().to_owned())
        .collect();
    assert_eq!(documented, built);

    let id = DeviceId::new("dev").unwrap();
    let topics = Topics::new("nanoleaf").unwrap();
    for path in StatusPath::ALL {
        assert_eq!(
            topics.status(&id, path),
            format!("nanoleaf/status/dev/{}", path.as_str())
        );
    }
}

#[test]
fn command_paths_match_the_builder() {
    let documented: BTreeSet<String> = first_cells("## Commands").into_iter().collect();
    let built: BTreeSet<String> = CommandPath::ALL
        .iter()
        .map(|p| p.as_str().to_owned())
        .collect();
    assert_eq!(documented, built);
}

#[test]
fn a_complete_device_fills_every_status_path() {
    let snapshot = DeviceSnapshot {
        info: DeviceInfo {
            name: Some("Test".to_owned()),
            model: Some("NL42".to_owned()),
            firmware: Some("1.0".to_owned()),
            panel_count: Some(3),
            layout: Some(Layout {
                orientation: Some(120),
                side_length: Some(134),
                panels: vec![PanelPosition {
                    id: 1001,
                    x: Some(201),
                    y: Some(77),
                    rotation: Some(0),
                    shape_type: Some(8),
                }],
            }),
            ..DeviceInfo::default()
        },
        state: LightState {
            on: Some(true),
            brightness: Some(1),
            hue: Some(1),
            saturation: Some(1),
            color_temperature: Some(2700),
            color_mode: Some(ColorMode::Effect),
            effect: Some("A".to_owned()),
            ranges: Ranges::default(),
        },
        effects: Some(vec!["A".to_owned()]),
    };
    let mut emitted: BTreeSet<StatusPath> = map::snapshot_values(&snapshot)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    // Reachability and touch come from their own events.
    emitted.insert(map::online_value(true).0);
    emitted.insert(StatusPath::Touch);
    assert_eq!(emitted, StatusPath::ALL.into_iter().collect());

    let device = map::snapshot_values(&snapshot)
        .into_iter()
        .find(|(p, _)| *p == StatusPath::Device)
        .unwrap()
        .1;
    let row = rows("## Device status topics")
        .into_iter()
        .find(|r| r[0] == "`device`")
        .unwrap();
    let example: Value = serde_json::from_str(row[2].trim_matches('`')).unwrap();
    assert_eq!(
        example.as_object().unwrap().keys().collect::<BTreeSet<_>>(),
        device.as_object().unwrap().keys().collect::<BTreeSet<_>>()
    );
}

#[test]
fn layout_example_and_shape_names_match_the_code() {
    let row = rows("## Device status topics")
        .into_iter()
        .find(|r| r[0] == "`layout`")
        .unwrap();
    let example: Value = serde_json::from_str(row[2].trim_matches('`')).unwrap();
    let panel = &example["panels"][0];
    assert_eq!(panel["shape"], map::shape_name(8));
    assert_eq!(panel["shape_type"], 8);

    // Every shape name in the code is documented with its number, and the other way round.
    let doc = contract();
    let mut known = 0;
    for number in 0..64_u32 {
        let name = map::shape_name(number);
        if name != "unknown" {
            known += 1;
            assert!(
                doc.contains(&format!("`{name}` ({number})"))
                    || doc.contains(&format!("`{name}`\n({number})")),
                "{name} ({number}) is not documented"
            );
        }
    }
    assert_eq!(known, 17);
    assert!(doc.contains("`unknown` for any other number"));
}

#[test]
fn gestures_match_the_documented_list() {
    let doc = contract();
    let line = doc
        .lines()
        .skip_while(|l| !l.starts_with("Gestures:"))
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let documented: BTreeSet<String> = line
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    let mut built: BTreeSet<String> = [
        Gesture::SingleTap,
        Gesture::DoubleTap,
        Gesture::SwipeUp,
        Gesture::SwipeDown,
        Gesture::SwipeLeft,
        Gesture::SwipeRight,
        Gesture::LongPress,
    ]
    .into_iter()
    .map(map::gesture_name)
    .collect();
    assert_eq!(map::gesture_name(Gesture::Unknown(7)), "unknown_7");
    built.insert("unknown_<n>".to_owned());
    assert_eq!(documented, built);

    let row = rows("## Device status topics")
        .into_iter()
        .find(|r| r[0] == "`touch`")
        .unwrap();
    let example: Value = serde_json::from_str(row[2].trim_matches('`')).unwrap();
    assert_eq!(example, map::touch_value(Some(1234), Gesture::SingleTap));
}

#[test]
fn home_assistant_discovery_topics_match_the_builder() {
    let id = DeviceId::new("dev").unwrap();
    let documented: BTreeSet<String> = first_cells("## Home Assistant")
        .into_iter()
        .filter(|t| t.contains('/'))
        .map(|t| t.replace("<ha>", "homeassistant").replace("<id>", "dev"))
        .collect();
    let built: BTreeSet<String> = [ha_discovery("homeassistant", &id)].into();
    assert_eq!(documented, built);

    // The components of the message: one row each, with its platform and unique id.
    let components = rows("## Home Assistant")
        .into_iter()
        .filter(|r| r.len() == 4 && r[0].starts_with("`") && !r[0].contains('/'))
        .map(|r| {
            (
                r[0].trim_matches('`').to_owned(),
                r[1].trim_matches('`').to_owned(),
                r[2].trim_matches('`').replace("<id>", "dev"),
            )
        })
        .collect::<Vec<_>>();
    let topics = Topics::new("nanoleaf").unwrap();
    let payload = nanoleaf2mqtt::mqtt::discovery::config(
        &topics,
        &id,
        &nanoleaf2mqtt::mqtt::discovery::DiscoveryDevice::default(),
        true,
    );
    let built: Vec<(String, String, String)> = HaEntity::ALL
        .into_iter()
        .map(|entity| {
            let component = &payload["components"][entity.key()];
            (
                entity.key().to_owned(),
                component["p"].as_str().unwrap().to_owned(),
                component["unique_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(components, built);
}

#[test]
fn the_deviations_from_mqtt_smarthome_are_documented() {
    let doc = contract();
    let section = doc
        .split("\n## ")
        .find(|s| s.starts_with("Deviations from mqtt-smarthome 2.0"))
        .expect("a section on the deviations");
    for topic in ["`-`", "`connected = 1`"] {
        assert!(section.contains(topic), "{topic} not in the deviations");
    }
}

#[test]
fn hs_is_documented_with_its_example() {
    let row = rows("## Device status topics")
        .into_iter()
        .find(|r| r[0] == "`hs`")
        .unwrap();
    let example: Value = serde_json::from_str(row[2].trim_matches('`')).unwrap();
    assert_eq!(map::hs_value(120, 80).1, example);
}
