//! Payload shapes of the contract: `{val, ts, lc}`, `info`, and command parsing.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::topic::CommandPath;
use crate::model::Command;

/// Version of the MQTT contract (`CONTRACT.md`) this crate implements.
pub const CONTRACT_VERSION: u32 = 2;

/// The version of mqtt-smarthome implemented, as published in `info.spec`.
pub const SPEC: &str = "2.0";

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

/// The payload of `touch`: an event, so every one is a change and `lc` equals `ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPayload {
    /// The event.
    pub val: Value,
    /// When it was received, ms since the epoch.
    pub ts: i64,
    /// When it last changed; the same as `ts`.
    pub lc: i64,
}

/// The retained `info` payload (mqtt-smarthome 2.0, section 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoPayload {
    /// Service name.
    pub name: String,
    /// Service version.
    pub version: String,
    /// The mqtt-smarthome version implemented, [`SPEC`].
    pub spec: String,
    /// Name of the host the service runs on; left out when it cannot be found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Process id.
    pub pid: u32,
    /// When the service started, RFC 3339.
    pub started: String,
    /// Whether the maintenance topics are enabled: this service has none.
    pub maintenance: bool,
    /// [`CONTRACT_VERSION`].
    pub contract_version: u32,
}

impl InfoPayload {
    /// The payload of this process, started at `started`.
    pub fn new(started: DateTime<Utc>) -> Self {
        Self::for_process(started, host_name(), std::process::id())
    }

    /// The payload of a process on `host` with `pid`, started at `started`.
    pub fn for_process(started: DateTime<Utc>, host: Option<String>, pid: u32) -> Self {
        Self {
            name: SERVICE_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            spec: SPEC.to_owned(),
            host,
            pid,
            started: started.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            maintenance: false,
            contract_version: CONTRACT_VERSION,
        }
    }
}

/// The host name: the kernel's on Linux, otherwise the environment's; `None` if neither says.
pub fn host_name() -> Option<String> {
    let from_file = ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok());
    from_file
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
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

/// A number from a command payload, and whether it had to be clamped.
type Number = (u16, bool);

/// A command payload, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// The device commands, in the order they are sent.
    pub commands: Vec<Command>,
    /// Whether a number lay outside the command's fixed range and was clamped into it.
    pub clamped: bool,
}

/// Parses the payload of a command topic: a plain value or `{"val": …}`.
///
/// Numbers are rounded to whole numbers and clamped into the command's fixed range (brightness
/// and saturation 0–100, hue 0–360, colour temperature and transition 0–65535); the ranges the
/// device reports are applied later. A payload that is no number where one is needed is an
/// error.
///
/// Every command is one device command, except `hs`: hue, then saturation, both parsed before
/// either is returned.
pub fn parse_command(path: CommandPath, payload: &[u8]) -> Result<ParsedCommand, PayloadError> {
    if path == CommandPath::HueSaturation {
        let val = parse_value(payload)?.0;
        let ((hue, hue_clamped), (saturation, saturation_clamped)) = parse_hs(&val)?;
        return Ok(ParsedCommand {
            commands: vec![Command::Hue(hue), Command::Saturation(saturation)],
            clamped: hue_clamped || saturation_clamped,
        });
    }
    let (command, clamped) = parse_single(path, payload)?;
    Ok(ParsedCommand {
        commands: vec![command],
        clamped,
    })
}

fn parse_single(path: CommandPath, payload: &[u8]) -> Result<(Command, bool), PayloadError> {
    if path == CommandPath::Identify {
        return Ok((Command::Identify, false));
    }
    let (val, transition) = parse_value(payload)?;
    match path {
        CommandPath::On => parse_bool(&val).map(|on| (Command::On(on), false)),
        CommandPath::Brightness => {
            let (value, value_clamped) = parse_number(&val, 100)?;
            let (transition, transition_clamped) = match transition {
                None | Some(Value::Null) => (None, false),
                Some(t) => {
                    let (seconds, clamped) =
                        parse_number(&t, u16::MAX).map_err(|_| PayloadError::Invalid {
                            expected: "transition in seconds",
                        })?;
                    (Some(seconds), clamped)
                }
            };
            Ok((
                Command::Brightness { value, transition },
                value_clamped || transition_clamped,
            ))
        }
        CommandPath::Hue => parse_number(&val, 360).map(|(v, c)| (Command::Hue(v), c)),
        CommandPath::HueSaturation => Err(PayloadError::Invalid {
            expected: "a single command",
        }),
        CommandPath::Saturation => {
            parse_number(&val, 100).map(|(v, c)| (Command::Saturation(v), c))
        }
        CommandPath::ColorTemperature => {
            parse_number(&val, u16::MAX).map(|(v, c)| (Command::ColorTemperature(v), c))
        }
        CommandPath::Effect => match val {
            Value::String(name) if !name.trim().is_empty() => {
                Ok((Command::Effect(name.trim().to_owned()), false))
            }
            _ => Err(PayloadError::Invalid {
                expected: "an effect name",
            }),
        },
        CommandPath::Identify => Ok((Command::Identify, false)),
    }
}

