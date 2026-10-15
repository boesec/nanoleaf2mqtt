//! Payload shapes of the contract: `{val, ts, lc}`, `info`, and command parsing.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::topic::CommandPath;
use crate::model::Command;

/// Version of the MQTT contract (`CONTRACT.md`) this crate implements.
pub const CONTRACT_VERSION: u32 = 1;

/// The wire convention, as published in `info.spec`.
pub const SPEC: &str = "mqtt-smarthome 2.0";

/// Name of the service in `info`.
pub const SERVICE_NAME: &str = "nanoleaf2mqtt";

/// Milliseconds since the Unix epoch.
pub fn to_ms(time: DateTime<Utc>) -> i64 {
    time.timestamp_millis()
}

/// The payload of every retained status topic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatePayload {
    /// The value.
    pub val: Value,
    /// When the value was obtained, ms since the epoch.
    pub ts: i64,
    /// When the value last changed, ms since the epoch.
    pub lc: i64,
}

/// The payload of `touch`: no `lc`, a touch is an event, not a state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPayload {
    /// The event.
    pub val: Value,
    /// When it was received, ms since the epoch.
    pub ts: i64,
}

/// The retained `info` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoPayload {
    /// Service name.
    pub name: String,
    /// Service version.
    pub version: String,
    /// The wire convention, [`SPEC`].
    pub spec: String,
    /// [`CONTRACT_VERSION`].
    pub contract_version: u32,
    /// When the service started, RFC 3339.
    pub started_at: String,
}

impl InfoPayload {
    /// The payload of this build, started at `started_at`.
    pub fn new(started_at: DateTime<Utc>) -> Self {
        Self {
            name: SERVICE_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            spec: SPEC.to_owned(),
            contract_version: CONTRACT_VERSION,
            started_at: started_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }
}

/// Remembers the last value and its last change per topic, in memory.
#[derive(Debug, Clone, Default)]
pub struct LastChange {
    entries: BTreeMap<String, (Value, i64)>,
}

impl LastChange {
    /// An empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `val` seen at `ts` on `topic` and returns `lc`: unchanged if the value is the same
    /// as before, `ts` if it changed or is new.
    pub fn observe(&mut self, topic: &str, val: &Value, ts: i64) -> i64 {
        match self.entries.get_mut(topic) {
            Some((last, lc)) => {
                if last != val {
                    *last = val.clone();
                    *lc = ts;
                }
                *lc
            }
            None => {
                self.entries.insert(topic.to_owned(), (val.clone(), ts));
                ts
            }
        }
    }

    /// The value last observed on `topic`.
    pub fn last(&self, topic: &str) -> Option<&Value> {
        self.entries.get(topic).map(|(val, _)| val)
    }

    /// Forgets every topic starting with `prefix`.
    pub fn forget_under(&mut self, prefix: &str) {
        self.entries.retain(|topic, _| !topic.starts_with(prefix));
    }
}

/// Why a command payload was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// The payload is not text.
    #[error("payload is not UTF-8")]
    NotText,
    /// The payload is empty.
    #[error("payload is empty")]
    Empty,
    /// The payload does not have the form the command needs.
    #[error("expected {expected}")]
    Invalid {
        /// What would have been accepted.
        expected: &'static str,
    },
}

/// Parses the payload of a command topic: a plain value or `{"val": …}`. Ranges the device
/// reports are checked later, against the device's own ranges.
pub fn parse_command(path: CommandPath, payload: &[u8]) -> Result<Command, PayloadError> {
    if path == CommandPath::Identify {
        return Ok(Command::Identify);
    }
    let text = std::str::from_utf8(payload)
        .map_err(|_| PayloadError::NotText)?
        .trim();
    if text.is_empty() {
        return Err(PayloadError::Empty);
    }
    // Plain text that is not JSON (`on`, `Northern Lights`) is taken as a string value.
    let json = serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.into()));
    let (val, transition) = match json {
        Value::Object(mut object) => {
            let val = object.remove("val").ok_or(PayloadError::Invalid {
                expected: "a plain value or an object with \"val\"",
            })?;
            (val, object.remove("transition"))
        }
        other => (other, None),
    };
    match path {
        CommandPath::On => parse_bool(&val).map(Command::On),
        CommandPath::Brightness => {
            let value = parse_int(&val, 100)?;
            let transition = match transition {
                None | Some(Value::Null) => None,
                Some(t) => Some(parse_int(&t, u16::MAX).map_err(|_| PayloadError::Invalid {
                    expected: "transition in whole seconds",
                })?),
            };
            Ok(Command::Brightness { value, transition })
        }
        CommandPath::Hue => parse_int(&val, 360).map(Command::Hue),
        CommandPath::Saturation => parse_int(&val, 100).map(Command::Saturation),
        CommandPath::ColorTemperature => parse_int(&val, u16::MAX).map(Command::ColorTemperature),
        CommandPath::Effect => match val {
            Value::String(name) if !name.trim().is_empty() => {
                Ok(Command::Effect(name.trim().to_owned()))
            }
            _ => Err(PayloadError::Invalid {
                expected: "an effect name",
            }),
        },
        CommandPath::Identify => Ok(Command::Identify),
    }
}

fn parse_bool(val: &Value) -> Result<bool, PayloadError> {
    const EXPECTED: PayloadError = PayloadError::Invalid {
        expected: "true/false, 1/0 or on/off",
    };
    match val {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => match n.as_u64() {
            Some(1) => Ok(true),
            Some(0) => Ok(false),
            _ => Err(EXPECTED),
        },
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => Ok(true),
            "off" | "false" | "0" => Ok(false),
            _ => Err(EXPECTED),
        },
        _ => Err(EXPECTED),
    }
}

