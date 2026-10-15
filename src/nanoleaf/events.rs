//! Mapping of the device's event stream frames into [`DeviceEvent`]s.
//!
//! | `id` | Type | `events[]` items |
//! |---|---|---|
//! | 1 | state | `{"attr": n, "value": v}`: 1 on, 2 brightness, 3 hue, 4 sat, 5 ct, 6 colour mode |
//! | 2 | layout | layout changed |
//! | 3 | effects | `{"attr": 1, "value": "<effect name>"}` |
//! | 4 | touch | `{"panelId": n, "gesture": g}`: 0 single tap, 1 double tap, 2–5 swipe up, down, left, right |
//!
//! Unknown ids, attributes and malformed items are ignored (logged at debug level).

use serde_json::Value;

use super::parse::{color_mode, number};
use super::sse::SseFrame;
use crate::model::{DeviceEvent, Gesture, StateChange};

/// Maps a gesture id of the device.
pub fn gesture(id: u8) -> Gesture {
    match id {
        0 => Gesture::SingleTap,
        1 => Gesture::DoubleTap,
        2 => Gesture::SwipeUp,
        3 => Gesture::SwipeDown,
        4 => Gesture::SwipeLeft,
        5 => Gesture::SwipeRight,
        other => Gesture::Unknown(other),
    }
}

/// Maps one item of a state frame (`id: 1`). `None` for unknown attributes and values of the
/// wrong type.
pub fn state_change(attr: u64, value: &Value) -> Option<StateChange> {
    match attr {
        1 => value.as_bool().map(StateChange::On),
        2 => number(value).map(StateChange::Brightness),
        3 => number(value).map(StateChange::Hue),
        4 => number(value).map(StateChange::Saturation),
        5 => number(value).map(StateChange::ColorTemperature),
        6 => value
            .as_str()
            .map(|name| StateChange::ColorMode(color_mode(name))),
        _ => None,
    }
}

/// Maps one frame into the events it carries. Frames that cannot be understood yield nothing.
pub fn decode(frame: &SseFrame) -> Vec<DeviceEvent> {
    let Some(id) = frame.id.as_deref().map(str::trim) else {
        tracing::debug!("event frame without id ignored");
        return Vec::new();
    };
    if !matches!(id, "1" | "2" | "3" | "4") {
        tracing::debug!(id, "event frame with unknown id ignored");
        return Vec::new();
    }
    let payload: Value = match serde_json::from_str(&frame.data) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::debug!(id, error = %err, "event frame with invalid JSON ignored");
            return Vec::new();
        }
    };
    let items = payload["events"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    match id {
        "1" => {
            let changes: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    let change = item["attr"]
                        .as_u64()
                        .and_then(|attr| state_change(attr, &item["value"]));
                    if change.is_none() {
                        tracing::debug!(%item, "state event item ignored");
                    }
                    change
                })
                .collect();
            if changes.is_empty() {
                Vec::new()
            } else {
                vec![DeviceEvent::StateChanged(changes)]
            }
        }
        "2" => vec![DeviceEvent::LayoutChanged],
        "3" => items
            .iter()
            .filter_map(
                |item| match (item["attr"].as_u64(), item["value"].as_str()) {
                    (Some(1), Some(name)) => Some(DeviceEvent::EffectChanged(name.to_owned())),
                    _ => {
                        tracing::debug!(%item, "effect event item ignored");
                        None
                    }
                },
            )
            .collect(),
        _ => items.iter().filter_map(touch).collect(),
    }
}

