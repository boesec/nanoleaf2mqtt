//! The supervisor: owns the device tasks and the broker connection and applies configuration
//! changes while the bridge runs.
//!
//! It is the only task that starts or stops device tasks and switches the broker, and it does so
//! one change at a time, so changes never race each other. Stopping a device aborts its tasks;
//! none of them holds anything another device needs, so this never waits for a device.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::device;
use super::{BridgeOptions, SHUTDOWN_TIMEOUT, client, same_broker, same_device};
use crate::model::{
    BrokerConfig, ConfigChange, DEFAULT_HA_DISCOVERY_PREFIX, DeviceConfig, DeviceId,
};
use crate::mqtt::{LastChangeUpdate, Mqtt, SetMessage, StatusPath};
use crate::nanoleaf;
use crate::store::{LastChangeRecord, Store};

/// Commands waiting per device; more are dropped (and logged) instead of blocking others.
const COMMAND_QUEUE: usize = 16;
/// Device events waiting to be published.
const EVENT_QUEUE: usize = 64;

/// The command queue of every followed device.
type Routes = Arc<RwLock<HashMap<DeviceId, mpsc::Sender<SetMessage>>>>;

/// Why a device's tasks are stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// It starts again right away with a new configuration: its values stay until it reports
    /// again.
    Restart,
    /// It is no longer followed: its pending `lc` values are persisted and its retained topics
    /// cleared.
    Remove,
    /// The bridge stops: everything stays as it is for the goodbye (`connected = 0`).
    Shutdown,
}

/// A device being followed.
struct Running {
    config: DeviceConfig,
    tasks: Vec<JoinHandle<()>>,
}

pub(super) struct Supervisor {
    mqtt: Mqtt,
    set_rx: Option<mpsc::Receiver<SetMessage>>,
    broker: Option<BrokerConfig>,
    ha_discovery: bool,
    store: Option<Store>,
    options: BridgeOptions,
    devices: BTreeMap<DeviceId, Running>,
    routes: Routes,
    /// The ids in `devices`, readable from outside for the status.
    followed: Arc<Mutex<BTreeSet<DeviceId>>>,
}

impl Supervisor {
    pub(super) fn new(
        mqtt: Mqtt,
        set_rx: mpsc::Receiver<SetMessage>,
        broker: Option<BrokerConfig>,
        ha_discovery: bool,
        store: Option<Store>,
        options: BridgeOptions,
        followed: Arc<Mutex<BTreeSet<DeviceId>>>,
    ) -> Self {
        Self {
            mqtt,
            set_rx: Some(set_rx),
            broker,
            ha_discovery,
            store,
            options,
            devices: BTreeMap::new(),
            routes: Arc::new(RwLock::new(HashMap::new())),
            followed,
        }
    }

    /// Starts the initial devices, then applies changes until `stop` fires (or every sender of
    /// `changes` is gone), and shuts everything down.
    pub(super) async fn run(
        mut self,
        devices: Vec<DeviceConfig>,
        mut changes: mpsc::Receiver<ConfigChange>,
        mut stop: oneshot::Receiver<()>,
    ) {
        let router = self
            .set_rx
            .take()
            .map(|set_rx| tokio::spawn(route(set_rx, Arc::clone(&self.routes))));
        self.seed_all().await;
        self.sync_ha_discovery().await;
        for config in devices {
            if self.devices.contains_key(&config.id) {
                tracing::error!(
                    device = config.id.as_str(),
                    "device configured twice, the second one is ignored"
                );
                continue;
            }
            self.start_device(config);
        }
        // Connected only now: the read-back after the connect knows every configured device
        // and the discovery setting.
        if let Some(broker) = self.broker.clone() {
            if let Err(error) = self.mqtt.switch(Some(&broker)).await {
                tracing::error!(%error, "the configured broker cannot be used");
                self.broker = None;
            }
        }
        let mut flush = tokio::time::interval(self.options.lc_flush);
        flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
        flush.tick().await;
        loop {
            tokio::select! {
                _ = &mut stop => break,
                change = changes.recv() => match change {
                    Some(change) => self.apply(change).await,
                    None => break,
                },
                _ = flush.tick() => self.flush().await,
            }
        }

        let ids: Vec<DeviceId> = self.devices.keys().cloned().collect();
        for id in ids {
            self.stop_device(&id, Stop::Shutdown).await;
        }
        if let Some(router) = router {
            router.abort();
            let _ = router.await;
        }
        self.flush().await;
        self.mqtt.shutdown().await;
        self.mqtt.wait_closed(SHUTDOWN_TIMEOUT).await;
    }

