//! The one place that assembles topic strings. Every topic of the contract is a method or an enum
//! variant here; a test keeps other files from spelling out topic levels.

use super::payload::SERVICE_NAME;
use crate::model::{ConfigError, DeviceId, validate_prefix};

/// The paths below `<prefix>/status/<id>/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatusPath {
    /// `online`: whether the device is reachable
    Online,
    /// `on`
    On,
    /// `brightness`
    Brightness,
    /// `hue`
    Hue,
    /// `sat`
    Saturation,
    /// `hs`: hue and saturation together, derived from the two
    HueSaturation,
    /// `ct`
    ColorTemperature,
    /// `colormode`
    ColorMode,
    /// `effect`
    Effect,
    /// `effects`
    Effects,
    /// `device`
    Device,
    /// `layout`: the panels' geometry
    Layout,
    /// `touch` (not retained)
    Touch,
}

impl StatusPath {
    /// Every status path of the contract.
    pub const ALL: [Self; 13] = [
        Self::Online,
        Self::On,
        Self::Brightness,
        Self::Hue,
        Self::Saturation,
        Self::HueSaturation,
        Self::ColorTemperature,
        Self::ColorMode,
        Self::Effect,
        Self::Effects,
        Self::Device,
        Self::Layout,
        Self::Touch,
    ];

    /// The topic level.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::On => "on",
            Self::Brightness => "brightness",
            Self::Hue => "hue",
            Self::Saturation => "sat",
            Self::HueSaturation => "hs",
            Self::ColorTemperature => "ct",
            Self::ColorMode => "colormode",
            Self::Effect => "effect",
            Self::Effects => "effects",
            Self::Device => "device",
            Self::Layout => "layout",
            Self::Touch => "touch",
        }
    }

    /// Whether the topic is retained (everything but `touch`).
    pub fn retained(self) -> bool {
        self != Self::Touch
    }

    /// The path named by a topic level.
    pub fn parse(level: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == level)
    }
}

/// The paths below `<prefix>/set/<id>/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CommandPath {
    /// `on`
    On,
    /// `brightness`
    Brightness,
    /// `hue`
    Hue,
    /// `sat`
    Saturation,
    /// `hs`: hue and saturation in one command
    HueSaturation,
    /// `ct`
    ColorTemperature,
    /// `effect`
    Effect,
    /// `identify`
    Identify,
}

impl CommandPath {
    /// Every command path of the contract.
    pub const ALL: [Self; 8] = [
        Self::On,
        Self::Brightness,
        Self::Hue,
        Self::Saturation,
        Self::HueSaturation,
        Self::ColorTemperature,
        Self::Effect,
        Self::Identify,
    ];

    /// The topic level.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Brightness => "brightness",
            Self::Hue => "hue",
            Self::Saturation => "sat",
            Self::HueSaturation => "hs",
            Self::ColorTemperature => "ct",
            Self::Effect => "effect",
            Self::Identify => "identify",
        }
    }

    /// The path named by a topic level.
    pub fn parse(level: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == level)
    }
}

/// A Home Assistant entity of one device, announced through MQTT discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HaEntity {
    /// The light itself.
    Light,
    /// Touch gestures, an `event` entity.
    Touch,
    /// Identify, a `button` entity.
    Identify,
}

impl HaEntity {
    /// Every entity announced per device.
    pub const ALL: [Self; 3] = [Self::Light, Self::Touch, Self::Identify];

    /// The Home Assistant component (the platform).
    pub fn component(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Touch => "event",
            Self::Identify => "button",
        }
    }

    /// The entity's part of ids and topics, unique within the device: the component id in the
    /// device-based discovery message and the end of the unique id.
    pub fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Touch => "touch",
            Self::Identify => "identify",
        }
    }
}

