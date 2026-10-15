//! The tasks of one device: following its events, executing its commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use crate::model::{Command, DeviceEvent, DeviceId, DeviceSnapshot, StateChange};
use crate::mqtt::{Mqtt, SetMessage, map, parse_command};
use crate::nanoleaf::Client;

/// Reachability of every device; `connected` is `2` only while all are reachable.
#[derive(Debug)]
pub(crate) struct Reachability {
    devices: Mutex<HashMap<DeviceId, bool>>,
}

impl Reachability {
    /// Every device starts as not (yet) reachable.
    pub(crate) fn new<'a>(ids: impl IntoIterator<Item = &'a DeviceId>) -> Self {
        Self {
            devices: Mutex::new(ids.into_iter().map(|id| (id.clone(), false)).collect()),
        }
    }

    /// Records the device's reachability and updates `connected`. The lock is held while
    /// publishing, so two devices changing at once cannot publish an outdated aggregate last.
    pub(crate) fn set(&self, mqtt: &Mqtt, id: &DeviceId, reachable: bool) {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        devices.insert(id.clone(), reachable);
        let all = !devices.is_empty() && devices.values().all(|r| *r);
        mqtt.set_all_reachable(all);
    }
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
    effects: watch::Sender<Option<Vec<String>>>,
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
                    Some(snapshot) => publish_snapshot(&mqtt, &id, &snapshot, &effects, false),
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
                publish_snapshot(&mqtt, &id, &snapshot, &effects, true);
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
                // The device does not reliably report the effect that goes with a new colour
                // mode (`*Solid*` after switching to hs or ct): read it.
                if changes
                    .iter()
                    .any(|c| matches!(c, StateChange::ColorMode(_)))
                {
                    reread(&mqtt, &id, &client, &effects, "colour mode change").await;
                }
            }
            DeviceEvent::EffectChanged(name) => {
                let (path, val) = map::effect_value(&name);
                mqtt.publish_status(&id, path, val);
            }
            DeviceEvent::LayoutChanged => {
                reread(&mqtt, &id, &client, &effects, "layout change").await;
            }
            DeviceEvent::Touch { panel_id, gesture } => {
                mqtt.publish_touch(&id, panel_id, gesture);
            }
        }
    }
}

async fn reread(
    mqtt: &Mqtt,
    id: &DeviceId,
    client: &Client,
    effects: &watch::Sender<Option<Vec<String>>>,
    reason: &'static str,
) {
    match client.snapshot().await {
        Ok(snapshot) => publish_snapshot(mqtt, id, &snapshot, effects, false),
        Err(error) => tracing::warn!(
            device = id.as_str(),
            reason,
            error = error.chain(),
            "re-reading the device failed"
        ),
    }
}

/// Publishes every value of the snapshot (`full`), or only those that changed.
fn publish_snapshot(
    mqtt: &Mqtt,
    id: &DeviceId,
    snapshot: &DeviceSnapshot,
    effects: &watch::Sender<Option<Vec<String>>>,
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
        effects.send_replace(snapshot.effects.clone());
    }
}

/// Executes the device's commands one after the other. What was sent is never published: after
/// a command the state is read back from the device and handed to [`follow`], which publishes
/// what changed. The event stream would report the change as well, but only on the device's own tick
/// of about 1.5 s, and not every effect change.
pub(crate) async fn execute(
    id: DeviceId,
    client: Client,
    mut commands: mpsc::Receiver<SetMessage>,
    effects: watch::Receiver<Option<Vec<String>>>,
    rereads: mpsc::Sender<DeviceSnapshot>,
) {
    while let Some(message) = commands.recv().await {
        let path = message.path.as_str();
        let command = match parse_command(message.path, &message.payload) {
            Ok(command) => command,
            Err(error) => {
                tracing::warn!(device = id.as_str(), command = path, %error, "invalid command ignored");
                continue;
            }
        };
        if let Command::Effect(name) = &command {
            if !effect_known(&client, &effects, name).await {
                tracing::warn!(
                    device = id.as_str(),
                    effect = name.as_str(),
                    "unknown effect ignored"
                );
                continue;
            }
        }
        if let Err(error) = client.send(&command).await {
            tracing::warn!(
                device = id.as_str(),
                command = path,
                error = error.chain(),
                "command failed"
            );
            continue;
        }
        tracing::debug!(device = id.as_str(), command = path, "command sent");
        if command == Command::Identify {
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
