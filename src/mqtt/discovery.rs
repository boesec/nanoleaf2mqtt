//! Home Assistant MQTT discovery: the retained configuration messages that let Home Assistant
//! find every device on its own.
//!
//! Each device becomes one Home Assistant device with three entities: the light, an `event`
//! entity for touch gestures and a `button` for identify. The messages point directly at the
//! contract's status and command topics and read `val` through value templates; there are no
//! extra state topics. An entity is available while the service is up (`connected` is `1` or `2`)
//! and its own device is reachable, so one unreachable device does not affect the others.
//!
//! Only what the device reported goes into the messages (model, firmware, colour temperature
//! range, effect list); nothing about how the device or the broker is reached.
//!
//! Home Assistant documentation: <https://www.home-assistant.io/integrations/mqtt/>,
//! <https://www.home-assistant.io/integrations/light.mqtt/>,
//! <https://www.home-assistant.io/integrations/event.mqtt/>,
//! <https://www.home-assistant.io/integrations/button.mqtt/>.

use serde_json::{Map, Value, json};

use super::map::gesture_name;
use super::payload::SERVICE_NAME;
use super::topic::{CommandPath, HaEntity, StatusPath, Topics, ha_discovery, ha_node};
use crate::model::{DeviceId, DeviceSnapshot, Gesture, ValueRange};

/// `connected` → availability: `1` and `2` (service up) count as available, `0` and anything else not.
const CONNECTED_TEMPLATE: &str = "{{ 'online' if value in ['1', '2'] else 'offline' }}";
/// `reachable` → availability; a cleared topic counts as not reachable.
const REACHABLE_TEMPLATE: &str =
    "{{ 'online' if value_json is defined and value_json.val == true else 'offline' }}";
/// `val` of a status topic as it is.
const VAL_TEMPLATE: &str = "{{ value_json.val }}";
/// `on` → the light's `payload_on` / `payload_off`.
const ON_TEMPLATE: &str = "{{ 'true' if value_json.val else 'false' }}";
/// `hs` → `h,s`, the form Home Assistant reads.
const HS_TEMPLATE: &str = "{{ value_json.val | join(',') }}";
/// Home Assistant's hue and saturation (fractional) → whole numbers as the contract takes them.
const HS_COMMAND_TEMPLATE: &str = "{{ hue | round | int }},{{ sat | round | int }}";
/// A touch → the event payload: the touch object plus `event_type`, the gesture.
const TOUCH_TEMPLATE: &str =
    "{{ dict(value_json.val, event_type=value_json.val.gesture) | to_json }}";

/// The gestures announced as event types: those the contract names (`unknown_<n>` is left out,
/// it is not a fixed name).
const GESTURES: [Gesture; 6] = [
    Gesture::SingleTap,
    Gesture::DoubleTap,
    Gesture::SwipeUp,
    Gesture::SwipeDown,
    Gesture::SwipeLeft,
    Gesture::SwipeRight,
];

/// What the discovery messages say about a device, as far as the device reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryDevice {
    /// Name the device reports for itself.
    pub name: Option<String>,
    /// Manufacturer.
    pub manufacturer: Option<String>,
    /// Model.
    pub model: Option<String>,
    /// Firmware version.
    pub firmware: Option<String>,
    /// Colour temperature range in Kelvin.
    pub color_temperature: Option<ValueRange>,
    /// The effect list.
    pub effects: Option<Vec<String>>,
}

impl DiscoveryDevice {
    /// Takes over what the snapshot reports; what it does not report stays as it was (like a
    /// status topic keeps its last value). Returns whether anything changed.
    pub fn update(&mut self, snapshot: &DeviceSnapshot) -> bool {
        let before = self.clone();
        let info = &snapshot.info;
        take(&mut self.name, &info.name);
        take(&mut self.manufacturer, &info.manufacturer);
        take(&mut self.model, &info.model);
        take(&mut self.firmware, &info.firmware);
        take(
            &mut self.color_temperature,
            &snapshot.state.ranges.color_temperature,
        );
        take(&mut self.effects, &snapshot.effects);
        *self != before
    }
}

