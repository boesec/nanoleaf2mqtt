//! Events a device reports.

use super::{ColorMode, DeviceSnapshot};

/// A touch gesture on the panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gesture {
    /// Single tap.
    SingleTap,
    /// Double tap.
    DoubleTap,
    /// Swipe up.
    SwipeUp,
    /// Swipe down.
    SwipeDown,
    /// Swipe left.
    SwipeLeft,
    /// Swipe right.
    SwipeRight,
    /// Long press: a panel is held (id 6, undocumented by Nanoleaf).
    LongPress,
    /// A gesture id this version does not know, as the device sent it.
    Unknown(u8),
}

/// One changed attribute of the light state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateChange {
    /// Switched on or off.
    On(bool),
    /// Brightness in percent.
    Brightness(u16),
    /// Hue in degrees.
    Hue(u16),
    /// Saturation in percent.
    Saturation(u16),
    /// Colour temperature in kelvin.
    ColorTemperature(u16),
    /// Colour mode.
    ColorMode(ColorMode),
}

/// Something the device reported, or a change of its reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// The device is reachable again (or for the first time). A [`DeviceEvent::Snapshot`]
    /// follows.
    Reachable,
    /// The device cannot be reached; values published before may be stale.
    Unreachable,
    /// Full state, read after every (re)connect so that nothing missed while away is lost.
    Snapshot(Box<DeviceSnapshot>),
    /// Attributes of the light state changed. The device groups changes that belong together
    /// (for example switching off reports `On(false)` and `Brightness(0)` at once).
    StateChanged(Vec<StateChange>),
    /// Another effect was selected.
    EffectChanged(String),
    /// The panel layout changed.
    LayoutChanged,
    /// A touch gesture.
    Touch {
        /// Panel that was touched; `None` for gestures not tied to one panel (swipes).
        panel_id: Option<u32>,
        /// The gesture.
        gesture: Gesture,
    },
}
