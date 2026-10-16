//! What the model's values look like on the wire: one `(path, val)` per status topic.
//!
//! Only what the device reported is mapped; an absent value yields no topic.

use serde_json::{Map, Value, json};

use super::topic::StatusPath;
use crate::model::{ColorMode, DeviceInfo, DeviceSnapshot, Gesture, Layout, StateChange};

/// The `val` of every status topic a snapshot reports a value for.
pub fn snapshot_values(snapshot: &DeviceSnapshot) -> Vec<(StatusPath, Value)> {
    let state = &snapshot.state;
    let mut values = Vec::new();
    if let Some(on) = state.on {
        values.push((StatusPath::On, json!(on)));
    }
    if let Some(v) = state.brightness {
        values.push((StatusPath::Brightness, json!(v)));
    }
    if let Some(v) = state.hue {
        values.push((StatusPath::Hue, json!(v)));
    }
    if let Some(v) = state.saturation {
        values.push((StatusPath::Saturation, json!(v)));
    }
    if let (Some(hue), Some(saturation)) = (state.hue, state.saturation) {
        values.push(hs_value(hue, saturation));
    }
    if let Some(v) = state.color_temperature {
        values.push((StatusPath::ColorTemperature, json!(v)));
    }
    if let Some(mode) = &state.color_mode {
        values.push((StatusPath::ColorMode, json!(color_mode(mode))));
    }
    if let Some(effect) = &state.effect {
        values.push((StatusPath::Effect, json!(effect)));
    }
    if let Some(effects) = &snapshot.effects {
        values.push((StatusPath::Effects, json!(effects)));
    }
    if let Some(device) = device_value(&snapshot.info) {
        values.push((StatusPath::Device, device));
    }
    if let Some(layout) = &snapshot.info.layout {
        values.push((StatusPath::Layout, layout_value(layout)));
    }
    values
}

/// Contract name of a panel shape type: the name in the shape-type table of the Nanoleaf
/// OpenAPI documentation (developer portal), spelled as `snake_case`. The numbers up to 20 were
/// cross-checked against the table in the `aionanoleaf` library
/// (<https://github.com/milanmeu/aionanoleaf>, `layout.py`). `unknown` for any other number.
pub fn shape_name(shape_type: u32) -> &'static str {
    match shape_type {
        0 => "triangle",
        1 => "rhythm",
        2 => "square",
        3 => "control_square_primary",
        4 => "control_square_passive",
        5 => "power_supply",
        7 => "hexagon_shapes",
        8 => "triangle_shapes",
        9 => "mini_triangle_shapes",
        12 => "controller",
        14 => "elements_hexagon",
        15 => "elements_hexagon_corner",
        16 => "lines_connector",
        17 => "light_lines",
        18 => "light_lines_single_zone",
        19 => "controller_cap",
        20 => "power_connector",
        _ => "unknown",
    }
}

/// `{orientation, side_length, panels: [{id, x, y, rotation, shape, shape_type}]}` with the
/// fields the device reported.
fn layout_value(layout: &Layout) -> Value {
    let mut object = Map::new();
    if let Some(orientation) = layout.orientation {
        object.insert("orientation".to_owned(), json!(orientation));
    }
    if let Some(side_length) = layout.side_length {
        object.insert("side_length".to_owned(), json!(side_length));
    }
    let panels: Vec<Value> = layout
        .panels
        .iter()
        .map(|panel| {
            let mut p = Map::new();
            p.insert("id".to_owned(), json!(panel.id));
            if let Some(x) = panel.x {
                p.insert("x".to_owned(), json!(x));
            }
            if let Some(y) = panel.y {
                p.insert("y".to_owned(), json!(y));
            }
            if let Some(rotation) = panel.rotation {
                p.insert("rotation".to_owned(), json!(rotation));
            }
            if let Some(shape_type) = panel.shape_type {
                p.insert("shape".to_owned(), json!(shape_name(shape_type)));
                p.insert("shape_type".to_owned(), json!(shape_type));
            }
            Value::Object(p)
        })
        .collect();
    object.insert("panels".to_owned(), Value::Array(panels));
    Value::Object(object)
}

/// The `val` of the topic one state change updates.
pub fn change_value(change: &StateChange) -> (StatusPath, Value) {
    match change {
        StateChange::On(on) => (StatusPath::On, json!(on)),
        StateChange::Brightness(v) => (StatusPath::Brightness, json!(v)),
        StateChange::Hue(v) => (StatusPath::Hue, json!(v)),
        StateChange::Saturation(v) => (StatusPath::Saturation, json!(v)),
        StateChange::ColorTemperature(v) => (StatusPath::ColorTemperature, json!(v)),
        StateChange::ColorMode(mode) => (StatusPath::ColorMode, json!(color_mode(mode))),
    }
}

