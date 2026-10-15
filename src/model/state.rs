//! Light state as reported by a device.

use std::fmt;

/// Inclusive range of values the device accepts for one attribute, as the device reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueRange {
    /// Smallest accepted value.
    pub min: u16,
    /// Largest accepted value.
    pub max: u16,
}

impl ValueRange {
    /// Whether `value` lies inside the range (both ends included).
    pub fn contains(&self, value: u16) -> bool {
        (self.min..=self.max).contains(&value)
    }
}

impl fmt::Display for ValueRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.min, self.max)
    }
}

/// Value ranges the device reports for its numeric attributes. `None` where the device did not
/// report a complete range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Ranges {
    /// Brightness range in percent.
    pub brightness: Option<ValueRange>,
    /// Hue range in degrees.
    pub hue: Option<ValueRange>,
    /// Saturation range in percent.
    pub saturation: Option<ValueRange>,
    /// Colour temperature range in kelvin.
    pub color_temperature: Option<ValueRange>,
}

/// What currently determines the colour of the light.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColorMode {
    /// An effect (scene) is running.
    Effect,
    /// Hue and saturation.
    HueSaturation,
    /// Colour temperature.
    ColorTemperature,
    /// A mode this version does not know, with the device's name for it.
    Unknown(String),
}

/// Light state. Every attribute is `None` when the device did not report it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LightState {
    /// Whether the light is on.
    pub on: Option<bool>,
    /// Brightness in percent (see [`Ranges::brightness`]).
    pub brightness: Option<u16>,
    /// Hue in degrees.
    pub hue: Option<u16>,
    /// Saturation in percent.
    pub saturation: Option<u16>,
    /// Colour temperature in kelvin.
    pub color_temperature: Option<u16>,
    /// What determines the colour.
    pub color_mode: Option<ColorMode>,
    /// Name of the selected effect, as the device reports it (may be a pseudo effect such as
    /// `*Solid*` or `*Shuffle*`).
    pub effect: Option<String>,
    /// Value ranges the device reports.
    pub ranges: Ranges,
}
