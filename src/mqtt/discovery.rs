//! Home Assistant MQTT discovery: the retained configuration message that lets Home Assistant
//! find every device on its own, in the device-based form (mqtt-smarthome 2.0, section 8).
//!
//! Each device becomes one Home Assistant device, announced in one message with three
//! components: the light, an `event` entity for touch gestures and a `button` for identify. The
//! message points directly at the contract's status and command topics and reads `val` through
//! value templates; there are no extra state topics. An entity is available while the service
//! is up (`connected` is `2`) and its own device is online, so one unreachable device does not
//! affect the others. The `online` entry is only named once the device's `online` topic has a
//! value: Home Assistant shows an entity as unavailable until every availability topic it names
//! has delivered one.
//!
//! Only what the device reported goes into the message (model, firmware, colour temperature
//! range, effect list); nothing about how the device or the broker is reached.
//!
//! Version 1 of the contract announced each entity on its own topic. Such a message is moved
//! over with Home Assistant's migration request (`{"migrate_discovery": true}` on the old topic,
//! then the device-based message, then the old topic cleared), which keeps the entities and
//! their settings; the unique ids are the same in both versions.
//!
//! Home Assistant documentation: <https://www.home-assistant.io/integrations/mqtt/> (device
//! discovery payload, migration from single component to device-based discovery),
//! <https://www.home-assistant.io/integrations/light.mqtt/>,
//! <https://www.home-assistant.io/integrations/event.mqtt/>,
//! <https://www.home-assistant.io/integrations/button.mqtt/>.

use serde_json::{Map, Value, json};

use super::map::gesture_name;
use super::payload::SERVICE_NAME;
use super::topic::{CommandPath, HaEntity, StatusPath, Topics, ha_discovery, ha_node};
use crate::model::{DeviceId, DeviceSnapshot, Gesture, ValueRange};

/// Public home of the service, named in the discovery `origin`.
const SUPPORT_URL: &str = "https://github.com/boesec/nanoleaf2mqtt";

/// `connected` → availability:`2` (and above) counts as available, `0`, a cleared topic and
/// anything else not.
const CONNECTED_TEMPLATE: &str = "{{ 'online' if value | int(0) >= 2 else 'offline' }}";
/// `online` → availability; a cleared topic counts as offline.
const ONLINE_TEMPLATE: &str =
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
const GESTURES: [Gesture; 7] = [
    Gesture::SingleTap,
    Gesture::DoubleTap,
    Gesture::SwipeUp,
    Gesture::SwipeDown,
    Gesture::SwipeLeft,
    Gesture::SwipeRight,
    Gesture::LongPress,
];

/// What the discovery message says about a device, as far as the device reported it.
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

/// The discovery topics of a device: the one device-based message.
pub fn topics(discovery_prefix: &str, device: &DeviceId) -> Vec<String> {
    vec![ha_discovery(discovery_prefix, device)]
}

/// The discovery message of a device: topic and payload. `online_known` says whether the
/// device's `online` topic has a value, so that it can be named for availability.
pub fn message(
    topics: &Topics,
    discovery_prefix: &str,
    device: &DeviceId,
    about: &DiscoveryDevice,
    online_known: bool,
) -> (String, Value) {
    (
        ha_discovery(discovery_prefix, device),
        config(topics, device, about, online_known),
    )
}

/// The device-based discovery payload of a device.
pub fn config(
    topics: &Topics,
    device: &DeviceId,
    about: &DiscoveryDevice,
    online_known: bool,
) -> Value {
    let mut availability = vec![availability_entry(topics.connected(), CONNECTED_TEMPLATE)];
    if online_known {
        availability.push(availability_entry(
            topics.status(device, StatusPath::Online),
            ONLINE_TEMPLATE,
        ));
    }
    let components: Map<String, Value> = HaEntity::ALL
        .into_iter()
        .map(|entity| {
            (
                entity.key().to_owned(),
                component(topics, device, about, entity),
            )
        })
        .collect();
    json!({
        "device": device_block(device, about),
        "origin": {
            "name": SERVICE_NAME,
            "sw_version": env!("CARGO_PKG_VERSION"),
            "support_url": SUPPORT_URL,
        },
        "availability": availability,
        "availability_mode": "all",
        "qos": 0,
        "components": components,
    })
}