/// The value of a command payload and, from the object form, its `transition`.
fn parse_value(payload: &[u8]) -> Result<(Value, Option<Value>), PayloadError> {
    let text = std::str::from_utf8(payload)
        .map_err(|_| PayloadError::NotText)?
        .trim();
    if text.is_empty() {
        return Err(PayloadError::Empty);
    }
    // Plain text that is not JSON (`on`, `Northern Lights`, `120,80`) is taken as a string value.
    let json = serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.into()));
    match json {
        Value::Object(mut object) => {
            let val = object.remove("val").ok_or(PayloadError::Invalid {
                expected: "a plain value or an object with \"val\"",
            })?;
            Ok((val, object.remove("transition")))
        }
        other => Ok((other, None)),
    }
}

/// Hue and saturation from `"h,s"` or `[h, s]`, each with whether it was clamped.
fn parse_hs(val: &Value) -> Result<(Number, Number), PayloadError> {
    const EXPECTED: PayloadError = PayloadError::Invalid {
        expected: "hue and saturation as \"h,s\" or [h, s]",
    };
    let (hue, saturation) = match val {
        Value::String(text) => {
            let (hue, saturation) = text.split_once(',').ok_or(EXPECTED)?;
            (
                Value::String(hue.to_owned()),
                Value::String(saturation.to_owned()),
            )
        }
        Value::Array(items) => match items.as_slice() {
            [hue, saturation] => (hue.clone(), saturation.clone()),
            _ => return Err(EXPECTED),
        },
        _ => return Err(EXPECTED),
    };
    let hue = parse_number(&hue, 360).map_err(|_| EXPECTED)?;
    let saturation = parse_number(&saturation, 100).map_err(|_| EXPECTED)?;
    Ok((hue, saturation))
}

fn parse_bool(val: &Value) -> Result<bool, PayloadError> {
    const EXPECTED: PayloadError = PayloadError::Invalid {
        expected: "true/false, 1/0, on/off or yes/no",
    };
    match val {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => match n.as_u64() {
            Some(1) => Ok(true),
            Some(0) => Ok(false),
            _ => Err(EXPECTED),
        },
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "yes" | "1" => Ok(true),
            "off" | "false" | "no" | "0" => Ok(false),
            _ => Err(EXPECTED),
        },
        _ => Err(EXPECTED),
    }
}

