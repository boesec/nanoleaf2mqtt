//! Tolerant mapping of the device's JSON answers into the model.
//!
//! Every field is looked up individually: a missing field or one of an unexpected type becomes
//! `None`, unknown fields are ignored. Only a body that is not JSON at all is an error.

use serde_json::Value;

use crate::model::{
    ColorMode, DeviceInfo, DeviceSnapshot, Layout, LightState, PanelPosition, Ranges, ValueRange,
};

/// Parses the answer of `GET /` (all panel info).
pub fn parse_root(body: &[u8]) -> Result<DeviceSnapshot, serde_json::Error> {
    let root: Value = serde_json::from_slice(body)?;
    let state = &root["state"];
    let effects = &root["effects"];
    let (brightness, brightness_range) = ranged(&state["brightness"]);
    let (hue, hue_range) = ranged(&state["hue"]);
    let (saturation, saturation_range) = ranged(&state["sat"]);
    let (color_temperature, color_temperature_range) = ranged(&state["ct"]);
    Ok(DeviceSnapshot {
        info: DeviceInfo {
            name: string(&root["name"]),
            serial: string(&root["serialNo"]),
            manufacturer: string(&root["manufacturer"]),
            model: string(&root["model"]),
            firmware: string(&root["firmwareVersion"]),
            hardware: string(&root["hardwareVersion"]),
            panel_count: root["panelLayout"]["layout"]["numPanels"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok()),
            layout: layout(&root["panelLayout"]),
        },
        state: LightState {
            on: state["on"]["value"].as_bool(),
            brightness,
            hue,
            saturation,
            color_temperature,
            color_mode: state["colorMode"].as_str().map(color_mode),
            effect: string(&effects["select"]),
            ranges: Ranges {
                brightness: brightness_range,
                hue: hue_range,
                saturation: saturation_range,
                color_temperature: color_temperature_range,
            },
        },
        effects: string_list(&effects["effectsList"]),
    })
}

/// The panel geometry; `None` unless the device reported a list of position data. A panel
/// without an id is skipped, any other missing field stays `None`.
fn layout(panel_layout: &Value) -> Option<Layout> {
    let positions = panel_layout["layout"]["positionData"].as_array()?;
    let panels = positions
        .iter()
        .filter_map(|p| {
            Some(PanelPosition {
                id: p["panelId"].as_u64().and_then(|n| u32::try_from(n).ok())?,
                x: integer(&p["x"]),
                y: integer(&p["y"]),
                rotation: number(&p["o"]),
                shape_type: p["shapeType"].as_u64().and_then(|n| u32::try_from(n).ok()),
            })
        })
        .collect();
    Some(Layout {
        orientation: number(&panel_layout["globalOrientation"]["value"]),
        side_length: panel_layout["layout"]["sideLength"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok()),
        panels,
    })
}

/// A signed integer that fits `i32`.
fn integer(value: &Value) -> Option<i32> {
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

/// Parses the answer of `GET /effects/effectsList`. Entries that are not strings are skipped; a
/// body that is not a list yields an empty list.
pub fn parse_effects_list(body: &[u8]) -> Result<Vec<String>, serde_json::Error> {
    let value: Value = serde_json::from_slice(body)?;
    Ok(string_list(&value).unwrap_or_default())
}

/// Maps the device's colour mode name.
pub(crate) fn color_mode(name: &str) -> ColorMode {
    match name {
        "effect" => ColorMode::Effect,
        "hs" => ColorMode::HueSaturation,
        "ct" => ColorMode::ColorTemperature,
        other => ColorMode::Unknown(other.to_owned()),
    }
}

/// A non-negative integer that fits `u16`. Integral floats (`30.0`) are accepted.
pub(crate) fn number(value: &Value) -> Option<u16> {
    if let Some(n) = value.as_u64() {
        return u16::try_from(n).ok();
    }
    let f = value.as_f64()?;
    if f.fract() == 0.0 && (0.0..=f64::from(u16::MAX)).contains(&f) {
        // Integral and inside the range checked just above, so the conversion is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        return Some(f as u16);
    }
    None
}

fn string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn string_list(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    })
}