/// `<discovery prefix>/device/<node>/config`: where Home Assistant expects the device-based
/// discovery message of one device. The node is `nanoleaf2mqtt_<id>`; device ids only use
/// characters Home Assistant accepts there (`a-z0-9-`).
pub fn ha_discovery(discovery_prefix: &str, device: &DeviceId) -> String {
    format!("{discovery_prefix}/device/{}/config", ha_node(device))
}

/// `<discovery prefix>/<component>/<node>/<entity>/config`: where version 1 of the contract
/// announced each entity on its own. Only used to migrate and clear those messages.
pub fn ha_legacy_discovery(discovery_prefix: &str, device: &DeviceId, entity: HaEntity) -> String {
    format!(
        "{discovery_prefix}/{}/{}/{}/config",
        entity.component(),
        ha_node(device),
        entity.key()
    )
}

/// The subscriptions that read back every discovery message this service may have left under
/// `discovery_prefix`: the device-based ones and those of contract version 1. They match other
/// services' messages as well; [`parse_ha_discovery`] tells them apart.
pub fn ha_readback_filters(discovery_prefix: &str) -> Vec<String> {
    let mut filters = vec![format!("{discovery_prefix}/device/+/config")];
    filters.extend(HaEntity::ALL.into_iter().map(|entity| {
        format!(
            "{discovery_prefix}/{}/+/{}/config",
            entity.component(),
            entity.key()
        )
    }));
    filters
}

/// A discovery topic of this service, split up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HaDiscoveryTopic {
    /// `<ha>/device/nanoleaf2mqtt_<id>/config`
    Device(DeviceId),
    /// `<ha>/<component>/nanoleaf2mqtt_<id>/<entity>/config` (contract version 1)
    Legacy(DeviceId, HaEntity),
}

/// Splits a discovery topic of this service under `discovery_prefix`; `None` for any other
/// topic, including other services' discovery messages.
pub fn parse_ha_discovery(discovery_prefix: &str, topic: &str) -> Option<HaDiscoveryTopic> {
    let rest = topic
        .strip_prefix(discovery_prefix)?
        .strip_prefix('/')?
        .strip_suffix("/config")?;
    let levels: Vec<&str> = rest.split('/').collect();
    let device = |node: &str| {
        node.strip_prefix(SERVICE_NAME)?
            .strip_prefix('_')
            .and_then(|id| DeviceId::new(id).ok())
    };
    match levels.as_slice() {
        ["device", node] => device(node).map(HaDiscoveryTopic::Device),
        [component, node, key] => {
            let entity = HaEntity::ALL
                .into_iter()
                .find(|e| e.component() == *component && e.key() == *key)?;
            device(node).map(|id| HaDiscoveryTopic::Legacy(id, entity))
        }
        _ => None,
    }
}

/// `nanoleaf2mqtt_<id>`: the device's id towards Home Assistant (discovery node, device
/// identifier, start of every unique id).
pub fn ha_node(device: &DeviceId) -> String {
    format!("{SERVICE_NAME}_{}", device.as_str())
}

/// A topic below `<prefix>/status/`, split up. Neither level is validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusTopic {
    /// The device id level as found.
    pub device: String,
    /// Everything below the device level (may contain `/`, may be empty).
    pub path: String,
}

/// A topic below `<prefix>/set/`, split up. The device id is not validated against the
/// configuration here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTopic {
    /// The device id level as received.
    pub device: String,
    /// The command level as received.
    pub command: String,
}

/// Builds every topic of the contract from one prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topics {
    prefix: String,
}

impl Topics {
    /// Validates the prefix.
    pub fn new(prefix: impl Into<String>) -> Result<Self, ConfigError> {
        let prefix = prefix.into();
        validate_prefix(&prefix)?;
        Ok(Self { prefix })
    }

