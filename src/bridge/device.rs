//! The tasks of one device: following its events, executing its commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use crate::model::{Command, DeviceEvent, DeviceId, DeviceSnapshot, Ranges, StateChange};
use crate::mqtt::{Mqtt, SetMessage, StatusPath, map, parse_command};
use crate::nanoleaf::Client;

/// Reachability of every device; `connected` is `2` only while all are reachable.
#[derive(Debug)]
pub(crate) struct Reachability {
    devices: Mutex<HashMap<DeviceId, bool>>,
}

impl Reachability {
    /// No device yet.
    pub(crate) fn new() -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
        }
    }

    /// Adds a device as not (yet) reachable and updates `connected`.
    pub(crate) fn add(&self, mqtt: &Mqtt, id: &DeviceId) {
        let mut devices = self.lock();
        devices.insert(id.clone(), false);
        Self::publish(mqtt, &devices);
    }

    /// Forgets a device and updates `connected`.
    pub(crate) fn remove(&self, mqtt: &Mqtt, id: &DeviceId) {
        let mut devices = self.lock();
        devices.remove(id);
        Self::publish(mqtt, &devices);
    }

    /// Records the reachability of a device added before and updates `connected`. The lock is
    /// held while publishing, so two devices changing at once cannot publish an outdated
    /// aggregate last.
    pub(crate) fn set(&self, mqtt: &Mqtt, id: &DeviceId, reachable: bool) {
        let mut devices = self.lock();
        if let Some(entry) = devices.get_mut(id) {
            *entry = reachable;
            Self::publish(mqtt, &devices);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<DeviceId, bool>> {
        self.devices.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(mqtt: &Mqtt, devices: &HashMap<DeviceId, bool>) {
        let all = !devices.is_empty() && devices.values().all(|r| *r);
        mqtt.set_all_reachable(all);
    }
}

/// What the device last reported that its commands are checked against.
#[derive(Debug)]
pub(crate) struct Known {
    /// The effect list.
    pub(crate) effects: watch::Sender<Option<Vec<String>>>,
    /// The value ranges.
    pub(crate) ranges: watch::Sender<Option<Ranges>>,
}

/// Publishes what the device reports, until its event stream ends: the stream's events, and the
/// state read back after commands (`rereads`), of which only the changed values are published.
pub(crate) async fn follow(
    id: DeviceId,
    mqtt: Mqtt,
    client: Client,
    mut events: mpsc::Receiver<DeviceEvent>,
    mut rereads: mpsc::Receiver<DeviceSnapshot>,
    reachability: Arc<Reachability>,
    known: Known,
) {
    // `Reachable` is followed by a snapshot; `reachable = true` goes out after its values, so
    // a consumer never sees "reachable" next to old values.
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
                let (path, val) = map::reachable_value(false);
                mqtt.publish_status(&id, path, val);
                reachability.set(&mqtt, &id, false);
            }
            DeviceEvent::Snapshot(snapshot) => {
                // After a (re)connect everything is published, changed or not.
                publish_snapshot(&mqtt, &id, &snapshot, &known, true);
                if std::mem::take(&mut came_back) {
                    let (path, val) = map::reachable_value(true);
                    mqtt.publish_status(&id, path, val);
                    reachability.set(&mqtt, &id, true);
                }
            }
            DeviceEvent::StateChanged(changes) => {
                for change in &changes {
                    let (path, val) = map::change_value(change);
                    mqtt.publish_status(&id, path, val);
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
                mqtt.publish_status(&id, path, val);
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

/// Publishes every value of the snapshot (`full`), or only those that changed, and hands what
/// it says about the device to the discovery messages.
fn publish_snapshot(
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
    mqtt.describe_device(id, snapshot);
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
/// A message that becomes several device requests (`hs`: hue, then saturation) is checked as a
/// whole first: if any value is outside the device's range, nothing is sent.
pub(crate) async fn execute(
    id: DeviceId,
    client: Client,
    mut commands: mpsc::Receiver<SetMessage>,
    effects: watch::Receiver<Option<Vec<String>>>,
    ranges: watch::Receiver<Option<Ranges>>,
    rereads: mpsc::Sender<DeviceSnapshot>,
) {
    while let Some(message) = commands.recv().await {
        let path = message.path.as_str();
        let commands = match parse_command(message.path, &message.payload) {
            Ok(commands) => commands,
            Err(error) => {
                tracing::warn!(device = id.as_str(), command = path, %error, "invalid command ignored");
                continue;
            }
        };
        if let [Command::Effect(name)] = commands.as_slice() {
            if !effect_known(&client, &effects, name).await {
                tracing::warn!(
                    device = id.as_str(),
                    effect = name.as_str(),
                    "unknown effect ignored"
                );
                continue;
            }
        }
        if commands.len() > 1 && !in_range(&id, &client, &ranges, &commands, path).await {
            continue;
        }
        let mut sent = 0;
        for command in &commands {
            if let Err(error) = client.send(command).await {
                tracing::warn!(
                    device = id.as_str(),
                    command = path,
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
        tracing::debug!(device = id.as_str(), command = path, "command sent");
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
                device = id.as_str(),
                command = path,
                error = error.chain(),
                "reading the state after a command failed"
            ),
        }
    }
}

/// Whether every command's value lies within the ranges the device reported (read first if
/// none are known yet). Logs the first that does not.
async fn in_range(
    id: &DeviceId,
    client: &Client,
    ranges: &watch::Receiver<Option<Ranges>>,
    commands: &[Command],
    path: &str,
) -> bool {
    let known = *ranges.borrow();
    let ranges = match known {
        Some(ranges) => ranges,
        None => match client.snapshot().await {
            Ok(snapshot) => snapshot.state.ranges,
            Err(error) => {
                tracing::warn!(
                    device = id.as_str(),
                    command = path,
                    error = error.chain(),
                    "cannot read the ranges, command ignored"
                );
                return false;
            }
        },
    };
    for command in commands {
        if let Err(error) = command.check(&ranges) {
            tracing::warn!(device = id.as_str(), command = path, %error, "command out of range ignored");
            return false;
        }
    }
    true
}

/// Whether the device offers the effect. The list may be older than an effect added in the
/// Nanoleaf app since, so a miss is checked against a fresh list; an unknown list lets the
/// device decide.
async fn effect_known(
    client: &Client,
    effects: &watch::Receiver<Option<Vec<String>>>,
    name: &str,
) -> bool {
    let known = effects
        .borrow()
        .as_ref()
        .map(|list| list.iter().any(|e| e == name));
    match known {
        None | Some(true) => true,
        Some(false) => match client.effects_list().await {
            Ok(list) => list.iter().any(|e| e == name),
            // The device will answer the command itself.
            Err(_) => true,
        },
    }
}