/// A number (JSON or decimal text), rounded to a whole number and clamped into `0..=max`; with
/// whether it had to be clamped.
fn parse_number(val: &Value, max: u16) -> Result<Number, PayloadError> {
    let number = match val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
    .ok_or(PayloadError::Invalid {
        expected: "a number",
    })?;
    let rounded = number.round();
    let clamped = rounded.clamp(0.0, f64::from(max));
    // Whole and within `0..=max`, so the conversion is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((clamped as u16, clamped != rounded))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(path: CommandPath, payload: &str) -> Result<Command, PayloadError> {
        let (command, clamped) = parse_clamped(path, payload)?;
        assert!(!clamped, "{path:?} {payload} was clamped");
        Ok(command)
    }

    fn parse_clamped(path: CommandPath, payload: &str) -> Result<(Command, bool), PayloadError> {
        let mut parsed = parse_command(path, payload.as_bytes())?;
        assert_eq!(parsed.commands.len(), 1, "{path:?} {payload}");
        Ok((parsed.commands.remove(0), parsed.clamped))
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
            lc: 3,
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"val":{"gesture":"tap"},"ts":3,"lc":3}"#
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
        let value =
            serde_json::to_value(InfoPayload::for_process(at, Some("box".to_owned()), 42)).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "nanoleaf2mqtt",
                "version": env!("CARGO_PKG_VERSION"),
                "spec": "2.0",
                "host": "box",
                "pid": 42,
                "started": "2026-10-15T12:00:00.000Z",
                "maintenance": false,
                "contract_version": 2,
            })
        );
        // A host that cannot be found is left out, not invented.
        let value = serde_json::to_value(InfoPayload::for_process(at, None, 42)).unwrap();
        assert!(value.get("host").is_none());
        assert_eq!(InfoPayload::new(at).pid, std::process::id());
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
            ("yes", true),
            ("YES", true),
            (" no ", false),
            (r#"{"val":"yes"}"#, true),
            (r#"{"val":" No "}"#, false),
        ] {
            assert_eq!(
                parse(CommandPath::On, payload),
                Ok(Command::On(on)),
                "{payload}"
            );
        }
        for bad in ["2", "maybe", "null", "[]", r#"{"value":true}"#, "", "  "] {
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
        // Rounded, not rejected.
        assert_eq!(
            parse(CommandPath::Brightness, "50.5"),
            Ok(Command::Brightness {
                value: 51,
                transition: None
            })
        );
        assert_eq!(parse(CommandPath::Hue, "360"), Ok(Command::Hue(360)));
        assert_eq!(parse(CommandPath::Hue, "359.4"), Ok(Command::Hue(359)));
        assert_eq!(
            parse(CommandPath::Saturation, "0"),
            Ok(Command::Saturation(0))
        );
        assert_eq!(
            parse(CommandPath::ColorTemperature, r#"{"val":2700}"#),
            Ok(Command::ColorTemperature(2700))
        );
        for (path, bad) in [
            (CommandPath::Brightness, "bright"),
            (CommandPath::Brightness, r#"{"val":50,"transition":"slow"}"#),
            (CommandPath::Brightness, "inf"),
            (CommandPath::Brightness, "NaN"),
            (CommandPath::Hue, "[1]"),
            (CommandPath::Saturation, "null"),
            (CommandPath::ColorTemperature, "true"),
        ] {
            assert!(
                parse_command(path, bad.as_bytes()).is_err(),
                "{path:?} {bad}"
            );
        }
    }

    #[test]
    fn numbers_outside_the_fixed_range_are_clamped() {
        let brightness = |value, transition| Command::Brightness { value, transition };
        for (path, payload, expected) in [
            (CommandPath::Brightness, "101", brightness(100, None)),
            (CommandPath::Brightness, "-5", brightness(0, None)),
            (
                CommandPath::Brightness,
                r#"{"val":50,"transition":-1}"#,
                brightness(50, Some(0)),
            ),
            (
                CommandPath::Brightness,
                r#"{"val":50,"transition":70000}"#,
                brightness(50, Some(u16::MAX)),
            ),
            (CommandPath::Hue, "400", Command::Hue(360)),
            (CommandPath::Hue, "-1", Command::Hue(0)),
            (CommandPath::Saturation, "101", Command::Saturation(100)),
            (
                CommandPath::ColorTemperature,
                "99999",
                Command::ColorTemperature(u16::MAX),
            ),
            (
                CommandPath::ColorTemperature,
                "-5",
                Command::ColorTemperature(0),
            ),
        ] {
            assert_eq!(
                parse_clamped(path, payload),
                Ok((expected, true)),
                "{path:?} {payload}"
            );
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
            parse_command(CommandPath::Identify, &[0xff]).map(|p| p.commands),
            Ok(vec![Command::Identify])
        );
    }

    #[test]
    fn hs_is_hue_then_saturation() {
        let both = |h, s, clamped| {
            Ok(ParsedCommand {
                commands: vec![Command::Hue(h), Command::Saturation(s)],
                clamped,
            })
        };
        for payload in [
            "120,80",
            " 120 , 80 ",
            "\"120,80\"",
            "[120,80]",
            "[120.0,80]",
            "[119.6,80]",
            r#"{"val":[120,80]}"#,
            r#"{"val":"120,80"}"#,
        ] {
            assert_eq!(
                parse_command(CommandPath::HueSaturation, payload.as_bytes()),
                both(120, 80, false),
                "{payload}"
            );
        }
        assert_eq!(
            parse_command(CommandPath::HueSaturation, b"360,0"),
            both(360, 0, false)
        );
        // Each component is clamped on its own.
        assert_eq!(
            parse_command(CommandPath::HueSaturation, b"400,150"),
            both(360, 100, true)
        );
        assert_eq!(
            parse_command(CommandPath::HueSaturation, b"-1,80"),
            both(0, 80, true)
        );
        for bad in [
            "",
            "120",
            "120,",
            ",80",
            "120,80,1",
            "[120]",
            "[120,80,1]",
            "red,80",
            r#"{"val":120}"#,
            r#"{"h":120,"s":80}"#,
            "red",
        ] {
            assert!(
                parse_command(CommandPath::HueSaturation, bad.as_bytes()).is_err(),
                "{bad}"
            );
        }
    }
}
