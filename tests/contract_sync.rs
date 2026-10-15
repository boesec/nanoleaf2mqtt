//! Keeps `CONTRACT.md` and the code that builds topics and payloads in sync.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use chrono::Utc;
use nanoleaf2mqtt::model::{
    ColorMode, DeviceId, DeviceInfo, DeviceSnapshot, Gesture, LightState, Ranges,
};
use nanoleaf2mqtt::mqtt::{CONTRACT_VERSION, CommandPath, InfoPayload, StatusPath, Topics, map};
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
    let actual = serde_json::to_value(InfoPayload::new(Utc::now())).unwrap();
    let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&example), keys(&actual));
    for fixed in ["name", "spec", "contract_version"] {
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
    emitted.insert(map::reachable_value(true).0);
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
    assert_eq!(example, map::touch_value(Some(1234), Gesture::SwipeUp));
}
