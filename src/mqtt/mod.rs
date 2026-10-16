//! MQTT publisher and subscriber following the mqtt-smarthome conventions; the topics and
//! payloads are specified in `CONTRACT.md`.

mod client;
pub mod discovery;
pub mod map;
pub mod payload;
mod topic;

pub use client::{LastChangeUpdate, Mqtt, MqttError, SetMessage, Timing};
pub use payload::{
    CONTRACT_VERSION, EventPayload, InfoPayload, LastChange, ParsedCommand, PayloadError,
    SERVICE_NAME, SPEC, StatePayload, parse_command,
};
pub use topic::{
    CommandPath, HaDiscoveryTopic, HaEntity, SetTopic, StatusPath, StatusTopic, Topics,
    ha_discovery, ha_legacy_discovery, ha_node, ha_readback_filters, parse_ha_discovery,
};
