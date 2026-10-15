//! Configuration of the devices and the broker, independent of where it comes from.

use std::fmt;

use crate::secret::Secret;

/// Longest accepted device id.
const MAX_ID_LEN: usize = 64;

/// Port of an MQTT broker when the URL names none.
pub const DEFAULT_MQTT_PORT: u16 = 1883;

/// Port of the Nanoleaf OpenAPI, suggested for a new device.
pub const DEFAULT_DEVICE_PORT: u16 = 16021;

/// Topic prefix until another one is configured.
pub const DEFAULT_PREFIX: &str = "nanoleaf";

/// The MQTT client id of this process. Unique per process, so a development instance next to
/// the running service never takes over the service's session.
pub fn process_client_id() -> String {
    format!("nanoleaf2mqtt-{}", std::process::id())
}

/// Invalid configuration value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The device id is empty, too long or contains characters other than `a-z`, `0-9`, `-`.
    #[error("device id `{0}` is invalid: use 1-64 characters out of a-z, 0-9 and -")]
    InvalidDeviceId(String),
    /// The broker URL cannot be used.
    #[error("broker URL is invalid: {0}")]
    InvalidBrokerUrl(&'static str),
    /// The topic prefix cannot be used.
    #[error("topic prefix `{prefix}` is invalid: {reason}")]
    InvalidPrefix {
        /// The offending prefix.
        prefix: String,
        /// What is wrong with it.
        reason: &'static str,
    },
}

/// Id of a device below the topic prefix: 1-64 characters out of `a-z`, `0-9` and `-`. Chosen by
/// the user and stable across renames and device swaps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    /// Validates `id`.
    pub fn new(id: impl Into<String>) -> Result<Self, ConfigError> {
        let id = id.into();
        let valid = !id.is_empty()
            && id.len() <= MAX_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if valid {
            Ok(Self(id))
        } else {
            Err(ConfigError::InvalidDeviceId(id))
        }
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Suggests an id for a device called `name`: lower case, German umlauts spelled out, every
    /// other run of characters outside `a-z0-9` turned into one `-`. `None` when nothing usable
    /// is left (for example a name of punctuation only).
    pub fn suggest(name: &str) -> Option<Self> {
        let mut slug = String::with_capacity(name.len());
        let mut dash = false;
        for c in name.chars().flat_map(char::to_lowercase) {
            let replacement = match c {
                'ä' => Some("ae"),
                'ö' => Some("oe"),
                'ü' => Some("ue"),
                'ß' => Some("ss"),
                _ => None,
            };
            if let Some(text) = replacement {
                slug.push_str(text);
                dash = false;
            } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
                slug.push(c);
                dash = false;
            } else if !dash && !slug.is_empty() {
                slug.push('-');
                dash = true;
            }
        }
        slug.truncate(MAX_ID_LEN);
        let slug = slug.trim_end_matches('-');
        Self::new(slug).ok()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One configured device.
#[derive(Debug)]
pub struct DeviceConfig {
    /// Id below the topic prefix.
    pub id: DeviceId,
    /// Address of the device.
    pub host: String,
    /// Port of the device's OpenAPI.
    pub port: u16,
    /// Auth token.
    pub token: Secret<String>,
}

impl Clone for DeviceConfig {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            host: self.host.clone(),
            port: self.port,
            token: Secret::new(self.token.expose().clone()),
        }
    }
}

/// Connection to the MQTT broker and the topic prefix.
#[derive(Debug)]
pub struct BrokerConfig {
    /// Host name or address of the broker.
    pub host: String,
    /// Port of the broker.
    pub port: u16,
    /// User name; `None` connects anonymously.
    pub username: Option<String>,
    /// Password of `username`.
    pub password: Option<Secret<String>>,
    /// MQTT client id.
    pub client_id: String,
    /// Topic prefix, the first level of every topic.
    pub prefix: String,
}

impl Clone for BrokerConfig {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self
                .password
                .as_ref()
                .map(|p| Secret::new(p.expose().clone())),
            client_id: self.client_id.clone(),
            prefix: self.prefix.clone(),
        }
    }
}

/// Splits a broker URL `mqtt://host[:port]` (or `tcp://`) into host and port. TLS
/// (`mqtts://`, `ssl://`) is not supported yet and rejected, as is anything after the
/// authority and any user information (credentials are configured separately).
pub fn parse_broker_url(url: &str) -> Result<(String, u16), ConfigError> {
    let url = url.trim();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or(ConfigError::InvalidBrokerUrl("expected mqtt://host[:port]"))?;
    match scheme.to_ascii_lowercase().as_str() {
        "mqtt" | "tcp" => {}
        "mqtts" | "ssl" | "tls" => {
            return Err(ConfigError::InvalidBrokerUrl("TLS is not supported yet"));
        }
        _ => return Err(ConfigError::InvalidBrokerUrl("scheme must be mqtt://")),
    }
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains(['/', '?', '#']) {
        return Err(ConfigError::InvalidBrokerUrl(
            "no path or query allowed after host[:port]",
        ));
    }
    if authority.contains('@') {
        return Err(ConfigError::InvalidBrokerUrl(
            "credentials do not belong into the URL",
        ));
    }
    let (host, port) = split_host_port(authority)?;
    if host.is_empty() {
        return Err(ConfigError::InvalidBrokerUrl("host is missing"));
    }
    Ok((host.to_owned(), port))
}

