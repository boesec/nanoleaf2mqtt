//! The one place that assembles topic strings. Every topic of the contract is a method or an enum
//! variant here; a test keeps other files from spelling out topic levels.

use crate::model::{ConfigError, DeviceId, validate_prefix};

/// The paths below `<prefix>/status/<id>/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatusPath {
    /// `reachable`
    Reachable,
    /// `on`
    On,
    /// `brightness`
    Brightness,
    /// `hue`
    Hue,
    /// `sat`
    Saturation,
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
    /// `touch` (not retained)
    Touch,
}

impl StatusPath {
    /// Every status path of the contract.
    pub const ALL: [Self; 11] = [
        Self::Reachable,
        Self::On,
        Self::Brightness,
        Self::Hue,
        Self::Saturation,
        Self::ColorTemperature,
        Self::ColorMode,
        Self::Effect,
        Self::Effects,
        Self::Device,
        Self::Touch,
    ];

    /// The topic level.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::On => "on",
            Self::Brightness => "brightness",
            Self::Hue => "hue",
            Self::Saturation => "sat",
            Self::ColorTemperature => "ct",
            Self::ColorMode => "colormode",
            Self::Effect => "effect",
            Self::Effects => "effects",
            Self::Device => "device",
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
    /// `ct`
    ColorTemperature,
    /// `effect`
    Effect,
    /// `identify`
    Identify,
}

impl CommandPath {
    /// Every command path of the contract.
    pub const ALL: [Self; 7] = [
        Self::On,
        Self::Brightness,
        Self::Hue,
        Self::Saturation,
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
        assert_eq!(t.status_prefix(&dev()), "nanoleaf/status/kellertreppe/");
        assert_eq!(
            t.set(&dev(), CommandPath::ColorTemperature),
            "nanoleaf/set/kellertreppe/ct"
        );
        assert_eq!(t.set_wildcard(), "nanoleaf/set/#");
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
    fn paths_round_trip() {
        for path in CommandPath::ALL {
            assert_eq!(CommandPath::parse(path.as_str()), Some(path));
        }
        assert_eq!(CommandPath::parse("reachable"), None);
        for path in StatusPath::ALL {
            assert_eq!(StatusPath::parse(path.as_str()), Some(path));
        }
        assert_eq!(StatusPath::parse("identify"), None);
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