fn take<T: Clone>(slot: &mut Option<T>, reported: &Option<T>) {
    if let Some(value) = reported {
        *slot = Some(value.clone());
    }
}

/// The discovery topic of every entity of a device.
pub fn topics(discovery_prefix: &str, device: &DeviceId) -> Vec<String> {
    HaEntity::ALL
        .into_iter()
        .map(|entity| ha_discovery(discovery_prefix, device, entity))
        .collect()
}

/// Every discovery message of a device: topic and payload.
pub fn messages(
    topics: &Topics,
    discovery_prefix: &str,
    device: &DeviceId,
    about: &DiscoveryDevice,
) -> Vec<(String, Value)> {
    HaEntity::ALL
        .into_iter()
        .map(|entity| {
            (
                ha_discovery(discovery_prefix, device, entity),
                config(topics, device, about, entity),
            )
        })
        .collect()
}

/// The discovery payload of one entity.
pub fn config(
    topics: &Topics,
    device: &DeviceId,
    about: &DiscoveryDevice,
    entity: HaEntity,
) -> Value {
    let node = ha_node(device);
    let mut object = Map::new();
    let mut put = |key: &str, value: Value| {
        object.insert(key.to_owned(), value);
    };
    put("unique_id", json!(format!("{node}_{}", entity.key())));
    put(
        "default_entity_id",
        json!(default_entity_id(device, entity)),
    );
    put("device", device_block(device, about));
    put(
        "origin",
        json!({"name": SERVICE_NAME, "sw_version": env!("CARGO_PKG_VERSION")}),
    );
    put(
        "availability",
        json!([
            {"topic": topics.connected(), "value_template": CONNECTED_TEMPLATE},
            {
                "topic": topics.status(device, StatusPath::Reachable),
                "value_template": REACHABLE_TEMPLATE
            },
        ]),
    );
    put("availability_mode", json!("all"));
    let status = |path| json!(topics.status(device, path));
    let set = |path| json!(topics.set(device, path));
    match entity {
        HaEntity::Light => {
            // The light is the device itself: it takes the device's name.
            put("name", Value::Null);
            put("qos", json!(1));
            put("state_topic", status(StatusPath::On));
            put("state_value_template", json!(ON_TEMPLATE));
            put("command_topic", set(CommandPath::On));
            put("payload_on", json!("true"));
            put("payload_off", json!("false"));
            put("brightness_state_topic", status(StatusPath::Brightness));
            put("brightness_value_template", json!(VAL_TEMPLATE));
            put("brightness_command_topic", set(CommandPath::Brightness));
            put("brightness_scale", json!(100));
            put("hs_state_topic", status(StatusPath::HueSaturation));
            put("hs_value_template", json!(HS_TEMPLATE));
            put("hs_command_topic", set(CommandPath::HueSaturation));
            put("hs_command_template", json!(HS_COMMAND_TEMPLATE));
            put("color_temp_kelvin", json!(true));
            put(
                "color_temp_state_topic",
                status(StatusPath::ColorTemperature),
            );
            put("color_temp_value_template", json!(VAL_TEMPLATE));
            put(
                "color_temp_command_topic",
                set(CommandPath::ColorTemperature),
            );
            if let Some(range) = about.color_temperature {
                put("min_kelvin", json!(range.min));
                put("max_kelvin", json!(range.max));
            }
            put("effect_state_topic", status(StatusPath::Effect));
            put("effect_value_template", json!(VAL_TEMPLATE));
            put("effect_command_topic", set(CommandPath::Effect));
            if let Some(effects) = &about.effects {
                put("effect_list", json!(effects));
            }
        }
        HaEntity::Touch => {
            put("name", json!("Touch"));
            put("device_class", json!("button"));
            put("state_topic", status(StatusPath::Touch));
            put("value_template", json!(TOUCH_TEMPLATE));
            put(
                "event_types",
                json!(GESTURES.into_iter().map(gesture_name).collect::<Vec<_>>()),
            );
        }
        HaEntity::Identify => {
            put("name", json!("Identify"));
            put("device_class", json!("identify"));
            put("entity_category", json!("config"));
            put("qos", json!(1));
            put("command_topic", set(CommandPath::Identify));
        }
    }
    Value::Object(object)
}

