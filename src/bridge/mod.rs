//! Wiring between device events, the model and MQTT.
//!
//! Every device gets its own tasks (event stream, event publishing, command execution), so one
//! failing or hanging device never blocks another. Commands arriving on MQTT are routed to the
//! device's command queue without waiting.

mod device;
pub mod env;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::model::{BrokerConfig, DeviceConfig, DeviceId};
use crate::mqtt::{Mqtt, MqttError, SetMessage, Timing};
use crate::nanoleaf::{self, Client, StreamOptions};
use device::Reachability;

/// Commands waiting per device; more are dropped (and logged) instead of blocking others.
const COMMAND_QUEUE: usize = 16;
/// Device events waiting to be published.
const EVENT_QUEUE: usize = 64;
/// How long a shutdown waits for the broker connection to close.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Timing of both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOptions {
    /// Broker connection.
    pub mqtt: Timing,
    /// Device event streams.
    pub stream: StreamOptions,
    /// Timeout of one device request; `None` waits forever.
    pub request_timeout: Option<Duration>,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            mqtt: Timing::default(),
            stream: StreamOptions::default(),
            request_timeout: Some(nanoleaf::DEFAULT_REQUEST_TIMEOUT),
        }
    }
}

/// Error starting the bridge.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// The broker configuration cannot be used.
    #[error(transparent)]
    Mqtt(#[from] MqttError),
    /// Two devices have the same id.
    #[error("device id `{0}` is configured twice")]
    DuplicateDevice(DeviceId),
    /// The client of a device cannot be built.
    #[error("device `{id}`: {source}")]
    Device {
        /// The device.
        id: DeviceId,
        /// What failed.
        #[source]
        source: nanoleaf::Error,
    },
}

/// The running bridge.
#[derive(Debug)]
pub struct Bridge {
    mqtt: Mqtt,
    mqtt_task: JoinHandle<()>,
    tasks: Vec<JoinHandle<()>>,
}

impl Bridge {
    /// Connects to the broker and starts following every device.
    pub fn start(
        broker: &BrokerConfig,
        devices: &[DeviceConfig],
        options: BridgeOptions,
    ) -> Result<Self, BridgeError> {
        let mut seen = HashSet::new();
        let mut clients = Vec::with_capacity(devices.len());
        for config in devices {
            if !seen.insert(config.id.clone()) {
                return Err(BridgeError::DuplicateDevice(config.id.clone()));
            }
            let client = Client::new(
                &config.host,
                config.port,
                crate::secret::Secret::new(config.token.expose().clone()),
            )
            .map_err(|source| BridgeError::Device {
                id: config.id.clone(),
                source,
            })?
            .with_request_timeout(options.request_timeout);
            clients.push((config.id.clone(), client));
        }

        let (mqtt, set_rx, mqtt_task) = Mqtt::spawn(broker, options.mqtt.clone())?;
        let reachability = Arc::new(Reachability::new(clients.iter().map(|(id, _)| id)));
        let mut tasks = Vec::new();
        let mut routes = HashMap::new();
        for (id, client) in clients {
            tracing::info!(
                device = id.as_str(),
                address = client.label(),
                "following device"
            );
            let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE);
            let (effects_tx, effects_rx) = watch::channel(None);
            let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE);
            tasks.push(nanoleaf::spawn(
                client.clone(),
                options.stream.clone(),
                event_tx.clone(),
            ));
            tasks.push(tokio::spawn(device::follow(
                id.clone(),
                mqtt.clone(),
                client.clone(),
                event_rx,
                Arc::clone(&reachability),
                effects_tx,
            )));
            tasks.push(tokio::spawn(device::execute(
                id.clone(),
                client,
                command_rx,
                effects_rx,
                event_tx,
            )));
            routes.insert(id, command_tx);
        }
        tasks.push(tokio::spawn(route(set_rx, routes)));
        Ok(Self {
            mqtt,
            mqtt_task,
            tasks,
        })
    }

    /// The broker connection.
    pub fn mqtt(&self) -> &Mqtt {
        &self.mqtt
    }

    /// Stops following the devices, publishes `connected = 0` and disconnects.
    pub async fn shutdown(self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            // A cancelled task is what is expected here.
            let _ = task.await;
        }
        self.mqtt.shutdown().await;
        let mut mqtt_task = self.mqtt_task;
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut mqtt_task)
            .await
            .is_err()
        {
            tracing::warn!("broker connection did not close in time");
            mqtt_task.abort();
        }
    }
}

/// Hands every command to its device's queue without waiting.
async fn route(
    mut set_rx: mpsc::Receiver<SetMessage>,
    routes: HashMap<DeviceId, mpsc::Sender<SetMessage>>,
) {
    while let Some(message) = set_rx.recv().await {
        let target = DeviceId::new(message.device.as_str())
            .ok()
            .and_then(|id| routes.get(&id));
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
