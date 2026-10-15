//! Bootstrap settings, read from the environment.
//!
//! Only what is needed before the configuration store can be opened lives here:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `NANOLEAF2MQTT_DATA_DIR` | `./data` | directory of the SQLite store |
//! | `NANOLEAF2MQTT_HTTP_BIND` | `127.0.0.1:8080` | address of the configuration UI |
//! | `NANOLEAF2MQTT_LOG` | `info` | `tracing` filter |

use std::net::SocketAddr;
use std::path::PathBuf;

/// Environment variable naming the data directory.
pub const ENV_DATA_DIR: &str = "NANOLEAF2MQTT_DATA_DIR";
/// Environment variable naming the HTTP bind address.
pub const ENV_HTTP_BIND: &str = "NANOLEAF2MQTT_HTTP_BIND";
/// Environment variable holding the log filter.
pub const ENV_LOG: &str = "NANOLEAF2MQTT_LOG";

const DEFAULT_DATA_DIR: &str = "./data";
const DEFAULT_HTTP_BIND: &str = "127.0.0.1:8080";
const DEFAULT_LOG: &str = "info";

/// Error while reading the bootstrap settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The HTTP bind address is not a valid socket address.
    #[error("{ENV_HTTP_BIND} is not a valid socket address (`{value}`): {source}")]
    InvalidBind {
        /// The offending value.
        value: String,
        /// Parse error.
        source: std::net::AddrParseError,
    },
}

/// Bootstrap settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Directory of the SQLite store.
    pub data_dir: PathBuf,
    /// Address the configuration UI binds to.
    pub http_bind: SocketAddr,
    /// `tracing` filter directive.
    pub log: String,
}

impl Config {
    /// Reads the settings from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the settings through `lookup`, which returns the value of a variable if set.
    pub fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let data_dir = lookup(ENV_DATA_DIR).unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned());
        let bind = lookup(ENV_HTTP_BIND).unwrap_or_else(|| DEFAULT_HTTP_BIND.to_owned());
        let log = lookup(ENV_LOG).unwrap_or_else(|| DEFAULT_LOG.to_owned());
        let http_bind = bind.parse().map_err(|source| ConfigError::InvalidBind {
            value: bind,
            source,
        })?;
        Ok(Self {
            data_dir: PathBuf::from(data_dir),
            http_bind,
            log,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn load(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_when_unset() {
        let config = load(&[]).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("./data"));
        assert_eq!(
            config.http_bind,
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.log, "info");
    }

    #[test]
    fn values_from_env() {
        let config = load(&[
            (ENV_DATA_DIR, "/var/lib/nanoleaf2mqtt"),
            (ENV_HTTP_BIND, "0.0.0.0:9000"),
            (ENV_LOG, "debug"),
        ])
        .unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/nanoleaf2mqtt"));
        assert_eq!(
            config.http_bind,
            "0.0.0.0:9000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.log, "debug");
    }

    #[test]
    fn invalid_bind_is_error() {
        let err = load(&[(ENV_HTTP_BIND, "not-an-address")]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("NANOLEAF2MQTT_HTTP_BIND"), "{message}");
        assert!(message.contains("not-an-address"), "{message}");
    }
}