/// One entry of the availability list: only the keys mqtt-smarthome 2.0 allows there (Home
/// Assistant rejects the whole message over an unknown key in a list entry).
fn availability_entry(topic: String, template: &str) -> Value {
    json!({
        "topic": topic,
        "payload_available": "online",
        "payload_not_available": "offline",
        "value_template": template,
    })
}

/// The configuration of one component of the device-based message.
pub fn component(
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
    put("p", json!(entity.component()));
    put("unique_id", json!(format!("{node}_{}", entity.key())));
    put(
        "default_entity_id",
        json!(default_entity_id(device, entity)),
    );
    let status = |path| json!(topics.status(device, path));
    let set = |path| json!(topics.set(device, path));
    match entity {
        HaEntity::Light => {
            // The light is the device itself: it takes the device's name.
            put("name", Value::Null);
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
            put("command_topic", set(CommandPath::Identify));
        }
    }
    Value::Object(object)
}

/// Home Assistant's migration request, published on a version 1 topic before the device-based
/// message takes over its entity.
pub fn migrate_request() -> Value {
    json!({"migrate_discovery": true})
}

/// Whether a retained payload is a migration request.
pub fn is_migrate_request(payload: &[u8]) -> bool {
    serde_json::from_slice::<Value>(payload).is_ok_and(|v| {
        v.get("migrate_discovery")
            .or_else(|| v.get("migr_discvry"))
            .is_some_and(|m| *m == Value::Bool(true))
    })
}

/// Whether a retained discovery payload was published by the service instance whose
/// `connected` topic is `connected`: its availability names that topic. Another instance on the
/// same broker (another prefix) announces its devices under the same discovery prefix, and its
/// messages are not this instance's to clear.
pub fn published_by(payload: &[u8], connected: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(payload) else {
        return false;
    };
    let list = value.get("availability").or_else(|| value.get("avty"));
    list.and_then(Value::as_array).is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry
                .get("topic")
                .or_else(|| entry.get("t"))
                .and_then(Value::as_str)
                == Some(connected)
        })
    })
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
        let payload = config(&topics, &id, &DiscoveryDevice::default(), true);
        let light = &payload["components"]["light"];
        for key in ["min_kelvin", "max_kelvin", "effect_list"] {
            assert!(light.get(key).is_none(), "{key}");
        }
        assert_eq!(
            payload["device"],
            json!({"identifiers": ["nanoleaf2mqtt_my-dev"], "name": "my-dev"})
        );
        assert_eq!(light["default_entity_id"], "light.nanoleaf_my_dev");
    }

    #[test]
    fn the_online_entry_waits_for_a_value() {
        let topics = Topics::new("nanoleaf").unwrap();
        let id = DeviceId::new("dev").unwrap();
        let about = DiscoveryDevice::default();
        let without = config(&topics, &id, &about, false);
        assert_eq!(without["availability"].as_array().unwrap().len(), 1);
        let with = config(&topics, &id, &about, true);
        assert_eq!(
            with["availability"][1]["topic"],
            "nanoleaf/status/dev/online"
        );
    }

    #[test]
    fn the_serial_number_is_not_announced() {
        let mut about = DiscoveryDevice::default();
        about.update(&snapshot());
        let topics = Topics::new("nanoleaf").unwrap();
        let id = DeviceId::new("dev").unwrap();
        let (topic, payload) = message(&topics, "homeassistant", &id, &about, true);
        assert!(!payload.to_string().contains("S-1"));
        assert_eq!(vec![topic], super::topics("homeassistant", &id));
    }

    #[test]
    fn ownership_and_migration_requests_are_recognised() {
        let topics = Topics::new("nanoleaf").unwrap();
        let id = DeviceId::new("dev").unwrap();
        let ours = config(&topics, &id, &DiscoveryDevice::default(), true).to_string();
        assert!(published_by(ours.as_bytes(), "nanoleaf/connected"));
        assert!(!published_by(ours.as_bytes(), "other/connected"));
        assert!(published_by(
            br#"{"avty":[{"t":"nanoleaf/connected"}]}"#,
            "nanoleaf/connected"
        ));
        assert!(!published_by(b"not json", "nanoleaf/connected"));
        assert!(!published_by(b"{}", "nanoleaf/connected"));
        assert!(is_migrate_request(migrate_request().to_string().as_bytes()));
        assert!(is_migrate_request(br#"{"migr_discvry":true}"#));
        assert!(!is_migrate_request(ours.as_bytes()));
    }
}