/// The `val` of `hs`: `[hue, sat]`.
pub fn hs_value(hue: u16, saturation: u16) -> (StatusPath, Value) {
    (StatusPath::HueSaturation, json!([hue, saturation]))
}

/// The `val` of `hs` from the `val`s of `hue` and `sat`; `None` unless both are known numbers.
pub fn hs_from(hue: Option<&Value>, saturation: Option<&Value>) -> Option<(StatusPath, Value)> {
    let hue = u16::try_from(hue?.as_u64()?).ok()?;
    let saturation = u16::try_from(saturation?.as_u64()?).ok()?;
    Some(hs_value(hue, saturation))
}

/// The `val` of `reachable`.
pub fn reachable_value(reachable: bool) -> (StatusPath, Value) {
    (StatusPath::Reachable, json!(reachable))
}

/// The `val` of `effect`.
pub fn effect_value(effect: &str) -> (StatusPath, Value) {
    (StatusPath::Effect, json!(effect))
}

/// The `val` of a `touch` event. A gesture not tied to one panel has no `panel`.
pub fn touch_value(panel: Option<u32>, gesture: Gesture) -> Value {
    let mut object = Map::new();
    if let Some(panel) = panel {
        object.insert("panel".to_owned(), json!(panel));
    }
    object.insert("gesture".to_owned(), json!(gesture_name(gesture)));
    Value::Object(object)
}

/// Contract name of a gesture.
pub fn gesture_name(gesture: Gesture) -> String {
    match gesture {
        Gesture::SingleTap => "tap".to_owned(),
        Gesture::DoubleTap => "double_tap".to_owned(),
        Gesture::SwipeUp => "swipe_up".to_owned(),
        Gesture::SwipeDown => "swipe_down".to_owned(),
        Gesture::SwipeLeft => "swipe_left".to_owned(),
        Gesture::SwipeRight => "swipe_right".to_owned(),
        Gesture::Unknown(id) => format!("unknown_{id}"),
    }
}

/// Contract name of a colour mode: the device's own name.
pub fn color_mode(mode: &ColorMode) -> &str {
    match mode {
        ColorMode::Effect => "effect",
        ColorMode::HueSaturation => "hs",
        ColorMode::ColorTemperature => "ct",
        ColorMode::Unknown(name) => name,
    }
}

