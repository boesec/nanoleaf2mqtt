//! Commands to a device and their range check.

use super::{Ranges, ValueRange};

/// A command to a light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Switch on (`true`) or off (`false`).
    On(bool),
    /// Set the brightness in percent, optionally with a transition time in seconds.
    Brightness {
        /// Target brightness.
        value: u16,
        /// Transition time in seconds; `None` lets the device decide.
        transition: Option<u16>,
    },
    /// Set the hue in degrees.
    Hue(u16),
    /// Set the saturation in percent.
    Saturation(u16),
    /// Set the colour temperature in kelvin.
    ColorTemperature(u16),
    /// Select an effect by name.
    Effect(String),
    /// Let the panels flash to identify them.
    Identify,
}

/// A command value outside the range the device reported. Commands from MQTT are clamped into
/// the range first ([`Command::clamp`]); this check is the last line before a request goes out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{attribute} {value} is outside the range {range} the device accepts")]
pub struct OutOfRange {
    /// Name of the attribute.
    pub attribute: &'static str,
    /// Requested value.
    pub value: u16,
    /// Range the device reported.
    pub range: ValueRange,
}

impl Command {
    /// Checks the command's value against the ranges the device reported. An attribute whose
    /// range the device did not report is not checked here; the device decides.
    pub fn check(&self, ranges: &Ranges) -> Result<(), OutOfRange> {
        let (attribute, value, range) = match self {
            Self::Brightness { value, .. } => ("brightness", *value, ranges.brightness),
            Self::Hue(value) => ("hue", *value, ranges.hue),
            Self::Saturation(value) => ("saturation", *value, ranges.saturation),
            Self::ColorTemperature(value) => {
                ("color temperature", *value, ranges.color_temperature)
            }
            Self::On(_) | Self::Effect(_) | Self::Identify => return Ok(()),
        };
        match range {
            Some(range) if !range.contains(value) => Err(OutOfRange {
                attribute,
                value,
                range,
            }),
            _ => Ok(()),
        }
    }

    /// Moves the command's value into the range the device reported for it, the nearest end if
    /// it lies outside. Returns whether the value changed. An attribute whose range the device
    /// did not report is left as it is.
    pub fn clamp(&mut self, ranges: &Ranges) -> bool {
        let (value, range) = match self {
            Self::Brightness { value, .. } => (value, ranges.brightness),
            Self::Hue(value) => (value, ranges.hue),
            Self::Saturation(value) => (value, ranges.saturation),
            Self::ColorTemperature(value) => (value, ranges.color_temperature),
            Self::On(_) | Self::Effect(_) | Self::Identify => return false,
        };
        let Some(range) = range else {
            return false;
        };
        // A range the device reported upside down is not trusted to clamp into.
        if range.min > range.max {
            return false;
        }
        let clamped = (*value).clamp(range.min, range.max);
        let changed = clamped != *value;
        *value = clamped;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges() -> Ranges {
        Ranges {
            brightness: Some(ValueRange { min: 0, max: 100 }),
            hue: Some(ValueRange { min: 0, max: 360 }),
            saturation: Some(ValueRange { min: 0, max: 100 }),
            color_temperature: Some(ValueRange {
                min: 1200,
                max: 6500,
            }),
        }
    }

    #[test]
    fn values_at_the_bounds_pass() {
        let r = ranges();
        for command in [
            Command::Brightness {
                value: 0,
                transition: None,
            },
            Command::Brightness {
                value: 100,
                transition: Some(5),
            },
            Command::Hue(360),
            Command::Saturation(0),
            Command::ColorTemperature(1200),
            Command::ColorTemperature(6500),
            Command::On(true),
            Command::Effect("Anything".to_owned()),
            Command::Identify,
        ] {
            assert_eq!(command.check(&r), Ok(()), "{command:?}");
        }
    }

    #[test]
    fn values_outside_are_rejected() {
        let r = ranges();
        let err = Command::Brightness {
            value: 101,
            transition: None,
        }
        .check(&r)
        .unwrap_err();
        assert_eq!(err.attribute, "brightness");
        assert_eq!(err.value, 101);
        assert_eq!(
            err.to_string(),
            "brightness 101 is outside the range 0..=100 the device accepts"
        );
        assert!(Command::Hue(361).check(&r).is_err());
        assert!(Command::Saturation(101).check(&r).is_err());
        assert!(Command::ColorTemperature(1199).check(&r).is_err());
        assert!(Command::ColorTemperature(6501).check(&r).is_err());
    }

    #[test]
    fn unknown_range_is_not_checked() {
        assert_eq!(Command::Hue(9999).check(&Ranges::default()), Ok(()));
    }

    #[test]
    fn clamp_moves_values_into_the_reported_range() {
        let r = ranges();
        let clamp = |mut command: Command| {
            let changed = command.clamp(&r);
            (command, changed)
        };
        assert_eq!(
            clamp(Command::ColorTemperature(65535)),
            (Command::ColorTemperature(6500), true)
        );
        assert_eq!(
            clamp(Command::ColorTemperature(0)),
            (Command::ColorTemperature(1200), true)
        );
        assert_eq!(
            clamp(Command::ColorTemperature(2700)),
            (Command::ColorTemperature(2700), false)
        );
        assert_eq!(clamp(Command::Hue(361)), (Command::Hue(360), true));
        assert_eq!(
            clamp(Command::Brightness {
                value: 101,
                transition: Some(3)
            }),
            (
                Command::Brightness {
                    value: 100,
                    transition: Some(3)
                },
                true
            )
        );
        assert_eq!(clamp(Command::On(true)), (Command::On(true), false));
        let mut unknown = Command::Hue(9999);
        assert!(!unknown.clamp(&Ranges::default()));
        assert_eq!(unknown, Command::Hue(9999));
        let mut inverted = Command::Hue(5);
        let odd = Ranges {
            hue: Some(ValueRange { min: 10, max: 1 }),
            ..Ranges::default()
        };
        assert!(!inverted.clamp(&odd));
        // Clamped commands pass the check.
        for mut command in [Command::ColorTemperature(9), Command::Saturation(500)] {
            command.clamp(&r);
            assert_eq!(command.check(&r), Ok(()));
        }
    }
}
