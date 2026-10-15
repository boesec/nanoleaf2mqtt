//! Interim configuration of one device and the broker from environment variables, until the
//! configuration store takes over.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `NANOLEAF_HOST` | yes | address of the device |
//! | `NANOLEAF_PORT` | no, `16021` | port of the device's OpenAPI |
//! | `NANOLEAF_TOKEN` | yes | auth token (see the `pair` subcommand) |
//! | `NANOLEAF_DEVICE_ID` | yes | device id below the prefix, `a-z0-9-` |
//! | `MQTT_URL` | yes | `mqtt://host[:port]` |
//! | `MQTT_USER` | no | broker user; anonymous when unset |
//! | `MQTT_PASSWORD` | no | password of `MQTT_USER` |
//! | `NANOLEAF2MQTT_PREFIX` | no, `nanoleaf` | topic prefix |

use crate::model::{
    BrokerConfig, ConfigError, DeviceConfig, DeviceId, parse_broker_url, validate_prefix,
};
use crate::nanoleaf::DEFAULT_PORT;
use crate::secret::Secret;

/// Address of the device.
pub const ENV_HOST: &str = "NANOLEAF_HOST";
/// Port of the device.
pub const ENV_PORT: &str = "NANOLEAF_PORT";
/// Auth token of the device.
pub const ENV_TOKEN: &str = "NANOLEAF_TOKEN";
/// Device id below the prefix.
pub const ENV_DEVICE_ID: &str = "NANOLEAF_DEVICE_ID";
/// Broker URL.
pub const ENV_MQTT_URL: &str = "MQTT_URL";
/// Broker user.
pub const ENV_MQTT_USER: &str = "MQTT_USER";
/// Broker password.
pub const ENV_MQTT_PASSWORD: &str = "MQTT_PASSWORD";
/// Topic prefix.
pub const ENV_PREFIX: &str = "NANOLEAF2MQTT_PREFIX";

/// Topic prefix when none is configured.
pub const DEFAULT_PREFIX: &str = "nanoleaf";

/// Error in the interim configuration. Never contains the token or the password.
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    /// A required variable is not set (or empty).
    #[error("{0} is not set")]
    Missing(&'static str),
    /// The port is not a number 1-65535.
    #[error("{ENV_PORT} is not a port number 1-65535")]
    InvalidPort,
    /// A value is invalid.
    #[error("{name}: {source}")]
    Invalid {
        /// The variable.
        name: &'static str,
        /// What is wrong.
        source: ConfigError,
    },
}

/// Reads broker and device through `lookup`, which returns the value of a variable if set.
pub fn from_lookup<F>(lookup: F) -> Result<(BrokerConfig, Vec<DeviceConfig>), EnvError>
where
    F: Fn(&str) -> Option<String>,
{
    let get = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
    let required = |name: &'static str| get(name).ok_or(EnvError::Missing(name));

    let id =
        DeviceId::new(required(ENV_DEVICE_ID)?.trim()).map_err(|source| EnvError::Invalid {
            name: ENV_DEVICE_ID,
            source,
        })?;
    let host = required(ENV_HOST)?.trim().to_owned();
    let port = match get(ENV_PORT) {
        None => DEFAULT_PORT,
        Some(port) => match port.trim().parse::<u16>() {
            Ok(0) | Err(_) => return Err(EnvError::InvalidPort),
            Ok(port) => port,
        },
    };
    let token = Secret::new(required(ENV_TOKEN)?.trim().to_owned());

    let (broker_host, broker_port) =
        parse_broker_url(&required(ENV_MQTT_URL)?).map_err(|source| EnvError::Invalid {
            name: ENV_MQTT_URL,
            source,
        })?;
    let prefix = get(ENV_PREFIX).map_or_else(|| DEFAULT_PREFIX.to_owned(), |p| p.trim().to_owned());
    validate_prefix(&prefix).map_err(|source| EnvError::Invalid {
        name: ENV_PREFIX,
        source,
    })?;
    let username = get(ENV_MQTT_USER).map(|u| u.trim().to_owned());
    let password = username
        .as_ref()
        .and_then(|_| lookup(ENV_MQTT_PASSWORD))
        .map(Secret::new);

    let broker = BrokerConfig {
        host: broker_host,
        port: broker_port,
        username,
        password,
        // Unique per process: a second instance (development next to production) must not
        // take over the session of the first.
        client_id: format!("nanoleaf2mqtt-{}", std::process::id()),
        prefix,
    };
    let device = DeviceConfig {
        id,
        host,
        port,
        token,
    };
    Ok((broker, vec![device]))
}

