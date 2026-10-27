//! The tasks of one device: following its events, executing its commands.

use tokio::sync::{mpsc, watch};

use crate::model::{Command, DeviceEvent, DeviceId, DeviceSnapshot, Ranges, StateChange};
use crate::mqtt::{Mqtt, SetMessage, StatusPath, map, parse_command};
use crate::nanoleaf::Client;

/// What the device last reported that its commands are checked against.
#[derive(Debug)]
pub(crate) struct Known {
    /// The effect list.
    pub(crate) effects: watch::Sender<Option<Vec<String>>>,
    /// The value ranges.
    pub(crate) ranges: watch::Sender<Option<Ranges>>,
}

/// Publishes what the device reports, until its event stream ends: the stream's events, and the
/// state read back after commands (`rereads`). Only values that changed are published, except
/// after the device (re)connects: then its snapshot is published completely.
pub(crate) async fn follow(
    id: DeviceId,
    mqtt: Mqtt,
    client: Client,
    mut events: mpsc::Receiver<DeviceEvent>,
    mut rereads: mpsc::Receiver<DeviceSnapshot>,
    known: Known,
) {
    // `Reachable` is followed by a snapshot; `online = true` goes out after its values, so a
    // consumer never sees "online" next to old values.
    let mut came_back = false;
    let mut rereads_open = true;
    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Some(event) => event,
                None => return,
            },
            snapshot = rereads.recv(), if rereads_open => {
                match snapshot {
                    Some(snapshot) => publish_snapshot(&mqtt, &id, &snapshot, &known, false),
                    None => rereads_open = false,
                }
                continue;
            }
        };
        match event {
            DeviceEvent::Reachable => came_back = true,
            DeviceEvent::Unreachable => {
                came_back = false;
                let (path, val) = map::online_value(false);
                mqtt.publish_status(&id, path, val);
            }
            DeviceEvent::Snapshot(snapshot) => {
                // After a (re)connect everything is published, changed or not; the discovery
                // message follows `online`, which it names for availability.
                publish_values(&mqtt, &id, &snapshot, &known, true);
                if std::mem::take(&mut came_back) {
                    let (path, val) = map::online_value(true);
                    mqtt.publish_status(&id, path, val);
                }
                mqtt.describe_device(&id, &snapshot);
            }
            DeviceEvent::StateChanged(changes) => {
                // The device reports a change made through MQTT again on its event stream; the
                // read-back after the command has published it already.
                for change in &changes {
                    let (path, val) = map::change_value(change);
                    mqtt.publish_status_if_changed(&id, path, val);
                }
                if changes
                    .iter()
                    .any(|c| matches!(c, StateChange::Hue(_) | StateChange::Saturation(_)))
                {
                    publish_hs(&mqtt, &id);
                }
                // The device does not reliably report the effect that goes with a new colour
                // mode (`*Solid*` after switching to hs or ct): read it.
                if changes
                    .iter()
                    .any(|c| matches!(c, StateChange::ColorMode(_)))
                {
                    reread(&mqtt, &id, &client, &known, "colour mode change").await;
                }
            }
            DeviceEvent::EffectChanged(name) => {
                let (path, val) = map::effect_value(&name);
                mqtt.publish_status_if_changed(&id, path, val);
            }
            DeviceEvent::LayoutChanged => {
                reread(&mqtt, &id, &client, &known, "layout change").await;
            }
            DeviceEvent::Touch { panel_id, gesture } => {
                mqtt.publish_touch(&id, panel_id, gesture);
            }
        }
    }
}

/// Publishes `hs` from the last published hue and saturation, if both are known and the pair
/// changed.
fn publish_hs(mqtt: &Mqtt, id: &DeviceId) {
    let hue = mqtt.value(id, StatusPath::Hue);
    let saturation = mqtt.value(id, StatusPath::Saturation);
    if let Some((path, val)) = map::hs_from(hue.as_ref(), saturation.as_ref()) {
        mqtt.publish_status_if_changed(id, path, val);
    }
}

async fn reread(mqtt: &Mqtt, id: &DeviceId, client: &Client, known: &Known, reason: &'static str) {
    match client.snapshot().await {
        Ok(snapshot) => publish_snapshot(mqtt, id, &snapshot, known, false),
        Err(error) => tracing::warn!(
            device = id.as_str(),
            reason,
            error = error.chain(),
            "re-reading the device failed"
        ),
    }
}

/// Publishes the values of a snapshot (see [`publish_values`]) and hands what it says about the
/// device to the discovery message.
fn publish_snapshot(
    mqtt: &Mqtt,
    id: &DeviceId,
    snapshot: &DeviceSnapshot,
    known: &Known,
    full: bool,
) {
    publish_values(mqtt, id, snapshot, known, full);
    mqtt.describe_device(id, snapshot);
}

/// Publishes every value of the snapshot (`full`), or only those that changed, and takes over
/// its effect list and ranges for the commands.
fn publish_values(
    mqtt: &Mqtt,
    id: &DeviceId,
    snapshot: &DeviceSnapshot,
    known: &Known,
    full: bool,
) {
    for (path, val) in map::snapshot_values(snapshot) {
        if full {
            mqtt.publish_status(id, path, val);
        } else {
            mqtt.publish_status_if_changed(id, path, val);
        }
    }
    if snapshot.effects.is_some() {
        known.effects.send_replace(snapshot.effects.clone());
    }
    known.ranges.send_replace(Some(snapshot.state.ranges));
}