fn touch(item: &Value) -> Option<DeviceEvent> {
    let Some(id) = item["gesture"].as_u64().and_then(|g| u8::try_from(g).ok()) else {
        tracing::debug!(%item, "touch event item ignored");
        return None;
    };
    // Gestures that are not tied to a panel (swipes) report a negative panel id.
    let panel_id = item["panelId"].as_u64().and_then(|p| u32::try_from(p).ok());
    Some(DeviceEvent::Touch {
        panel_id,
        gesture: gesture(id),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::ColorMode;

    fn frame(id: &str, data: &str) -> SseFrame {
        SseFrame {
            id: Some(id.to_owned()),
            data: data.to_owned(),
        }
    }

    #[test]
    fn gesture_mapping() {
        assert_eq!(gesture(0), Gesture::SingleTap);
        assert_eq!(gesture(1), Gesture::DoubleTap);
        assert_eq!(gesture(2), Gesture::SwipeUp);
        assert_eq!(gesture(3), Gesture::SwipeDown);
        assert_eq!(gesture(4), Gesture::SwipeLeft);
        assert_eq!(gesture(5), Gesture::SwipeRight);
        assert_eq!(gesture(6), Gesture::Unknown(6));
        assert_eq!(gesture(255), Gesture::Unknown(255));
    }

    #[test]
    fn state_attr_mapping() {
        assert_eq!(state_change(1, &json!(true)), Some(StateChange::On(true)));
        assert_eq!(
            state_change(2, &json!(44)),
            Some(StateChange::Brightness(44))
        );
        assert_eq!(state_change(3, &json!(120)), Some(StateChange::Hue(120)));
        assert_eq!(
            state_change(4, &json!(80)),
            Some(StateChange::Saturation(80))
        );
        assert_eq!(
            state_change(5, &json!(4000)),
            Some(StateChange::ColorTemperature(4000))
        );
        assert_eq!(
            state_change(6, &json!("hs")),
            Some(StateChange::ColorMode(ColorMode::HueSaturation))
        );
        assert_eq!(
            state_change(6, &json!("ct")),
            Some(StateChange::ColorMode(ColorMode::ColorTemperature))
        );
        assert_eq!(
            state_change(6, &json!("effect")),
            Some(StateChange::ColorMode(ColorMode::Effect))
        );
        assert_eq!(state_change(7, &json!(1)), None, "unknown attr");
        assert_eq!(state_change(0, &json!(1)), None, "unknown attr");
        assert_eq!(state_change(1, &json!("yes")), None, "wrong type");
        assert_eq!(state_change(2, &json!(-5)), None, "negative");
    }

    #[test]
    fn state_frame_with_several_attributes() {
        let events = decode(&frame(
            "1",
            r#"{"events":[{"attr":1,"value":false},{"attr":2,"value":0},{"attr":9,"value":1}]}"#,
        ));
        assert_eq!(
            events,
            vec![DeviceEvent::StateChanged(vec![
                StateChange::On(false),
                StateChange::Brightness(0)
            ])]
        );
    }

    #[test]
    fn layout_frame() {
        assert_eq!(
            decode(&frame("2", r#"{"events":[{"attr":1,"value":{}}]}"#)),
            vec![DeviceEvent::LayoutChanged]
        );
    }

    #[test]
    fn effect_frame() {
        assert_eq!(
            decode(&frame(
                "3",
                r#"{"events":[{"attr":1,"value":"*Shuffle*"}]}"#
            )),
            vec![DeviceEvent::EffectChanged("*Shuffle*".to_owned())]
        );
        assert_eq!(
            decode(&frame("3", r#"{"events":[{"attr":2,"value":"x"}]}"#)),
            vec![]
        );
    }

    #[test]
    fn touch_frame() {
        let events = decode(&frame(
            "4",
            r#"{"events":[{"panelId":1001,"gesture":0},{"panelId":-1,"gesture":2},{"panelId":7,"gesture":42},{"panelId":7}]}"#,
        ));
        assert_eq!(
            events,
            vec![
                DeviceEvent::Touch {
                    panel_id: Some(1001),
                    gesture: Gesture::SingleTap
                },
                DeviceEvent::Touch {
                    panel_id: None,
                    gesture: Gesture::SwipeUp
                },
                DeviceEvent::Touch {
                    panel_id: Some(7),
                    gesture: Gesture::Unknown(42)
                },
            ]
        );
    }

    #[test]
    fn unknown_id_bad_json_and_odd_shapes_yield_nothing() {
        assert_eq!(decode(&frame("5", r#"{"events":[]}"#)), vec![]);
        assert_eq!(decode(&frame("x", "{}")), vec![]);
        assert_eq!(decode(&frame("1", "{not json")), vec![]);
        assert_eq!(decode(&frame("1", r#"{"events":"nope"}"#)), vec![]);
        assert_eq!(decode(&frame("1", "[]")), vec![]);
        assert_eq!(decode(&frame("4", r#"{"events":[1,2]}"#)), vec![]);
        assert_eq!(
            decode(&SseFrame {
                id: None,
                data: "{}".to_owned()
            }),
            vec![]
        );
    }

    #[test]
    fn id_with_surrounding_space() {
        assert_eq!(
            decode(&frame(" 3 ", r#"{"events":[{"attr":1,"value":"Prism"}]}"#)),
            vec![DeviceEvent::EffectChanged("Prism".to_owned())]
        );
    }
}