/// A whole number `0..=max`; `50.0` counts as whole, `50.5` does not.
fn parse_int(val: &Value, max: u16) -> Result<u16, PayloadError> {
    let invalid = PayloadError::Invalid {
        expected: "a whole number in range",
    };
    let number = match val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or(invalid.clone())?;
    if number.fract() != 0.0 || number < 0.0 || number > f64::from(max) {
        return Err(invalid);
    }
    // In range and whole, so the conversion is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as u16)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(path: CommandPath, payload: &str) -> Result<Command, PayloadError> {
        parse_command(path, payload.as_bytes())
    }

    #[test]
    fn state_payload_shape() {
        let payload = StatePayload {
            val: json!(30),
            ts: 2,
            lc: 1,
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"val":30,"ts":2,"lc":1}"#
        );
        let event = EventPayload {
            val: json!({"gesture": "tap"}),
            ts: 3,
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"val":{"gesture":"tap"},"ts":3}"#
        );
    }

    #[test]
    fn lc_changes_only_on_change() {
        let mut lc = LastChange::new();
        assert_eq!(lc.observe("t", &json!(1), 10), 10);
        assert_eq!(lc.observe("t", &json!(1), 20), 10);
        assert_eq!(lc.observe("t", &json!(2), 30), 30);
        assert_eq!(lc.observe("t", &json!(2), 40), 30);
        assert_eq!(lc.observe("u", &json!(2), 50), 50, "topics are separate");
        assert_eq!(lc.last("t"), Some(&json!(2)));
        assert_eq!(lc.last("v"), None);
        lc.forget_under("t");
        assert_eq!(lc.last("t"), None);
        assert_eq!(lc.observe("t", &json!(2), 60), 60, "forgotten");
        assert_eq!(lc.observe("u", &json!(2), 70), 50);
    }

    #[test]
    fn info_payload() {
        let at = DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let value = serde_json::to_value(InfoPayload::new(at)).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "nanoleaf2mqtt",
                "version": env!("CARGO_PKG_VERSION"),
                "spec": "mqtt-smarthome 2.0",
                "contract_version": 1,
                "started_at": "2026-10-15T12:00:00.000Z",
            })
        );
    }

    #[test]
    fn on_forms() {
        for (payload, on) in [
            ("true", true),
            ("false", false),
            ("1", true),
            ("0", false),
            ("on", true),
            ("OFF", false),
            (" On \n", true),
            ("\"on\"", true),
            ("\"false\"", false),
            (r#"{"val":true}"#, true),
            (r#"{"val":0}"#, false),
            (r#"{"val":"off"}"#, false),
        ] {
            assert_eq!(
                parse(CommandPath::On, payload),
                Ok(Command::On(on)),
                "{payload}"
            );
        }
        for bad in ["2", "yes", "null", "[]", r#"{"value":true}"#, "", "  "] {
            assert!(parse(CommandPath::On, bad).is_err(), "{bad}");
        }
        assert_eq!(
            parse_command(CommandPath::On, &[0xff, 0xfe]),
            Err(PayloadError::NotText)
        );
    }

    #[test]
    fn numeric_forms() {
        assert_eq!(
            parse(CommandPath::Brightness, "50"),
            Ok(Command::Brightness {
                value: 50,
                transition: None
            })
        );
        assert_eq!(
            parse(CommandPath::Brightness, r#"{"val":50,"transition":2}"#),
            Ok(Command::Brightness {
                value: 50,
                transition: Some(2)
            })
        );
        assert_eq!(
            parse(CommandPath::Brightness, r#"{"val":"50.0"}"#),
            Ok(Command::Brightness {
                value: 50,
                transition: None
            })
        );
        assert_eq!(parse(CommandPath::Hue, "360"), Ok(Command::Hue(360)));
        assert_eq!(
            parse(CommandPath::Saturation, "0"),
            Ok(Command::Saturation(0))
        );
        assert_eq!(
            parse(CommandPath::ColorTemperature, r#"{"val":2700}"#),
            Ok(Command::ColorTemperature(2700))
        );
        for (path, bad) in [
            (CommandPath::Brightness, "101"),
            (CommandPath::Brightness, "-1"),
            (CommandPath::Brightness, "50.5"),
            (CommandPath::Brightness, "bright"),
            (CommandPath::Brightness, r#"{"val":50,"transition":-1}"#),
            (CommandPath::Brightness, r#"{"val":50,"transition":"slow"}"#),
            (CommandPath::Hue, "361"),
            (CommandPath::Saturation, "101"),
            (CommandPath::ColorTemperature, "70000"),
            (CommandPath::ColorTemperature, "true"),
        ] {
            assert!(parse(path, bad).is_err(), "{path:?} {bad}");
        }
    }

    #[test]
    fn effect_forms() {
        let effect = |name: &str| Ok(Command::Effect(name.to_owned()));
        assert_eq!(
            parse(CommandPath::Effect, "Northern Lights"),
            effect("Northern Lights")
        );
        assert_eq!(parse(CommandPath::Effect, "\"Prism\""), effect("Prism"));
        assert_eq!(
            parse(CommandPath::Effect, r#"{"val":"Prism"}"#),
            effect("Prism")
        );
        for bad in ["", "42", r#"{"val":""}"#, r#"{"val":null}"#] {
            assert!(parse(CommandPath::Effect, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn identify_takes_anything() {
        for payload in ["", "1", "whatever", "{}"] {
            assert_eq!(parse(CommandPath::Identify, payload), Ok(Command::Identify));
        }
        assert_eq!(
            parse_command(CommandPath::Identify, &[0xff]),
            Ok(Command::Identify)
        );
    }
}