/// Executes the device's commands one after the other. What was sent is never published: after
/// a command the state is read back from the device and handed to [`follow`], which publishes
/// what changed. The event stream would report the change as well, but only on the device's own tick
/// of about 1.5 s, and not every effect change.
///
/// Numbers outside the range the device reported are clamped into it and logged at `warn`
/// (mqtt-smarthome 2.0, section 3.3); a payload that cannot be read and an unknown effect are
/// logged and ignored.
pub(crate) async fn execute(
    id: DeviceId,
    client: Client,
    mut commands: mpsc::Receiver<SetMessage>,
    effects: watch::Receiver<Option<Vec<String>>>,
    ranges: watch::Receiver<Option<Ranges>>,
    rereads: mpsc::Sender<DeviceSnapshot>,
) {
    while let Some(message) = commands.recv().await {
        let topic = message.topic.as_str();
        let payload = String::from_utf8_lossy(&message.payload);
        let parsed = match parse_command(message.path, &message.payload) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(topic, payload = %payload, %error, "invalid command ignored");
                continue;
            }
        };
        let mut commands = parsed.commands;
        if let [Command::Effect(name)] = commands.as_mut_slice() {
            match resolve_effect(&client, &effects, name).await {
                Some(device_name) => *name = device_name,
                None => {
                    tracing::warn!(topic, payload = %payload, "unknown effect ignored");
                    continue;
                }
            }
        }
        let mut clamped = parsed.clamped;
        if needs_range(&commands) {
            let Some(device_ranges) = current_ranges(&client, &ranges, topic, &payload).await
            else {
                continue;
            };
            for command in &mut commands {
                clamped |= command.clamp(&device_ranges);
            }
        }
        if clamped {
            tracing::warn!(topic, payload = %payload, sent = ?commands, "value out of range, clamped");
        }
        let mut sent = 0;
        for command in &commands {
            if let Err(error) = client.send(command).await {
                tracing::warn!(
                    topic,
                    payload = %payload,
                    error = error.chain(),
                    "command failed"
                );
                break;
            }
            sent += 1;
        }
        if sent == 0 {
            continue;
        }
        tracing::debug!(device = id.as_str(), topic, "command sent");
        if commands == [Command::Identify] {
            continue;
        }
        match client.snapshot().await {
            Ok(snapshot) => {
                if rereads.send(snapshot).await.is_err() {
                    return;
                }
            }
            Err(error) => tracing::warn!(
                topic,
                error = error.chain(),
                "reading the state after a command failed"
            ),
        }
    }
}

/// Whether any of the commands carries a number the device has a range for.
fn needs_range(commands: &[Command]) -> bool {
    commands.iter().any(|c| {
        matches!(
            c,
            Command::Brightness { .. }
                | Command::Hue(_)
                | Command::Saturation(_)
                | Command::ColorTemperature(_)
        )
    })
}

/// The ranges the device reported, read first if none are known yet. `None` (logged) if they
/// cannot be read: then nothing is sent.
async fn current_ranges(
    client: &Client,
    ranges: &watch::Receiver<Option<Ranges>>,
    topic: &str,
    payload: &str,
) -> Option<Ranges> {
    let known = *ranges.borrow();
    match known {
        Some(ranges) => Some(ranges),
        None => match client.snapshot().await {
            Ok(snapshot) => Some(snapshot.state.ranges),
            Err(error) => {
                tracing::warn!(
                    topic,
                    payload,
                    error = error.chain(),
                    "cannot read the ranges, command ignored"
                );
                None
            }
        },
    }
}

/// The device's own spelling of the effect, or `None` if it does not offer it. Names match
/// ignoring ASCII case (mqtt-smarthome 2.0, section 5.3), an exact match first. The list may be
/// older than an effect added in the Nanoleaf app since, so a miss is checked against a fresh
/// list; an unknown list lets the device decide, with the name as given.
async fn resolve_effect(
    client: &Client,
    effects: &watch::Receiver<Option<Vec<String>>>,
    name: &str,
) -> Option<String> {
    let known = effects
        .borrow()
        .as_ref()
        .map(|list| match_effect(list, name));
    match known {
        None => Some(name.to_owned()),
        Some(Some(found)) => Some(found),
        Some(None) => match client.effects_list().await {
            Ok(list) => match_effect(&list, name),
            // The device will answer the command itself.
            Err(_) => Some(name.to_owned()),
        },
    }
}

/// Finds `name` in `list`: the exact spelling if present, otherwise the first entry that
/// differs only in ASCII case (several such entries are logged at `debug`).
fn match_effect(list: &[String], name: &str) -> Option<String> {
    if list.iter().any(|e| e == name) {
        return Some(name.to_owned());
    }
    let mut matches = list.iter().filter(|e| e.eq_ignore_ascii_case(name));
    let first = matches.next()?;
    if matches.next().is_some() {
        tracing::debug!(
            effect = name,
            chosen = first.as_str(),
            "several effects differ only in case, the first one is used"
        );
    }
    Some(first.clone())
}

#[cfg(test)]
mod tests {
    use super::match_effect;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn effect_names_match_ignoring_case_and_return_the_device_spelling() {
        let effects = list(&["Blaze", "Northern Lights"]);
        assert_eq!(
            match_effect(&effects, "northern lights").as_deref(),
            Some("Northern Lights")
        );
        assert_eq!(match_effect(&effects, "BLAZE").as_deref(), Some("Blaze"));
        assert_eq!(match_effect(&effects, "Nothing"), None);
    }

    #[test]
    fn an_exact_match_wins_over_a_case_variant() {
        let effects = list(&["blaze", "Blaze", "BLAZE"]);
        assert_eq!(match_effect(&effects, "Blaze").as_deref(), Some("Blaze"));
        assert_eq!(match_effect(&effects, "bLaze").as_deref(), Some("blaze"));
    }
}