    async fn apply(&mut self, change: ConfigChange) {
        match change {
            ConfigChange::Broker(broker) => {
                if same_broker(self.broker.as_ref(), broker.as_ref()) {
                    tracing::debug!("broker unchanged");
                    return;
                }
                match self.mqtt.switch(broker.as_ref()).await {
                    Ok(()) => self.broker = broker,
                    Err(error) => {
                        tracing::error!(%error, "the new broker settings cannot be used, keeping the old ones");
                    }
                }
            }
            ConfigChange::Device { id, run } => {
                let running = self.devices.get(&id).map(|r| &r.config);
                match (running, run) {
                    (Some(current), Some(new)) if same_device(current, &new) => {
                        tracing::debug!(device = id.as_str(), "device unchanged");
                    }
                    (Some(_), Some(new)) => {
                        tracing::info!(device = id.as_str(), "device changed, restarting it");
                        self.stop_device(&id, Stop::Restart).await;
                        self.start_device(new);
                    }
                    (None, Some(new)) => {
                        self.seed(&id).await;
                        self.start_device(new);
                    }
                    (Some(_), None) => {
                        tracing::info!(device = id.as_str(), "no longer following device");
                        self.stop_device(&id, Stop::Remove).await;
                    }
                    (None, None) => {
                        // Not followed; values a previous run may have left are cleared anyway.
                        self.mqtt.clear_device(&id);
                    }
                }
            }
            ConfigChange::HaDiscovery(on) => {
                if self.ha_discovery != on {
                    tracing::info!(on, "Home Assistant discovery switched");
                    self.ha_discovery = on;
                }
                // Also when unchanged: the stored discovery prefix is read again.
                self.sync_ha_discovery().await;
            }
        }
    }

    /// Hands the discovery setting to the broker connection: on under the stored prefix, or off.
    async fn sync_ha_discovery(&self) {
        let prefix = if self.ha_discovery {
            Some(self.ha_discovery_prefix().await)
        } else {
            None
        };
        if let Err(error) = self.mqtt.set_ha_discovery(prefix.as_deref()) {
            tracing::error!(%error, "the Home Assistant discovery prefix cannot be used, discovery is off");
            // Off is always accepted.
            let _ = self.mqtt.set_ha_discovery(None);
        }
    }

    /// The stored Home Assistant discovery prefix; Home Assistant's default without a store.
    async fn ha_discovery_prefix(&self) -> String {
        let Some(store) = &self.store else {
            return DEFAULT_HA_DISCOVERY_PREFIX.to_owned();
        };
        match store.settings().await {
            Ok(settings) => settings.ha_discovery_prefix,
            Err(error) => {
                tracing::warn!(%error, "cannot read the discovery prefix, using the default");
                DEFAULT_HA_DISCOVERY_PREFIX.to_owned()
            }
        }
    }