/// `light.nanoleaf_<id>`, `event.nanoleaf_<id>_touch`, `button.nanoleaf_<id>_identify`; entity
/// ids take no `-`.
fn default_entity_id(device: &DeviceId, entity: HaEntity) -> String {
    let id = device.as_str().replace('-', "_");
    match entity {
        HaEntity::Light => format!("{}.nanoleaf_{id}", entity.component()),
        HaEntity::Touch | HaEntity::Identify => {
            format!("{}.nanoleaf_{id}_{}", entity.component(), entity.key())
        }
    }
}

/// The Home Assistant device: identified by the device id; the name the device reports (the id
/// while it has not reported one), and what it reports about itself.
fn device_block(device: &DeviceId, about: &DiscoveryDevice) -> Value {
    let mut object = Map::new();
    object.insert("identifiers".to_owned(), json!([ha_node(device)]));
    object.insert(
        "name".to_owned(),
        json!(about.name.as_deref().unwrap_or(device.as_str())),
    );
    for (key, value) in [
        ("manufacturer", &about.manufacturer),
        ("model", &about.model),
        ("sw_version", &about.firmware),
    ] {
        if let Some(value) = value {
            object.insert(key.to_owned(), json!(value));
        }
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeviceInfo, LightState, Ranges};

    fn snapshot() -> DeviceSnapshot {
        DeviceSnapshot {
            info: DeviceInfo {
                name: Some("Shapes".to_owned()),
                manufacturer: Some("Nanoleaf".to_owned()),
                model: Some("NL42".to_owned()),
                firmware: Some("12.0.3".to_owned()),
                serial: Some("S-1".to_owned()),
                ..DeviceInfo::default()
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
            effects: Some(vec!["A".to_owned()]),
        }
    }

    #[test]
    fn update_keeps_what_a_snapshot_does_not_report() {
        let mut about = DiscoveryDevice::default();
        assert!(about.update(&snapshot()));
        assert!(!about.update(&snapshot()), "nothing new");
        assert!(!about.update(&DeviceSnapshot::default()), "absent keeps");
        assert_eq!(about.model.as_deref(), Some("NL42"));
        let mut more = snapshot();
        more.effects = Some(vec!["A".to_owned(), "B".to_owned()]);
        assert!(about.update(&more));
        assert_eq!(about.effects.as_ref().map(Vec::len), Some(2));
    }

    #[test]
    fn unknown_values_are_left_out() {
        let topics = Topics::new("nanoleaf").unwrap();
        let id = DeviceId::new("my-dev").unwrap();
        let light = config(&topics, &id, &DiscoveryDevice::default(), HaEntity::Light);
        for key in ["min_kelvin", "max_kelvin", "effect_list"] {
            assert!(light.get(key).is_none(), "{key}");
        }
        assert_eq!(
            light["device"],
            json!({"identifiers": ["nanoleaf2mqtt_my-dev"], "name": "my-dev"})
        );
        assert_eq!(light["default_entity_id"], "light.nanoleaf_my_dev");
    }

    #[test]
    fn the_serial_number_is_not_announced() {
        let mut about = DiscoveryDevice::default();
        about.update(&snapshot());
        let topics = Topics::new("nanoleaf").unwrap();
        let id = DeviceId::new("dev").unwrap();
        let all = messages(&topics, "homeassistant", &id, &about);
        assert_eq!(all.len(), 3);
        assert!(!format!("{all:?}").contains("S-1"));
        assert_eq!(
            all.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
            super::topics("homeassistant", &id)
        );
    }
}