/// `{"value": v, "min": a, "max": b}` → value and range; the range only when both ends are there.
fn ranged(value: &Value) -> (Option<u16>, Option<ValueRange>) {
    let range = match (number(&value["min"]), number(&value["max"])) {
        (Some(min), Some(max)) if min <= max => Some(ValueRange { min, max }),
        _ => None,
    };
    (number(&value["value"]), range)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &[u8] = include_bytes!("../../fixtures/nanoleaf/root.json");
    const ROOT_MINIMAL: &[u8] = include_bytes!("../../fixtures/nanoleaf/root-minimal.json");
    const EFFECTS_LIST: &[u8] = include_bytes!("../../fixtures/nanoleaf/effects-list.json");

    #[test]
    fn root_fixture_parses_into_model() {
        let snapshot = parse_root(ROOT).unwrap();
        assert_eq!(
            snapshot.info,
            DeviceInfo {
                name: Some("Shapes Test Panels".to_owned()),
                serial: Some("S00000000000".to_owned()),
                manufacturer: Some("Nanoleaf".to_owned()),
                model: Some("NL42".to_owned()),
                firmware: Some("12.0.3".to_owned()),
                hardware: Some("1.2-0".to_owned()),
                panel_count: Some(15),
                layout: snapshot.info.layout.clone(),
            }
        );
        assert_eq!(
            snapshot.state,
            LightState {
                on: Some(false),
                brightness: Some(30),
                hue: Some(0),
                saturation: Some(0),
                color_temperature: Some(2700),
                color_mode: Some(ColorMode::Effect),
                effect: Some("Morning Sky".to_owned()),
                ranges: Ranges {
                    brightness: Some(ValueRange { min: 0, max: 100 }),
                    hue: Some(ValueRange { min: 0, max: 360 }),
                    saturation: Some(ValueRange { min: 0, max: 100 }),
                    color_temperature: Some(ValueRange {
                        min: 1200,
                        max: 6500
                    }),
                },
            }
        );
        let effects = snapshot.effects.unwrap();
        assert_eq!(effects.len(), 20);
        assert!(effects.contains(&"Northern Lights".to_owned()));
    }

    #[test]
    fn root_fixture_layout_is_parsed() {
        let layout = parse_root(ROOT).unwrap().info.layout.unwrap();
        assert_eq!(layout.orientation, Some(120));
        assert_eq!(layout.side_length, Some(134));
        assert_eq!(layout.panels.len(), 15);
        assert_eq!(
            layout.panels[0],
            PanelPosition {
                id: 1001,
                x: Some(201),
                y: Some(77),
                rotation: Some(0),
                shape_type: Some(8),
            }
        );
        let controller = layout.panels.last().unwrap();
        assert_eq!(controller.id, 0);
        assert_eq!(controller.rotation, Some(300));
        assert_eq!(controller.shape_type, Some(12));
    }

    #[test]
    fn layout_is_absent_without_position_data() {
        assert_eq!(parse_root(ROOT_MINIMAL).unwrap().info.layout, None);
    }

    #[test]
    fn layout_fields_are_tolerant() {
        let body = br#"{"panelLayout":{"layout":{"positionData":[
            {"panelId":5,"x":"left","y":-3,"shapeType":99},
            {"x":1,"y":2},
            {"panelId":6}]}}}"#;
        let layout = parse_root(body).unwrap().info.layout.unwrap();
        assert_eq!(layout.orientation, None);
        assert_eq!(layout.side_length, None);
        assert_eq!(
            layout.panels,
            vec![
                PanelPosition {
                    id: 5,
                    x: None,
                    y: Some(-3),
                    rotation: None,
                    shape_type: Some(99),
                },
                PanelPosition {
                    id: 6,
                    ..PanelPosition::default()
                },
            ]
        );
    }

    #[test]
    fn opaque_and_unknown_keys_are_ignored() {
        // The fixture carries the opaque random-named key of the real device (renamed).
        let text = std::str::from_utf8(ROOT).unwrap();
        assert!(text.contains("opaquekeyplaceholder"));
        assert!(parse_root(ROOT).is_ok());
        assert!(
            std::str::from_utf8(ROOT_MINIMAL)
                .unwrap()
                .contains("someFutureField")
        );
        assert!(parse_root(ROOT_MINIMAL).is_ok());
    }

    #[test]
    fn missing_fields_stay_none() {
        let snapshot = parse_root(ROOT_MINIMAL).unwrap();
        assert_eq!(
            snapshot.info,
            DeviceInfo {
                name: None,
                serial: None,
                manufacturer: None,
                model: Some("NL42".to_owned()),
                firmware: Some("12.0.3".to_owned()),
                hardware: None,
                panel_count: None,
                layout: None,
            }
        );
        let state = snapshot.state;
        assert_eq!(state.on, Some(true));
        assert_eq!(state.brightness, Some(55));
        assert_eq!(state.ranges.brightness, None, "range without min/max");
        assert_eq!(state.hue, None, "wrong type is absent, not zero");
        assert_eq!(state.ranges.hue, Some(ValueRange { min: 0, max: 360 }));
        assert_eq!(state.saturation, None);
        assert_eq!(state.ranges.saturation, None);
        assert_eq!(state.color_temperature, None);
        assert_eq!(state.ranges.color_temperature, None);
        assert_eq!(
            state.color_mode,
            Some(ColorMode::Unknown("rainbow".to_owned()))
        );
        assert_eq!(state.effect, None);
        assert_eq!(
            snapshot.effects,
            Some(vec!["Blaze".to_owned(), "Prism".to_owned()])
        );
    }

    #[test]
    fn empty_object_is_all_none() {
        let snapshot = parse_root(b"{}").unwrap();
        assert_eq!(snapshot, DeviceSnapshot::default());
        assert_eq!(snapshot.state.on, None);
        assert_eq!(snapshot.effects, None);
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_root(b"<html>").is_err());
        assert!(parse_effects_list(b"[").is_err());
    }

    #[test]
    fn effects_list_fixture() {
        let list = parse_effects_list(EFFECTS_LIST).unwrap();
        assert_eq!(list.len(), 20);
        assert_eq!(list[0], "Beatdrop");
        assert_eq!(parse_effects_list(b"{}").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn numbers() {
        assert_eq!(number(&serde_json::json!(30)), Some(30));
        assert_eq!(number(&serde_json::json!(30.0)), Some(30));
        assert_eq!(number(&serde_json::json!(30.5)), None);
        assert_eq!(number(&serde_json::json!(-1)), None);
        assert_eq!(number(&serde_json::json!(70000)), None);
        assert_eq!(number(&serde_json::json!("30")), None);
    }
}
