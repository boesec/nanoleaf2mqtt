//! MQTT publisher and subscriber following the mqtt-smarthome conventions; the topics and
//! payloads are specified in `CONTRACT.md`.

mod client;
pub mod map;
pub mod payload;
mod topic;

pub use client::{Mqtt, MqttError, SetMessage, Timing};
pub use payload::{
    CONTRACT_VERSION, EventPayload, InfoPayload, LastChange, PayloadError, SERVICE_NAME, SPEC,
    StatePayload, parse_command,
};
pub use topic::{CommandPath, SetTopic, StatusPath, Topics};
