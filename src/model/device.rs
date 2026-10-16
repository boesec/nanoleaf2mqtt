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
    /// Panel geometry, `None` when the device reported no position data.
    pub layout: Option<Layout>,
}

/// The geometry of the panels as the device reports it. Every value is `None` when the device
/// did not report it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    /// Global orientation in degrees.
    pub orientation: Option<u16>,
    /// Side length of one panel, in the layout's units.
    pub side_length: Option<u32>,
    /// The panels, controller units included.
    pub panels: Vec<PanelPosition>,
}

/// One panel of the layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelPosition {
    /// Panel id as the device reports it (also used in touch events).
    pub id: u32,
    /// Horizontal position.
    pub x: Option<i32>,
    /// Vertical position.
    pub y: Option<i32>,
    /// Rotation in degrees.
    pub rotation: Option<u16>,
    /// The device's shape type number.
    pub shape_type: Option<u32>,
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
