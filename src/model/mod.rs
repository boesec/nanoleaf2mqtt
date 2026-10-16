//! Device-independent light state and commands. No I/O.
//!
//! Every value the device may report is an `Option`: a value the device did not report is
//! `None`, never a default.

mod command;
mod config;
mod device;
mod event;
mod runtime;
mod state;

pub use command::{Command, OutOfRange};
pub use config::{
    BrokerConfig, ConfigError, DEFAULT_DEVICE_PORT, DEFAULT_HA_DISCOVERY_PREFIX, DEFAULT_MQTT_PORT,
    DEFAULT_PREFIX, DeviceConfig, DeviceId, parse_broker_url, process_client_id, validate_prefix,
};
pub use device::{DeviceInfo, DeviceSnapshot, Layout, PanelPosition};
pub use event::{DeviceEvent, Gesture, StateChange};
pub use runtime::{BridgeStatus, ConfigChange, DeviceStatus, PairError};
pub use state::{ColorMode, LightState, Ranges, ValueRange};
