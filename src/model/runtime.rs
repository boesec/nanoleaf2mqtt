//! What the configuration side and the running bridge exchange: configuration changes one way,
//! a status snapshot the other.

use super::{BrokerConfig, DeviceConfig, DeviceId};

/// A change of the configuration the running bridge applies without a restart. Every message
/// carries the complete new state of what it names, so applying it twice changes nothing.
#[derive(Debug, Clone)]
pub enum ConfigChange {
    /// Broker connection and topic prefix; `None` when no broker is configured.
    Broker(Option<BrokerConfig>),
    /// What one device should do from now on.
    Device {
        /// The device.
        id: DeviceId,
        /// Run with this configuration; `None` stops it and clears its retained topics (the
        /// device was removed, disabled, or has no token yet).
        run: Option<DeviceConfig>,
    },
    /// Whether Home Assistant MQTT discovery is wanted.
    HaDiscovery(bool),
}

/// A snapshot of the running bridge, for display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeStatus {
    /// Whether a session with the broker exists.
    pub broker_connected: bool,
    /// Every device the bridge follows, in id order.
    pub devices: Vec<DeviceStatus>,
}

impl BridgeStatus {
    /// The status of one device, if the bridge follows it.
    pub fn device(&self, id: &DeviceId) -> Option<&DeviceStatus> {
        self.devices.iter().find(|d| &d.id == id)
    }
}

/// A snapshot of one device the bridge follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    /// The device.
    pub id: DeviceId,
    /// Whether it is reachable; `None` while it has not reported yet.
    pub reachable: Option<bool>,
    /// The last value published per status path (path, value as JSON text), in path order.
    pub values: Vec<(String, String)>,
}

/// Why pairing with a device failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairError {
    /// The device is not in pairing mode.
    #[error(
        "the device refused pairing: hold its power button for 5-7 seconds until the lights \
         flash, then try again within 30 seconds"
    )]
    NotInPairingMode,
    /// Anything else (unreachable, unexpected answer). The text never contains a token.
    #[error("pairing failed: {0}")]
    Failed(String),
}
