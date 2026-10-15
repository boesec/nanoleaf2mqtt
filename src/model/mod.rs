//! Device-independent light state and commands. No I/O.
//!
//! Every value the device may report is an `Option`: a value the device did not report is
//! `None`, never a default.

mod command;
mod config;
mod device;
mod event;
mod state;

pub use command::{Command, OutOfRange};
pub use config::{
    BrokerConfig, ConfigError, DEFAULT_MQTT_PORT, DeviceConfig, DeviceId, parse_broker_url,
    validate_prefix,
};
pub use device::{DeviceInfo, DeviceSnapshot};
pub use event::{DeviceEvent, Gesture, StateChange};
pub use state::{ColorMode, LightState, Ranges, ValueRange};