/// `{model, firmware, name, panels}` with the fields the device reported; `None` if none.
fn device_value(info: &DeviceInfo) -> Option<Value> {
    let mut object = Map::new();
    if let Some(model) = &info.model {
        object.insert("model".to_owned(), json!(model));
    }
    if let Some(firmware) = &info.firmware {
        object.insert("firmware".to_owned(), json!(firmware));
    }
    if let Some(name) = &info.name {
        object.insert("name".to_owned(), json!(name));
    }
    if let Some(panels) = info.panel_count {
        object.insert("panels".to_owned(), json!(panels));
    }
    (!object.is_empty()).then_some(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LightState, Ranges};

    #[test]
    fn full_snapshot() {
        let snapshot = DeviceSnapshot {
            info: DeviceInfo {
                name: Some("Test Shapes".to_owned()),
                serial: Some("S-1".to_owned()),
                model: Some("NL42".to_owned()),
                firmware: Some("12.0.3".to_owned()),
                panel_count: Some(15),
                ..DeviceInfo::default()
            },
            state: LightState {
                on: Some(true),
                brightness: Some(30),
                hue: Some(120),
                saturation: Some(80),
                color_temperature: Some(2700),
                color_mode: Some(ColorMode::Effect),
                effect: Some("*Shuffle*".to_owned()),
                ranges: Ranges::default(),
            },
            effects: Some(vec!["A".to_owned(), "B".to_owned()]),
        };
        let values = snapshot_values(&snapshot);
        assert_eq!(
            values,
            vec![
                (StatusPath::On, json!(true)),
                (StatusPath::Brightness, json!(30)),
                (StatusPath::Hue, json!(120)),
                (StatusPath::Saturation, json!(80)),
                (StatusPath::HueSaturation, json!([120, 80])),
                (StatusPath::ColorTemperature, json!(2700)),
                (StatusPath::ColorMode, json!("effect")),
                (StatusPath::Effect, json!("*Shuffle*")),
                (StatusPath::Effects, json!(["A", "B"])),
                (
                    StatusPath::Device,
                    json!({"model":"NL42","firmware":"12.0.3","name":"Test Shapes","panels":15})
                ),
            ]
        );
        // The serial number never leaves the bridge.
        assert!(!format!("{values:?}").contains("S-1"));
    }

    #[test]
    fn absent_values_yield_no_topic() {
        assert!(snapshot_values(&DeviceSnapshot::default()).is_empty());
        let snapshot = DeviceSnapshot {
            info: DeviceInfo {
                model: Some("NL42".to_owned()),
                ..DeviceInfo::default()
            },
            ..DeviceSnapshot::default()
        };
        assert_eq!(
            snapshot_values(&snapshot),
            vec![(StatusPath::Device, json!({"model":"NL42"}))]
        );
    }

    #[test]
    fn hs_needs_both_values() {
        let only_hue = DeviceSnapshot {
            state: LightState {
                hue: Some(10),
                ..LightState::default()
            },
            ..DeviceSnapshot::default()
        };
        assert_eq!(
            snapshot_values(&only_hue),
            vec![(StatusPath::Hue, json!(10))]
        );
        assert_eq!(
            hs_from(Some(&json!(10)), Some(&json!(20))),
            Some((StatusPath::HueSaturation, json!([10, 20])))
        );
        assert_eq!(hs_from(Some(&json!(10)), None), None);
        assert_eq!(hs_from(None, Some(&json!(20))), None);
        assert_eq!(hs_from(Some(&json!("10")), Some(&json!(20))), None);
    }

    #[test]
    fn changes_and_modes() {
        assert_eq!(
            change_value(&StateChange::ColorMode(ColorMode::HueSaturation)),
            (StatusPath::ColorMode, json!("hs"))
        );
        assert_eq!(
            change_value(&StateChange::ColorMode(ColorMode::Unknown(
                "rhythm".to_owned()
            ))),
            (StatusPath::ColorMode, json!("rhythm"))
        );
        assert_eq!(
            change_value(&StateChange::ColorTemperature(4000)),
            (StatusPath::ColorTemperature, json!(4000))
        );
        assert_eq!(
            change_value(&StateChange::On(false)),
            (StatusPath::On, json!(false))
        );
    }

    #[test]
    fn layout_maps_shapes_and_keeps_the_numbers() {
        use crate::model::PanelPosition;
        let snapshot = DeviceSnapshot {
            info: DeviceInfo {
                layout: Some(Layout {
                    orientation: Some(120),
                    side_length: None,
                    panels: vec![
                        PanelPosition {
                            id: 1001,
                            x: Some(201),
                            y: Some(77),
                            rotation: Some(0),
                            shape_type: Some(8),
                        },
                        PanelPosition {
                            id: 7,
                            shape_type: Some(4242),
                            ..PanelPosition::default()
                        },
                        PanelPosition {
                            id: 8,
                            ..PanelPosition::default()
                        },
                    ],
                }),
                ..DeviceInfo::default()
            },
            ..DeviceSnapshot::default()
        };
        assert_eq!(
            snapshot_values(&snapshot),
            vec![(
                StatusPath::Layout,
                json!({
                    "orientation": 120,
                    "panels": [
                        {"id": 1001, "x": 201, "y": 77, "rotation": 0,
                         "shape": "triangle_shapes", "shape_type": 8},
                        {"id": 7, "shape": "unknown", "shape_type": 4242},
                        {"id": 8}
                    ]
                })
            )]
        );
    }

    #[test]
    fn shape_names_are_snake_case() {
        assert_eq!(shape_name(12), "controller");
        assert_eq!(shape_name(7), "hexagon_shapes");
        assert_eq!(shape_name(6), "unknown");
        assert!(
            (0..64)
                .map(shape_name)
                .all(|n| n.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        );
    }

    #[test]
    fn touch_and_gestures() {
        assert_eq!(
            touch_value(Some(1234), Gesture::SwipeUp),
            json!({"panel":1234,"gesture":"swipe_up"})
        );
        assert_eq!(
            touch_value(None, Gesture::SwipeLeft),
            json!({"gesture":"swipe_left"})
        );
        let names: Vec<String> = [
            Gesture::SingleTap,
            Gesture::DoubleTap,
            Gesture::SwipeUp,
            Gesture::SwipeDown,
            Gesture::SwipeLeft,
            Gesture::SwipeRight,
            Gesture::Unknown(9),
        ]
        .into_iter()
        .map(gesture_name)
        .collect();
        assert_eq!(
            names,
            [
                "tap",
                "double_tap",
                "swipe_up",
                "swipe_down",
                "swipe_left",
                "swipe_right",
                "unknown_9"
            ]
        );
    }
}