/// Reads broker and device from the process environment.
pub fn from_env() -> Result<(BrokerConfig, Vec<DeviceConfig>), EnvError> {
    from_lookup(|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const TOKEN: &str = "env-test-token";
    const PASSWORD: &str = "env-test-password";

    fn load(pairs: &[(&str, &str)]) -> Result<(BrokerConfig, Vec<DeviceConfig>), EnvError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        from_lookup(|name| map.get(name).cloned())
    }

    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            (ENV_DEVICE_ID, "kellertreppe"),
            (ENV_HOST, "192.0.2.10"),
            (ENV_TOKEN, TOKEN),
            (ENV_MQTT_URL, "mqtt://broker.test"),
        ]
    }

    #[test]
    fn minimal_configuration() {
        let (broker, devices) = load(&minimal()).unwrap();
        assert_eq!(broker.host, "broker.test");
        assert_eq!(broker.port, 1883);
        assert_eq!(broker.prefix, "nanoleaf");
        assert_eq!(broker.username, None);
        assert!(broker.password.is_none());
        assert!(broker.client_id.starts_with("nanoleaf2mqtt-"));
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.id.as_str(), "kellertreppe");
        assert_eq!(device.host, "192.0.2.10");
        assert_eq!(device.port, 16021);
        assert_eq!(device.token.expose(), TOKEN);
    }

    #[test]
    fn full_configuration() {
        let mut vars = minimal();
        vars.extend([
            (ENV_PORT, "16022"),
            (ENV_MQTT_URL, "mqtt://broker.test:1884"),
            (ENV_MQTT_USER, "bridge"),
            (ENV_MQTT_PASSWORD, PASSWORD),
            (ENV_PREFIX, "home/nanoleaf"),
        ]);
        let (broker, devices) = load(&vars).unwrap();
        assert_eq!(broker.port, 1884);
        assert_eq!(broker.username.as_deref(), Some("bridge"));
        assert_eq!(broker.password.as_ref().unwrap().expose(), PASSWORD);
        assert_eq!(broker.prefix, "home/nanoleaf");
        assert_eq!(devices[0].port, 16022);
        let debug = format!("{broker:?} {devices:?}");
        assert!(
            !debug.contains(TOKEN) && !debug.contains(PASSWORD),
            "{debug}"
        );
    }

    #[test]
    fn device_id_is_required_and_never_invented() {
        for name in [ENV_DEVICE_ID, ENV_HOST, ENV_TOKEN, ENV_MQTT_URL] {
            let vars: Vec<_> = minimal().into_iter().filter(|(k, _)| *k != name).collect();
            let err = load(&vars).unwrap_err();
            assert!(
                matches!(err, EnvError::Missing(n) if n == name),
                "{name}: {err}"
            );
            let mut empty = vars.clone();
            empty.push((name, " "));
            assert!(matches!(load(&empty).unwrap_err(), EnvError::Missing(_)));
        }
    }

    #[test]
    fn invalid_values() {
        for (name, value) in [
            (ENV_DEVICE_ID, "Keller Treppe"),
            (ENV_PORT, "0"),
            (ENV_PORT, "port"),
            (ENV_MQTT_URL, "mqtts://broker"),
            (ENV_PREFIX, "nano/#"),
        ] {
            let mut vars = minimal();
            vars.push((name, value));
            let message = load(&vars).unwrap_err().to_string();
            assert!(message.contains(name), "{name}: {message}");
        }
    }

    #[test]
    fn errors_never_contain_secrets() {
        let mut vars = minimal();
        vars.push((ENV_MQTT_URL, "mqtt://user:secret-in-url@broker"));
        let message = load(&vars).unwrap_err().to_string();
        assert!(!message.contains("secret-in-url") && !message.contains(TOKEN));
    }
}
