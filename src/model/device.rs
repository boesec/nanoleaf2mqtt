//! Device identity and full snapshots.

use super::LightState;

/// What a device reports about itself. Every field is `None` when the device did not report it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// User-assigned device name.
    pub name: Option<String>,
    /// Serial number.
    pub serial: Option<String>,
    /// Manufacturer.
    pub manufacturer: Option<String>,
    /// Model identifier (for example `NL42`).
    pub model: Option<String>,
    /// Firmware version.
    pub firmware: Option<String>,
    /// Hardware version.
    pub hardware: Option<String>,
    /// Number of panels in the layout, controller units included as the device counts them.
    pub panel_count: Option<u32>,
}

/// Everything read from a device in one go: identity, light state and the effects it offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceSnapshot {
    /// Identity.
    pub info: DeviceInfo,
    /// Light state.
    pub state: LightState,
    /// Names of the effects the device offers, `None` when not reported.
    pub effects: Option<Vec<String>>,
}