    fn start_device(&mut self, config: DeviceConfig) {
        let id = config.id.clone();
        let client = match client(&config, &self.options) {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(
                    device = id.as_str(),
                    error = error.chain(),
                    "device cannot be followed"
                );
                return;
            }
        };
        tracing::info!(
            device = id.as_str(),
            address = client.label(),
            "following device"
        );
        self.mqtt.track_device(&id);
        let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE);
        let (effects_tx, effects_rx) = watch::channel(None);
        let (ranges_tx, ranges_rx) = watch::channel(None);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE);
        let (reread_tx, reread_rx) = mpsc::channel(COMMAND_QUEUE);
        let tasks = vec![
            nanoleaf::spawn(client.clone(), self.options.stream.clone(), event_tx),
            tokio::spawn(device::follow(
                id.clone(),
                self.mqtt.clone(),
                client.clone(),
                event_rx,
                reread_rx,
                device::Known {
                    effects: effects_tx,
                    ranges: ranges_tx,
                },
            )),
            tokio::spawn(device::execute(
                id.clone(),
                client,
                command_rx,
                effects_rx,
                ranges_rx,
                reread_tx,
            )),
        ];
        self.routes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), command_tx);
        self.followed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone());
        self.devices.insert(id, Running { config, tasks });
    }

    /// Stops the device's tasks; see [`Stop`] for what else happens.
    async fn stop_device(&mut self, id: &DeviceId, how: Stop) {
        let Some(running) = self.devices.remove(id) else {
            return;
        };
        self.routes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        for task in &running.tasks {
            task.abort();
        }
        for task in running.tasks {
            // A cancelled task is what is expected here.
            let _ = task.await;
        }
        self.followed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        // Nothing of the device publishes any more.
        if how == Stop::Remove {
            self.flush().await;
            self.mqtt.clear_device(id);
        }
    }

    /// Seeds the `lc` values of every stored device.
    async fn seed_all(&self) {
        let Some(store) = &self.store else {
            return;
        };
        match store.last_changes().await {
            Ok(records) => self.seed_records(&records),
            Err(error) => tracing::warn!(%error, "cannot load the last changes"),
        }
    }

    /// Seeds the `lc` values of one device.
    async fn seed(&self, id: &DeviceId) {
        let Some(store) = &self.store else {
            return;
        };
        match store.last_changes_of(id).await {
            Ok(records) => self.seed_records(&records),
            Err(error) => {
                tracing::warn!(device = id.as_str(), %error, "cannot load the last changes");
            }
        }
    }

    fn seed_records(&self, records: &[LastChangeRecord]) {
        for record in records {
            let path = StatusPath::parse(&record.path);
            let val = serde_json::from_str(&record.val).ok();
            match (path, val) {
                (Some(path), Some(val)) => {
                    self.mqtt
                        .seed_last_change(&record.device, path, &val, record.lc);
                }
                _ => tracing::debug!(
                    device = record.device.as_str(),
                    path = record.path,
                    "unusable stored last change ignored"
                ),
            }
        }
    }

    /// Persists the `lc` values that changed since the last flush, in one transaction.
    async fn flush(&self) {
        let updates = self.mqtt.take_last_changes();
        let Some(store) = &self.store else {
            return;
        };
        if updates.is_empty() {
            return;
        }
        let records: Vec<LastChangeRecord> = updates.into_iter().map(record).collect();
        if let Err(error) = store.save_last_changes(&records).await {
            tracing::warn!(%error, count = records.len(), "cannot persist the last changes");
        }
    }
}

fn record(update: LastChangeUpdate) -> LastChangeRecord {
    LastChangeRecord {
        device: update.device,
        path: update.path.as_str().to_owned(),
        val: update.val.to_string(),
        lc: update.lc,
    }
}

/// Hands every command to its device's queue without waiting.
async fn route(mut set_rx: mpsc::Receiver<SetMessage>, routes: Routes) {
    while let Some(message) = set_rx.recv().await {
        let target = DeviceId::new(message.device.as_str()).ok().and_then(|id| {
            routes
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id)
                .cloned()
        });
        let Some(target) = target else {
            tracing::debug!(
                device = message.device,
                "command for an unknown device ignored"
            );
            continue;
        };
        if let Err(error) = target.try_send(message) {
            tracing::warn!(%error, "device busy, command dropped");
        }
    }
}
