//! Wiring between device events, the model and MQTT, reconfigurable while it runs.
//!
//! One supervisor task owns the broker connection and the tasks of every device. It applies
//! [`ConfigChange`] messages one after the other: a device added is started, a changed device is
//! restarted, a removed or disabled device is stopped and its retained topics are cleared, a new
//! broker or prefix moves the connection (`connected = 0` on the old one, full republish on the
//! new one). Nothing of this waits for a device, so one device never holds up another.
//!
//! Every device gets its own tasks (event stream, event publishing, command execution).
//! Commands arriving on MQTT are routed to the device's command queue without waiting.

mod device;
mod supervisor;

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::model::{
    BridgeStatus, BrokerConfig, ConfigChange, DeviceConfig, DeviceId, DeviceStatus, PairError,
};
use crate::mqtt::{Mqtt, MqttError, StatusPath, Timing, Topics};
use crate::nanoleaf::{self, Client, StreamOptions};
use crate::secret::Secret;
use crate::store::Store;
use supervisor::Supervisor;

/// Configuration changes waiting for the supervisor.
const CHANGE_QUEUE: usize = 64;
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
    /// How often changed `lc` values are written to the store (and once more at shutdown).
    pub lc_flush: Duration,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            mqtt: Timing::default(),
            stream: StreamOptions::default(),
            request_timeout: Some(nanoleaf::DEFAULT_REQUEST_TIMEOUT),
            lc_flush: Duration::from_secs(5),
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

/// What the bridge starts with.
#[derive(Debug, Default)]
pub struct BridgeSetup {
    /// The broker; `None` waits for one to be configured.
    pub broker: Option<BrokerConfig>,
    /// The devices to follow.
    pub devices: Vec<DeviceConfig>,
    /// Whether Home Assistant discovery is wanted.
    pub ha_discovery: bool,
    /// Where `lc` values are loaded from at start and persisted to; `None` keeps them in memory.
    pub store: Option<Store>,
    /// Timing.
    pub options: BridgeOptions,
}

/// The running bridge.
#[derive(Debug)]
pub struct Bridge {
    handle: BridgeHandle,
    stop: oneshot::Sender<()>,
    supervisor: JoinHandle<()>,
}

impl Bridge {
    /// Connects to the broker and starts following every device: a fixed configuration that
    /// can still be changed through [`Bridge::handle`].
    pub fn start(
        broker: &BrokerConfig,
        devices: &[DeviceConfig],
        options: BridgeOptions,
    ) -> Result<Self, BridgeError> {
        Topics::new(broker.prefix.clone()).map_err(MqttError::from)?;
        let mut seen = HashSet::new();
        for config in devices {
            if !seen.insert(config.id.clone()) {
                return Err(BridgeError::DuplicateDevice(config.id.clone()));
            }
            client(config, &options).map_err(|source| BridgeError::Device {
                id: config.id.clone(),
                source,
            })?;
        }
        Ok(Self::launch(BridgeSetup {
            broker: Some(broker.clone()),
            devices: devices.to_vec(),
            ha_discovery: false,
            store: None,
            options,
        }))
    }

    /// Starts the bridge. Problems with single parts (a broker prefix or a device that cannot be
    /// used) are logged and leave that part out; everything else runs.
    pub fn launch(setup: BridgeSetup) -> Self {
        let BridgeSetup {
            broker,
            devices,
            ha_discovery,
            store,
            options,
        } = setup;
        let (mqtt, set_rx, broker) = match broker {
            Some(config) => match Mqtt::spawn(&config, options.mqtt.clone()) {
                // The connection task runs on its own; the handle tracks its end.
                Ok((mqtt, set_rx, _task)) => (mqtt, set_rx, Some(config)),
                Err(error) => {
                    tracing::error!(%error, "the configured broker cannot be used");
                    let (mqtt, set_rx) = Mqtt::new(options.mqtt.clone());
                    (mqtt, set_rx, None)
                }
            },
            None => {
                tracing::info!("no broker configured yet");
                let (mqtt, set_rx) = Mqtt::new(options.mqtt.clone());
                (mqtt, set_rx, None)
            }
        };
        let (changes_tx, changes_rx) = mpsc::channel(CHANGE_QUEUE);
        let (stop_tx, stop_rx) = oneshot::channel();
        let followed = Arc::new(Mutex::new(BTreeSet::new()));
        let supervisor = Supervisor::new(
            mqtt.clone(),
            set_rx,
            broker,
            ha_discovery,
            store,
            options,
            Arc::clone(&followed),
        );
        let supervisor = tokio::spawn(supervisor.run(devices, changes_rx, stop_rx));
        Self {
            handle: BridgeHandle {
                mqtt,
                changes: changes_tx,
                followed,
            },
            stop: stop_tx,
            supervisor,
        }
    }

    /// The broker connection.
    pub fn mqtt(&self) -> &Mqtt {
        &self.handle.mqtt
    }

    /// A handle for configuration changes and status.
    pub fn handle(&self) -> BridgeHandle {
        self.handle.clone()
    }