fn split_host_port(authority: &str) -> Result<(&str, u16), ConfigError> {
    // `[v6]:port` or `[v6]`
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or(ConfigError::InvalidBrokerUrl("unclosed '[' in host"))?;
        return match after.strip_prefix(':') {
            Some(port) => Ok((host, parse_port(port)?)),
            None if after.is_empty() => Ok((host, DEFAULT_MQTT_PORT)),
            None => Err(ConfigError::InvalidBrokerUrl("unexpected text after host")),
        };
    }
    match authority.split_once(':') {
        Some((host, port)) => Ok((host, parse_port(port)?)),
        None => Ok((authority, DEFAULT_MQTT_PORT)),
    }
}

fn parse_port(port: &str) -> Result<u16, ConfigError> {
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(ConfigError::InvalidBrokerUrl(
            "port is not a number 1-65535",
        )),
        Ok(port) => Ok(port),
    }
}

/// Validates a topic prefix: non-empty, no leading or trailing `/`, no empty level, no
/// wildcards, no whitespace or control characters.
pub fn validate_prefix(prefix: &str) -> Result<(), ConfigError> {
    let fail = |reason| {
        Err(ConfigError::InvalidPrefix {
            prefix: prefix.to_owned(),
            reason,
        })
    };
    if prefix.is_empty() {
        return fail("empty");
    }
    if prefix.split('/').any(str::is_empty) {
        return fail("must not start or end with '/' or contain an empty level");
    }
    if prefix.contains(['+', '#']) {
        return fail("must not contain the wildcards '+' or '#'");
    }
    if prefix.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return fail("must not contain whitespace or control characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids() {
        for ok in ["a", "kellertreppe", "panel-2", "0", &"x".repeat(64)] {
            assert_eq!(DeviceId::new(ok).unwrap().as_str(), ok);
        }
        for bad in ["", "Panel", "a b", "a_b", "a/b", "a+", "ä", &"x".repeat(65)] {
            assert_eq!(
                DeviceId::new(bad),
                Err(ConfigError::InvalidDeviceId(bad.to_owned())),
                "{bad}"
            );
        }
    }

    #[test]
    fn suggested_ids() {
        let suggest = |name: &str| DeviceId::suggest(name).map(|id| id.as_str().to_owned());
        assert_eq!(suggest("Kellertreppe").as_deref(), Some("kellertreppe"));
        assert_eq!(
            suggest("  Küche / Wand 2 ").as_deref(),
            Some("kueche-wand-2")
        );
        assert_eq!(suggest("Große Straße").as_deref(), Some("grosse-strasse"));
        assert_eq!(
            suggest("Shapes_Living-Room").as_deref(),
            Some("shapes-living-room")
        );
        assert_eq!(suggest("--"), None);
        assert_eq!(suggest(""), None);
        let long = suggest(&"ab ".repeat(40)).unwrap();
        assert!(long.len() <= 64 && !long.ends_with('-'), "{long}");
    }

    #[test]
    fn client_id_names_the_process() {
        assert_eq!(
            process_client_id(),
            format!("nanoleaf2mqtt-{}", std::process::id())
        );
    }

    #[test]
    fn broker_urls() {
        let ok = |url: &str| parse_broker_url(url).unwrap();
        assert_eq!(ok("mqtt://broker"), ("broker".to_owned(), 1883));
        assert_eq!(ok("mqtt://broker:1884"), ("broker".to_owned(), 1884));
        assert_eq!(ok("tcp://10.0.0.1:1883/"), ("10.0.0.1".to_owned(), 1883));
        assert_eq!(ok(" MQTT://broker "), ("broker".to_owned(), 1883));
        assert_eq!(ok("mqtt://[::1]:1885"), ("::1".to_owned(), 1885));
        assert_eq!(ok("mqtt://[::1]"), ("::1".to_owned(), 1883));
        for bad in [
            "broker:1883",
            "http://broker",
            "mqtts://broker",
            "mqtt://",
            "mqtt://:1883",
            "mqtt://broker:0",
            "mqtt://broker:x",
            "mqtt://broker/path",
            "mqtt://user:pw@broker",
            "mqtt://[::1",
        ] {
            assert!(parse_broker_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn credentials_in_url_are_not_echoed() {
        let err = parse_broker_url("mqtt://user:hunter2@broker").unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
    }

    #[test]
    fn prefixes() {
        for ok in ["nanoleaf", "home/nanoleaf", "a-b_c"] {
            assert_eq!(validate_prefix(ok), Ok(()), "{ok}");
        }
        for bad in ["", "/a", "a/", "a//b", "a+", "a/#", "a b", "a\nb"] {
            assert!(validate_prefix(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn secrets_stay_redacted_and_survive_clone() {
        let device = DeviceConfig {
            id: DeviceId::new("dev").unwrap(),
            host: "h".to_owned(),
            port: 1,
            token: Secret::new("tok-secret".to_owned()),
        };
        let clone = device.clone();
        assert_eq!(clone.token.expose(), "tok-secret");
        assert!(!format!("{device:?}").contains("tok-secret"));
        let broker = BrokerConfig {
            host: "b".to_owned(),
            port: 1,
            username: Some("u".to_owned()),
            password: Some(Secret::new("pw-secret".to_owned())),
            client_id: "c".to_owned(),
            prefix: "p".to_owned(),
        };
        assert!(!format!("{:?}", broker.clone()).contains("pw-secret"));
        assert_eq!(broker.clone().password.unwrap().expose(), "pw-secret");
    }
}
