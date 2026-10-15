//! The tasks of one device: following its events, executing its commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use crate::model::{Command, DeviceEvent, DeviceId, DeviceSnapshot};
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

/// Publishes what the device reports, until its event stream ends.
pub(crate) async fn follow(
    id: DeviceId,
    mqtt: Mqtt,
    client: Client,
    mut events: mpsc::Receiver<DeviceEvent>,
    reachability: Arc<Reachability>,
    effects: watch::Sender<Option<Vec<String>>>,
) {
    // `Reachable` is followed by a snapshot; `reachable = true` goes out after its values, so
    // a consumer never sees "reachable" next to old values.
    let mut came_back = false;
    while let Some(event) = events.recv().await {
        match event {
            DeviceEvent::Reachable => came_back = true,
            DeviceEvent::Unreachable => {
                came_back = false;
                let (path, val) = map::reachable_value(false);
                mqtt.publish_status(&id, path, val);
                reachability.set(&mqtt, &id, false);
            }
            DeviceEvent::Snapshot(snapshot) => {
                publish_snapshot(&mqtt, &id, &snapshot, &effects);
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
            }
            DeviceEvent::EffectChanged(name) => {
                let (path, val) = map::effect_value(&name);
                mqtt.publish_status(&id, path, val);
            }
            DeviceEvent::LayoutChanged => match client.snapshot().await {
                Ok(snapshot) => publish_snapshot(&mqtt, &id, &snapshot, &effects),
                Err(error) => tracing::warn!(
                    device = id.as_str(),
                    error = error.chain(),
                    "re-reading the device after a layout change failed"
                ),
            },
            DeviceEvent::Touch { panel_id, gesture } => {
                mqtt.publish_touch(&id, panel_id, gesture);
            }
        }
    }
}

fn publish_snapshot(
    mqtt: &Mqtt,
    id: &DeviceId,
    snapshot: &DeviceSnapshot,
    effects: &watch::Sender<Option<Vec<String>>>,
) {
    for (path, val) in map::snapshot_values(snapshot) {
        mqtt.publish_status(id, path, val);
    }
    if snapshot.effects.is_some() {
        effects.send_replace(snapshot.effects.clone());
    }
}

/// Executes the device's commands one after the other. Nothing is published here: what the
/// device does in response arrives through its event stream.
pub(crate) async fn execute(
    id: DeviceId,
    client: Client,
    mut commands: mpsc::Receiver<SetMessage>,
    effects: watch::Receiver<Option<Vec<String>>>,
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
        match client.send(&command).await {
            Ok(()) => tracing::debug!(device = id.as_str(), command = path, "command sent"),
            Err(error) => tracing::warn!(
                device = id.as_str(),
                command = path,
                error = error.chain(),
                "command failed"
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