    /// Stops following the devices, persists pending `lc` values, publishes `connected = 0` and
    /// disconnects.
    pub async fn shutdown(self) {
        // The supervisor may have ended already; then there is nothing left to stop.
        let _ = self.stop.send(());
        if let Err(error) = self.supervisor.await {
            tracing::error!(%error, "the bridge supervisor failed");
        }
    }
}

/// Handle to the running bridge: configuration changes in, status out. Cheap to clone.
#[derive(Debug, Clone)]
pub struct BridgeHandle {
    mqtt: Mqtt,
    changes: mpsc::Sender<ConfigChange>,
    followed: Arc<Mutex<BTreeSet<DeviceId>>>,
}

/// The bridge has stopped; the change was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the bridge has stopped")]
pub struct BridgeStopped;

impl BridgeHandle {
    /// The channel configuration changes are sent to. They are applied in the order sent.
    pub fn changes(&self) -> mpsc::Sender<ConfigChange> {
        self.changes.clone()
    }

    /// Hands a change to the supervisor; it is applied shortly after.
    pub async fn apply(&self, change: ConfigChange) -> Result<(), BridgeStopped> {
        self.changes.send(change).await.map_err(|_| BridgeStopped)
    }

    /// What the bridge does right now: broker session, the devices it follows and their last
    /// published values.
    pub fn status(&self) -> BridgeStatus {
        let followed = self
            .followed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut values = self.mqtt.values();
        let devices = followed
            .into_iter()
            .map(|id| {
                let device_values = values.remove(&id).unwrap_or_default();
                let reachable = device_values
                    .iter()
                    .find(|(path, _)| *path == StatusPath::Reachable)
                    .and_then(|(_, val)| val.as_bool());
                DeviceStatus {
                    id,
                    reachable,
                    values: device_values
                        .into_iter()
                        .filter(|(path, _)| *path != StatusPath::Reachable)
                        .map(|(path, val)| (path.as_str().to_owned(), val.to_string()))
                        .collect(),
                }
            })
            .collect();
        BridgeStatus {
            broker_connected: self.mqtt.is_connected(),
            devices,
        }
    }
}

/// Requests a new auth token from the device at `host:port`; the device must be in pairing
/// mode. The token is never logged.
pub async fn pair(host: &str, port: u16) -> Result<Secret<String>, PairError> {
    nanoleaf::pair(host, port)
        .await
        .map_err(|error| match error {
            nanoleaf::Error::PairingRefused => PairError::NotInPairingMode,
            other => PairError::Failed(other.chain()),
        })
}

fn client(config: &DeviceConfig, options: &BridgeOptions) -> Result<Client, nanoleaf::Error> {
    Ok(Client::new(
        &config.host,
        config.port,
        Secret::new(config.token.expose().clone()),
    )?
    .with_request_timeout(options.request_timeout))
}

/// Whether two device configurations would talk to the device in the same way.
fn same_device(a: &DeviceConfig, b: &DeviceConfig) -> bool {
    a.id == b.id && a.host == b.host && a.port == b.port && a.token.expose() == b.token.expose()
}

/// Whether two broker configurations lead to the same connection. The client id is not
/// compared: it is the process's own.
fn same_broker(a: Option<&BrokerConfig>, b: Option<&BrokerConfig>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.host == b.host
                && a.port == b.port
                && a.username == b.username
                && a.password.as_ref().map(|p| p.expose())
                    == b.password.as_ref().map(|p| p.expose())
                && a.prefix == b.prefix
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(host: &str, token: &str) -> DeviceConfig {
        DeviceConfig {
            id: DeviceId::new("dev").unwrap(),
            host: host.to_owned(),
            port: 16021,
            token: Secret::new(token.to_owned()),
        }
    }

    fn broker(prefix: &str, password: Option<&str>) -> BrokerConfig {
        BrokerConfig {
            host: "broker".to_owned(),
            port: 1883,
            username: Some("user".to_owned()),
            password: password.map(|p| Secret::new(p.to_owned())),
            client_id: "a".to_owned(),
            prefix: prefix.to_owned(),
        }
    }

    #[test]
    fn device_comparison() {
        assert!(same_device(&device("h", "t"), &device("h", "t")));
        assert!(!same_device(&device("h", "t"), &device("h2", "t")));
        assert!(!same_device(&device("h", "t"), &device("h", "t2")));
    }

    #[test]
    fn broker_comparison_ignores_the_client_id() {
        let a = broker("p", Some("pw"));
        let mut b = broker("p", Some("pw"));
        b.client_id = "b".to_owned();
        assert!(same_broker(Some(&a), Some(&b)));
        assert!(!same_broker(Some(&a), Some(&broker("q", Some("pw")))));
        assert!(!same_broker(Some(&a), Some(&broker("p", Some("other")))));
        assert!(!same_broker(Some(&a), Some(&broker("p", None))));
        assert!(!same_broker(Some(&a), None));
        assert!(same_broker(None, None));
    }
}