    /// The prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn join(&self, levels: &[&str]) -> String {
        let mut topic = self.prefix.clone();
        for level in levels {
            topic.push('/');
            topic.push_str(level);
        }
        topic
    }

    /// `<prefix>/connected`
    pub fn connected(&self) -> String {
        self.join(&["connected"])
    }

    /// `<prefix>/info`
    pub fn info(&self) -> String {
        self.join(&["info"])
    }

    /// `<prefix>/status/<id>/<path>`
    pub fn status(&self, device: &DeviceId, path: StatusPath) -> String {
        self.join(&["status", device.as_str(), path.as_str()])
    }

    /// `<prefix>/status/<id>/`, the start of every status topic of the device.
    pub fn status_prefix(&self, device: &DeviceId) -> String {
        self.join(&["status", device.as_str(), ""])
    }

    /// `<prefix>/status/#`, the subscription that reads back the retained status topics.
    pub fn status_wildcard(&self) -> String {
        self.join(&["status", "#"])
    }

    /// Splits `<prefix>/status/<id>/<path>`; `None` for a topic outside `<prefix>/status/`.
    pub fn parse_status(&self, topic: &str) -> Option<StatusTopic> {
        let rest = topic.strip_prefix(&self.join(&["status", ""]))?;
        let (device, path) = rest.split_once('/').unwrap_or((rest, ""));
        Some(StatusTopic {
            device: device.to_owned(),
            path: path.to_owned(),
        })
    }

    /// `<prefix>/set/<id>/<path>`
    pub fn set(&self, device: &DeviceId, path: CommandPath) -> String {
        self.join(&["set", device.as_str(), path.as_str()])
    }

    /// `<prefix>/set/#`, the subscription for commands.
    pub fn set_wildcard(&self) -> String {
        self.join(&["set", "#"])
    }

    /// Splits `<prefix>/set/<id>/<command>`; `None` for any other shape.
    pub fn parse_set(&self, topic: &str) -> Option<SetTopic> {
        let rest = topic.strip_prefix(&self.join(&["set", ""]))?;
        let (device, command) = rest.split_once('/')?;
        if device.is_empty() || command.is_empty() || command.contains('/') {
            return None;
        }
        Some(SetTopic {
            device: device.to_owned(),
            command: command.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topics() -> Topics {
        Topics::new("nanoleaf").unwrap()
    }

    fn dev() -> DeviceId {
        DeviceId::new("kellertreppe").unwrap()
    }

    #[test]
    fn every_topic() {
        let t = topics();
        assert_eq!(t.connected(), "nanoleaf/connected");
        assert_eq!(t.info(), "nanoleaf/info");
        assert_eq!(
            t.status(&dev(), StatusPath::Saturation),
            "nanoleaf/status/kellertreppe/sat"
        );
        assert_eq!(
            t.status(&dev(), StatusPath::Online),
            "nanoleaf/status/kellertreppe/online"
        );
        assert_eq!(t.status_prefix(&dev()), "nanoleaf/status/kellertreppe/");
        assert_eq!(
            t.set(&dev(), CommandPath::ColorTemperature),
            "nanoleaf/set/kellertreppe/ct"
        );
        assert_eq!(t.set_wildcard(), "nanoleaf/set/#");
        assert_eq!(t.status_wildcard(), "nanoleaf/status/#");
    }

    #[test]
    fn prefix_is_validated() {
        assert!(Topics::new("").is_err());
        assert!(Topics::new("a/#").is_err());
        assert_eq!(
            Topics::new("home/nl").unwrap().connected(),
            "home/nl/connected"
        );
    }

    #[test]
    fn set_topics_are_parsed() {
        let t = topics();
        assert_eq!(
            t.parse_set("nanoleaf/set/dev/on"),
            Some(SetTopic {
                device: "dev".to_owned(),
                command: "on".to_owned()
            })
        );
        for other in [
            "nanoleaf/set/dev",
            "nanoleaf/set//on",
            "nanoleaf/set/dev/",
            "nanoleaf/set/dev/on/x",
            "nanoleaf/status/dev/on",
            "other/set/dev/on",
            "nanoleafx/set/dev/on",
        ] {
            assert_eq!(t.parse_set(other), None, "{other}");
        }
    }

    #[test]
    fn status_topics_are_split() {
        let t = topics();
        let split = |topic| t.parse_status(topic).map(|s| (s.device, s.path));
        assert_eq!(
            split("nanoleaf/status/dev/on"),
            Some(("dev".to_owned(), "on".to_owned()))
        );
        assert_eq!(
            split("nanoleaf/status/dev/a/b"),
            Some(("dev".to_owned(), "a/b".to_owned()))
        );
        assert_eq!(
            split("nanoleaf/status/dev"),
            Some(("dev".to_owned(), String::new()))
        );
        assert_eq!(split("nanoleaf/set/dev/on"), None);
        assert_eq!(split("nanoleafx/status/dev/on"), None);
    }

    #[test]
    fn discovery_topics_are_built_and_recognised() {
        let id = dev();
        assert_eq!(
            ha_discovery("homeassistant", &id),
            "homeassistant/device/nanoleaf2mqtt_kellertreppe/config"
        );
        assert_eq!(
            ha_legacy_discovery("ha/x", &id, HaEntity::Touch),
            "ha/x/event/nanoleaf2mqtt_kellertreppe/touch/config"
        );
        assert_eq!(
            parse_ha_discovery("ha/x", &ha_discovery("ha/x", &id)),
            Some(HaDiscoveryTopic::Device(id.clone()))
        );
        for entity in HaEntity::ALL {
            assert_eq!(
                parse_ha_discovery("ha/x", &ha_legacy_discovery("ha/x", &id, entity)),
                Some(HaDiscoveryTopic::Legacy(id.clone(), entity))
            );
        }
        for other in [
            "ha/x/device/zigbee2mqtt_lamp/config",
            "ha/x/device/nanoleaf2mqtt_/config",
            "ha/x/device/nanoleaf2mqtt_Bad/config",
            "ha/x/light/nanoleaf2mqtt_kellertreppe/touch/config",
            "ha/x/light/nanoleaf2mqtt_kellertreppe/config",
            "ha/y/device/nanoleaf2mqtt_kellertreppe/config",
            "ha/x/device/nanoleaf2mqtt_kellertreppe/state",
        ] {
            assert_eq!(parse_ha_discovery("ha/x", other), None, "{other}");
        }
        assert_eq!(
            ha_readback_filters("ha"),
            [
                "ha/device/+/config",
                "ha/light/+/light/config",
                "ha/event/+/touch/config",
                "ha/button/+/identify/config",
            ]
        );
    }

    #[test]
    fn paths_round_trip() {
        for path in CommandPath::ALL {
            assert_eq!(CommandPath::parse(path.as_str()), Some(path));
        }
        assert_eq!(CommandPath::parse("online"), None);
        for path in StatusPath::ALL {
            assert_eq!(StatusPath::parse(path.as_str()), Some(path));
        }
        assert_eq!(StatusPath::parse("identify"), None);
        assert_eq!(StatusPath::parse("reachable"), None, "gone in version 2");
        assert!(
            StatusPath::ALL
                .iter()
                .all(|p| p.retained() == (*p != StatusPath::Touch))
        );
    }

    #[test]
    fn only_this_file_spells_out_topic_levels() {
        // The code that talks MQTT: the topic builder's users.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = [
            "\"connected\"",
            "\"info\"",
            "\"status\"",
            "\"set\"",
            "/status/",
            "/set/",
            "/connected",
            "/config",
        ];
        let mut files = Vec::new();
        let mut dirs = vec![src.join("mqtt"), src.join("bridge"), src.join("model")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        for path in files {
            if path.ends_with("mqtt/topic.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("#[cfg(test)]") {
                    break;
                }
                if trimmed.starts_with("//") {
                    continue;
                }
                for needle in forbidden {
                    assert!(
                        !trimmed.contains(needle),
                        "{}:{} spells out a topic level outside topic.rs: {line}",
                        path.display(),
                        number + 1
                    );
                }
            }
        }
    }
}
