//! The broker connection: one `rumqttc` event loop task per connection and the handle that
//! publishes through it.
//!
//! Every retained status value is remembered per device and path; after a (re)connect
//! `connected`, `info` and all of them are published again, so a broker that lost its retained
//! store is complete again. While the broker is unreachable (or none is configured) nothing is
//! queued: the remembered values go out on the next connect.
//!
//! The broker can be switched at runtime ([`Mqtt::switch`]): the old connection publishes
//! `connected = 0` and disconnects, the new one starts with a full republish under its prefix.
//! Every clone of the handle follows the switch.
//!
//! Home Assistant discovery ([`Mqtt::set_ha_discovery`]) works the same way: what each device
//! reported about itself is remembered, and while discovery is on its messages are published on
//! every connect, under the current prefix. Switching it off or forgetting a device clears its
//! discovery topics (empty retained); a clear that cannot be sent while the broker is
//! unreachable is sent on the next connect.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rumqttc::{AsyncClient, ConnectReturnCode, Event, EventLoop, LastWill, Outgoing, Packet, QoS};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};

use super::discovery::{self, DiscoveryDevice};
use super::map::touch_value;
use super::payload::{EventPayload, InfoPayload, LastChange, StatePayload, to_ms};
use super::topic::{CommandPath, StatusPath, Topics};
use crate::model::{BrokerConfig, ConfigError, DeviceId, DeviceSnapshot, Gesture, validate_prefix};

const REQUEST_QUEUE: usize = 1024;
const SET_QUEUE: usize = 64;
const MAX_PACKET: usize = 256 * 1024;
/// How long a connection being replaced may take to say goodbye.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Timing of the broker connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    /// MQTT keep-alive interval.
    pub keep_alive: Duration,
    /// First delay before reconnecting.
    pub backoff_initial: Duration,
    /// Longest delay between reconnect attempts.
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            keep_alive: Duration::from_secs(30),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// A command received on `<prefix>/set/<id>/<path>` (not retained).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetMessage {
    /// The device id level as received; not checked against the configuration.
    pub device: String,
    /// The command.
    pub path: CommandPath,
    /// The raw payload.
    pub payload: Vec<u8>,
}

/// A status value whose `lc` changed since it was last taken with
/// [`Mqtt::take_last_changes`].
#[derive(Debug, Clone, PartialEq)]
pub struct LastChangeUpdate {
    /// The device.
    pub device: DeviceId,
    /// The status path.
    pub path: StatusPath,
    /// The value.
    pub val: Value,
    /// When it last changed, ms since the epoch.
    pub lc: i64,
}

/// Error setting up the broker connection.
#[derive(Debug, thiserror::Error)]
pub enum MqttError {
    /// The configuration cannot be used.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

type Key = (DeviceId, StatusPath);

/// The last retained publish of a status topic.
struct Retained {
    val: Value,
    payload: Vec<u8>,
}

/// One connection to one broker.
struct Connection {
    generation: u64,
    client: AsyncClient,
    topics: Topics,
    abort: AbortHandle,
    /// `true` once the connection's task has ended.
    closed: watch::Receiver<bool>,
}

struct State {
    /// Generation of the current connection; a task of an older one changes nothing.
    generation: u64,
    connection: Option<Connection>,
    /// A session with the broker exists (ConnAck received, no error since).
    connected: bool,
    all_reachable: bool,
    /// Keyed by `<device>/<path>`, independent of the prefix.
    last_change: LastChange,
    retained: BTreeMap<Key, Retained>,
    /// Values whose `lc` changed and was not taken yet.
    dirty: BTreeMap<Key, (Value, i64)>,
    /// The Home Assistant discovery prefix while discovery is on.
    discovery: Option<String>,
    /// What every device reported about itself, for its discovery messages.
    about: BTreeMap<DeviceId, DiscoveryDevice>,
    /// Discovery topics to clear on the next connect.
    to_clear: BTreeSet<String>,
}

impl State {
    /// The current connection while a session exists.
    fn session(&self) -> Option<&Connection> {
        self.connection.as_ref().filter(|_| self.connected)
    }

    /// Publishes the discovery messages of one device, if discovery is on and a session exists.
    fn publish_discovery(&self, device: &DeviceId) {
        let (Some(prefix), Some(about)) = (&self.discovery, self.about.get(device)) else {
            return;
        };
        let Some(connection) = self.session() else {
            return;
        };
        for (topic, payload) in discovery::messages(&connection.topics, prefix, device, about) {
            match serde_json::to_vec(&payload) {
                Ok(payload) => try_publish(connection, &topic, QoS::AtLeastOnce, true, payload),
                Err(error) => tracing::warn!(topic, %error, "could not serialise discovery"),
            }
        }
    }

    /// Clears the discovery topics of one device under `prefix`, now or on the next connect.
    fn clear_discovery(&mut self, prefix: &str, device: &DeviceId) {
        let topics = discovery::topics(prefix, device);
        match self.session() {
            Some(connection) => {
                for topic in &topics {
                    try_publish(connection, topic, QoS::AtLeastOnce, true, Vec::new());
                }
            }
            None => self.to_clear.extend(topics),
        }
    }
}

struct Inner {
    started_at: DateTime<Utc>,
    timing: Timing,
    set_tx: mpsc::Sender<SetMessage>,
    state: Mutex<State>,
    ready: watch::Sender<bool>,
    stopping: AtomicBool,
}

fn lc_key(device: &DeviceId, path: StatusPath) -> String {
    format!("{device}/{}", path.as_str())
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // Never held across an await; the state stays usable after a panic elsewhere.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn connected_value(state: &State) -> &'static [u8] {
        if state.all_reachable { b"2" } else { b"1" }
    }

    /// Subscribes and publishes `connected`, `info` and every remembered status topic.
    fn on_connect(&self, state: &mut State) {
        state.connected = true;
        if state.connection.is_none() {
            return;
        }
        let to_clear = std::mem::take(&mut state.to_clear);
        let Some(connection) = &state.connection else {
            return;
        };
        let filter = connection.topics.set_wildcard();
        if let Err(error) = connection.client.try_subscribe(&filter, QoS::AtLeastOnce) {
            tracing::warn!(filter, %error, "could not subscribe");
        }
        try_publish(
            connection,
            &connection.topics.connected(),
            QoS::AtLeastOnce,
            true,
            Self::connected_value(state).to_vec(),
        );
        match serde_json::to_vec(&InfoPayload::new(self.started_at)) {
            Ok(info) => try_publish(
                connection,
                &connection.topics.info(),
                QoS::AtLeastOnce,
                true,
                info,
            ),
            Err(error) => tracing::warn!(%error, "could not serialise info"),
        }
        // `reachable = true` promises that the values next to it are current: it goes out after
        // every value.
        let (reachable, values): (Vec<_>, Vec<_>) = state
            .retained
            .iter()
            .partition(|((_, path), _)| *path == StatusPath::Reachable);
        for ((device, path), retained) in values.into_iter().chain(reachable) {
            try_publish(
                connection,
                &connection.topics.status(device, *path),
                QoS::AtLeastOnce,
                true,
                retained.payload.clone(),
            );
        }
        // Clears first: a topic cleared and published again ends up published.
        for topic in to_clear {
            try_publish(connection, &topic, QoS::AtLeastOnce, true, Vec::new());
        }
        for device in state.about.keys() {
            state.publish_discovery(device);
        }
    }
}

fn try_publish(connection: &Connection, topic: &str, qos: QoS, retain: bool, payload: Vec<u8>) {
    if let Err(error) = connection.client.try_publish(topic, qos, retain, payload) {
        tracing::warn!(topic, %error, "could not queue publish");
    }
}

/// Handle to the broker connection. Cheap to clone.
#[derive(Clone)]
pub struct Mqtt {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Mqtt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mqtt")
            .field("prefix", &self.prefix())
            .field("session", &self.is_connected())
            .finish()
    }
}

impl Mqtt {
    /// A handle without a broker: values are remembered, nothing is published until
    /// [`Mqtt::switch`] names a broker. Non-retained commands of every later connection arrive
    /// on the returned receiver.
    pub fn new(timing: Timing) -> (Self, mpsc::Receiver<SetMessage>) {
        let (set_tx, set_rx) = mpsc::channel(SET_QUEUE);
        let inner = Arc::new(Inner {
            started_at: Utc::now(),
            timing,
            set_tx,
            state: Mutex::new(State {
                generation: 0,
                connection: None,
                connected: false,
                all_reachable: false,
                last_change: LastChange::new(),
                retained: BTreeMap::new(),
                dirty: BTreeMap::new(),
                discovery: None,
                about: BTreeMap::new(),
                to_clear: BTreeSet::new(),
            }),
            ready: watch::channel(false).0,
            stopping: AtomicBool::new(false),
        });
        (Self { inner }, set_rx)
    }

    /// Starts the connection task. It connects, reconnects forever with backoff until
    /// [`Mqtt::shutdown`] or [`Mqtt::switch`], and forwards non-retained commands to the returned
    /// receiver.
    pub fn spawn(
        config: &BrokerConfig,
        timing: Timing,
    ) -> Result<(Self, mpsc::Receiver<SetMessage>, JoinHandle<()>), MqttError> {
        let topics = Topics::new(config.prefix.clone())?;
        let (mqtt, set_rx) = Self::new(timing);
        let task = mqtt.connect(config, topics);
        Ok((mqtt, set_rx, task))
    }

    fn connect(&self, config: &BrokerConfig, topics: Topics) -> JoinHandle<()> {
        let mut options =
            rumqttc::MqttOptions::new(config.client_id.clone(), config.host.clone(), config.port);
        options.set_keep_alive(self.inner.timing.keep_alive);
        options.set_clean_session(true);
        options.set_max_packet_size(MAX_PACKET, MAX_PACKET);
        options.set_last_will(LastWill::new(
            topics.connected(),
            "0",
            QoS::AtLeastOnce,
            true,
        ));
        if let Some(username) = &config.username {
            let password = config
                .password
                .as_ref()
                .map(|p| p.expose().clone())
                .unwrap_or_default();
            options.set_credentials(username.clone(), password);
        }
        let (client, eventloop) = AsyncClient::new(options, REQUEST_QUEUE);
        tracing::info!(
            broker = %format_args!("{}:{}", config.host, config.port),
            prefix = topics.prefix(),
            "connecting to the broker"
        );
        let mut state = self.inner.state();
        state.generation += 1;
        let generation = state.generation;
        let (closed_tx, closed) = watch::channel(false);
        let task = tokio::spawn(run(
            eventloop,
            Arc::clone(&self.inner),
            generation,
            topics.clone(),
            closed_tx,
        ));
        state.connection = Some(Connection {
            generation,
            client,
            topics,
            abort: task.abort_handle(),
            closed,
        });
        task
    }

    /// Moves to another broker or prefix, or (`None`) disconnects for good: the current
    /// connection publishes `connected = 0` and disconnects, then the new one starts and
    /// republishes everything remembered. Values published meanwhile are remembered and go out
    /// with the republish.
    pub async fn switch(&self, config: Option<&BrokerConfig>) -> Result<(), MqttError> {
        let topics = config.map(|c| Topics::new(c.prefix.clone())).transpose()?;
        let (old, was_connected) = {
            let mut state = self.inner.state();
            state.generation += 1;
            let was_connected = std::mem::replace(&mut state.connected, false);
            self.inner.ready.send_replace(false);
            (state.connection.take(), was_connected)
        };
        if let Some(old) = old {
            close(old, was_connected).await;
        }
        if self.inner.stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let (Some(config), Some(topics)) = (config, topics) {
            // The task runs on its own; `closed` tells when it ends.
            drop(self.connect(config, topics));
        } else {
            tracing::info!("no broker configured");
        }
        Ok(())
    }

    /// The prefix of the current connection; `None` without a broker.
    pub fn prefix(&self) -> Option<String> {
        self.inner
            .state()
            .connection
            .as_ref()
            .map(|c| c.topics.prefix().to_owned())
    }

    /// Whether a session with the broker exists.
    pub fn is_connected(&self) -> bool {
        self.inner.state().connected
    }

    /// `true` once the command subscription of the current session is confirmed, `false` while
    /// disconnected.
    pub fn ready(&self) -> watch::Receiver<bool> {
        self.inner.ready.subscribe()
    }

    /// Publishes `val` on `status/<device>/<path>` as `{val, ts, lc}`, retained, and remembers it
    /// for the republish after a reconnect. `touch` goes through [`Mqtt::publish_touch`].
    pub fn publish_status(&self, device: &DeviceId, path: StatusPath, val: Value) {
        self.publish(device, path, val, false);
    }

    /// Like [`Mqtt::publish_status`], but only if `val` differs from the value last published on
    /// the topic (or none was). Returns whether it was published.
    pub fn publish_status_if_changed(
        &self,
        device: &DeviceId,
        path: StatusPath,
        val: Value,
    ) -> bool {
        self.publish(device, path, val, true)
    }

    fn publish(&self, device: &DeviceId, path: StatusPath, val: Value, only_changed: bool) -> bool {
        if !path.retained() {
            tracing::warn!("touch is an event; use publish_touch");
            return false;
        }
        let key = lc_key(device, path);
        let ts = to_ms(Utc::now());
        let mut state = self.inner.state();
        let changed = state.last_change.last(&key) != Some(&val);
        if only_changed && !changed {
            return false;
        }
        let lc = state.last_change.observe(&key, &val, ts);
        let payload = match serde_json::to_vec(&StatePayload {
            val: val.clone(),
            ts,
            lc,
        }) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(device = device.as_str(), path = path.as_str(), %error, "could not serialise status");
                return false;
            }
        };
        if state.connected {
            if let Some(connection) = &state.connection {
                try_publish(
                    connection,
                    &connection.topics.status(device, path),
                    QoS::AtLeastOnce,
                    true,
                    payload.clone(),
                );
            }
        }
        if changed {
            state
                .dirty
                .insert((device.clone(), path), (val.clone(), lc));
        }
        state
            .retained
            .insert((device.clone(), path), Retained { val, payload });
        true
    }

    /// Publishes a touch event on `status/<device>/touch`: not retained, QoS 0, `{val, ts}`.
    /// Dropped while the broker is unreachable.
    pub fn publish_touch(&self, device: &DeviceId, panel: Option<u32>, gesture: Gesture) {
        let payload = EventPayload {
            val: touch_value(panel, gesture),
            ts: to_ms(Utc::now()),
        };
        let state = self.inner.state();
        let connection = match &state.connection {
            Some(connection) if state.connected => connection,
            _ => {
                tracing::debug!(
                    device = device.as_str(),
                    "broker unreachable, touch dropped"
                );
                return;
            }
        };
        let topic = connection.topics.status(device, StatusPath::Touch);
        match serde_json::to_vec(&payload) {
            Ok(payload) => try_publish(connection, &topic, QoS::AtMostOnce, false, payload),
            Err(error) => tracing::warn!(topic, %error, "could not serialise touch"),
        }
    }

    /// Sets `connected` to `2` (all devices reachable) or `1`.
    pub fn set_all_reachable(&self, all_reachable: bool) {
        if self.inner.stopping.load(Ordering::SeqCst) {
            return;
        }
        let mut state = self.inner.state();
        if state.all_reachable == all_reachable {
            return;
        }
        state.all_reachable = all_reachable;
        if let (true, Some(connection)) = (state.connected, &state.connection) {
            try_publish(
                connection,
                &connection.topics.connected(),
                QoS::AtLeastOnce,
                true,
                Inner::connected_value(&state).to_vec(),
            );
        }
    }

    /// Clears every retained status topic of a device that is no longer followed (empty
    /// retained publish) and forgets its values, including `lc` values not taken yet. While Home
    /// Assistant discovery is on, its discovery topics are cleared as well.
    pub fn clear_device(&self, device: &DeviceId) {
        let mut state = self.inner.state();
        state.retained.retain(|(d, _), _| d != device);
        state.dirty.retain(|(d, _), _| d != device);
        state.last_change.forget_under(&format!("{device}/"));
        state.about.remove(device);
        if let Some(prefix) = state.discovery.clone() {
            state.clear_discovery(&prefix, device);
        }
        let connection = match &state.connection {
            Some(connection) if state.connected => connection,
            _ => {
                tracing::warn!(
                    device = device.as_str(),
                    "broker unreachable, retained topics of the device stay"
                );
                return;
            }
        };
        for path in StatusPath::ALL.into_iter().filter(|p| p.retained()) {
            try_publish(
                connection,
                &connection.topics.status(device, path),
                QoS::AtLeastOnce,
                true,
                Vec::new(),
            );
        }
    }

    /// Switches Home Assistant discovery on (under `prefix`) or off (`None`). On: the discovery
    /// messages of every device that has reported are published. Off, or another prefix: the
    /// topics under the old prefix are cleared. The same setting again changes nothing.
    pub fn set_ha_discovery(&self, prefix: Option<&str>) -> Result<(), MqttError> {
        if let Some(prefix) = prefix {
            validate_prefix(prefix)?;
        }
        let mut state = self.inner.state();
        if state.discovery.as_deref() == prefix {
            return Ok(());
        }
        let devices: Vec<DeviceId> = state.about.keys().cloned().collect();
        if let Some(old) = state.discovery.take() {
            for device in &devices {
                state.clear_discovery(&old, device);
            }
        }
        state.discovery = prefix.map(str::to_owned);
        for device in &devices {
            state.publish_discovery(device);
        }
        Ok(())
    }

    /// The Home Assistant discovery prefix while discovery is on.
    pub fn ha_discovery(&self) -> Option<String> {
        self.inner.state().discovery.clone()
    }

    /// Takes over what a snapshot of the device reports about it (see
    /// [`DiscoveryDevice::update`]). The first snapshot of a device, and every one that changes
    /// what is known, (re)publishes its discovery messages while discovery is on.
    pub fn describe_device(&self, device: &DeviceId, snapshot: &DeviceSnapshot) {
        let mut state = self.inner.state();
        let known = state.about.contains_key(device);
        let changed = state
            .about
            .entry(device.clone())
            .or_default()
            .update(snapshot);
        if changed || !known {
            state.publish_discovery(device);
        }
    }

    /// The value last published on `status/<device>/<path>`.
    pub fn value(&self, device: &DeviceId, path: StatusPath) -> Option<Value> {
        self.inner
            .state()
            .retained
            .get(&(device.clone(), path))
            .map(|r| r.val.clone())
    }

    /// Seeds the last change of a status value, typically from persisted values before the
    /// device reports: a first report of the same value keeps `lc`. A value already known is
    /// left alone.
    pub fn seed_last_change(&self, device: &DeviceId, path: StatusPath, val: &Value, lc: i64) {
        let key = lc_key(device, path);
        let mut state = self.inner.state();
        if state.last_change.last(&key).is_none() {
            state.last_change.observe(&key, val, lc);
        }
    }

    /// Takes every value whose `lc` changed since the last call, for persisting.
    pub fn take_last_changes(&self) -> Vec<LastChangeUpdate> {
        let dirty = std::mem::take(&mut self.inner.state().dirty);
        dirty
            .into_iter()
            .map(|((device, path), (val, lc))| LastChangeUpdate {
                device,
                path,
                val,
                lc,
            })
            .collect()
    }

    /// The last value published per status path of every device, in device and path order.
    pub fn values(&self) -> BTreeMap<DeviceId, Vec<(StatusPath, Value)>> {
        let state = self.inner.state();
        let mut out: BTreeMap<DeviceId, Vec<(StatusPath, Value)>> = BTreeMap::new();
        for ((device, path), retained) in &state.retained {
            out.entry(device.clone())
                .or_default()
                .push((*path, retained.val.clone()));
        }
        out
    }

    /// Publishes `connected = 0`, disconnects and lets the connection task end. Nothing is
    /// connected afterwards, not even by [`Mqtt::switch`].
    pub async fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        let (client, connected_topic, was_connected) = {
            let state = self.inner.state();
            match &state.connection {
                Some(c) => (c.client.clone(), c.topics.connected(), state.connected),
                None => return,
            }
        };
        if was_connected {
            if let Err(error) = client
                .publish(connected_topic, QoS::AtLeastOnce, true, "0")
                .await
            {
                tracing::debug!(%error, "could not publish connected = 0");
            }
        }
        if let Err(error) = client.disconnect().await {
            tracing::debug!(%error, "disconnect request failed");
        }
    }

    /// Waits until the current connection's task has ended, at most `timeout`, then ends it.
    /// Meant after [`Mqtt::shutdown`].
    pub async fn wait_closed(&self, timeout: Duration) {
        let connection = {
            let state = self.inner.state();
            state
                .connection
                .as_ref()
                .map(|c| (c.closed.clone(), c.abort.clone()))
        };
        let Some((mut closed, abort)) = connection else {
            return;
        };
        if tokio::time::timeout(timeout, closed.wait_for(|c| *c))
            .await
            .is_err()
        {
            tracing::warn!("broker connection did not close in time");
            abort.abort();
        }
    }
}

/// Ends a connection that is being replaced: `connected = 0` and a clean disconnect while a
/// session exists, otherwise (reconnecting, nothing to say goodbye to) the task is ended at once.
async fn close(old: Connection, was_connected: bool) {
    let Connection {
        generation,
        client,
        topics,
        abort,
        mut closed,
    } = old;
    if was_connected {
        let goodbye = async {
            if let Err(error) = client
                .publish(topics.connected(), QoS::AtLeastOnce, true, "0")
                .await
            {
                tracing::debug!(%error, "could not publish connected = 0");
            }
            if let Err(error) = client.disconnect().await {
                tracing::debug!(%error, "disconnect request failed");
            }
            let _ = closed.wait_for(|c| *c).await;
        };
        if tokio::time::timeout(CLOSE_TIMEOUT, goodbye).await.is_err() {
            tracing::warn!(generation, "old broker connection did not close in time");
        }
    }
    abort.abort();
    tracing::info!(prefix = topics.prefix(), "left the broker");
}

/// Sends `true` when the connection task ends, also when it is aborted.
struct ClosedGuard(watch::Sender<bool>);

impl Drop for ClosedGuard {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

async fn run(
    mut eventloop: EventLoop,
    inner: Arc<Inner>,
    generation: u64,
    topics: Topics,
    closed: watch::Sender<bool>,
) {
    let _closed = ClosedGuard(closed);
    let mut backoff = Backoff::new(inner.timing.backoff_initial, inner.timing.backoff_max);
    let mut failing = false;
    let current = |state: &State| state.generation == generation;
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(ack))) => {
                if ack.code != ConnectReturnCode::Success {
                    // rumqttc reports a refusal as an error on the next poll as well.
                    tracing::warn!(code = ?ack.code, "broker refused the connection");
                    continue;
                }
                let mut state = inner.state();
                if !current(&state) {
                    continue;
                }
                tracing::info!("connected to the broker");
                failing = false;
                backoff.reset();
                inner.on_connect(&mut state);
            }
            Ok(Event::Incoming(Packet::SubAck(_))) => {
                if current(&inner.state()) {
                    inner.ready.send_replace(true);
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                if current(&inner.state()) {
                    dispatch(
                        &topics,
                        &inner.set_tx,
                        &publish.topic,
                        publish.retain,
                        &publish.payload,
                    );
                }
            }
            Ok(Event::Outgoing(Outgoing::Disconnect)) => {
                let mut state = inner.state();
                let is_current = current(&state);
                if inner.stopping.load(Ordering::SeqCst) || !is_current {
                    if is_current {
                        state.connected = false;
                        inner.ready.send_replace(false);
                    }
                    tracing::info!("disconnected from the broker");
                    return;
                }
            }
            Ok(_) => {}
            Err(error) => {
                let was_connected = {
                    let mut state = inner.state();
                    if !current(&state) {
                        return;
                    }
                    inner.ready.send_replace(false);
                    std::mem::replace(&mut state.connected, false)
                };
                if inner.stopping.load(Ordering::SeqCst) {
                    tracing::info!("disconnected from the broker");
                    return;
                }
                if was_connected || !failing {
                    tracing::warn!(%error, "broker connection lost, reconnecting");
                    failing = true;
                } else {
                    tracing::debug!(%error, "reconnect attempt failed");
                }
                tokio::time::sleep(backoff.next_delay()).await;
            }
        }
    }
}

fn dispatch(
    topics: &Topics,
    set_tx: &mpsc::Sender<SetMessage>,
    topic: &str,
    retain: bool,
    payload: &[u8],
) {
    let Some(set) = topics.parse_set(topic) else {
        tracing::debug!(topic, "ignoring message on an unknown topic");
        return;
    };
    if retain {
        // A broker replaying an old command must not switch the light.
        tracing::info!(topic, "ignoring retained command");
        return;
    }
    let Some(path) = CommandPath::parse(&set.command) else {
        tracing::warn!(topic, "ignoring unknown command");
        return;
    };
    let message = SetMessage {
        device: set.device,
        path,
        payload: payload.to_vec(),
    };
    if let Err(error) = set_tx.try_send(message) {
        tracing::warn!(topic, %error, "command dropped");
    }
}

/// Exponential backoff: `initial`, doubled per attempt, capped at `max`.
#[derive(Debug)]
struct Backoff {
    initial: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            next: initial.min(max),
        }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(self.max);
        delay
    }

    fn reset(&mut self) {
        self.next = self.initial.min(self.max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_cap_and_resets() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let delays: Vec<u64> = (0..7).map(|_| backoff.next_delay().as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn default_timing() {
        let timing = Timing::default();
        assert_eq!(timing.keep_alive, Duration::from_secs(30));
        assert_eq!(timing.backoff_initial, Duration::from_secs(1));
        assert_eq!(timing.backoff_max, Duration::from_secs(30));
    }

    fn dev(id: &str) -> DeviceId {
        DeviceId::new(id).unwrap()
    }

    #[tokio::test]
    async fn without_broker_values_are_remembered_and_lc_changes_are_collected() {
        let (mqtt, _commands) = Mqtt::new(Timing::default());
        assert_eq!(mqtt.prefix(), None);
        assert!(!mqtt.is_connected());
        mqtt.publish_status(&dev("a"), StatusPath::On, Value::Bool(true));
        mqtt.publish_status(&dev("a"), StatusPath::On, Value::Bool(true));
        mqtt.publish_status(&dev("b"), StatusPath::Brightness, Value::from(5));
        let taken = mqtt.take_last_changes();
        assert_eq!(taken.len(), 2, "{taken:?}");
        assert!(mqtt.take_last_changes().is_empty(), "taken once");
        // Same value again: no new lc, nothing to persist.
        mqtt.publish_status(&dev("a"), StatusPath::On, Value::Bool(true));
        assert!(mqtt.take_last_changes().is_empty());
        let values = mqtt.values();
        assert_eq!(values[&dev("a")], vec![(StatusPath::On, Value::Bool(true))]);

        mqtt.clear_device(&dev("a"));
        assert!(!mqtt.values().contains_key(&dev("a")));
    }

    #[tokio::test]
    async fn a_seeded_lc_is_kept_for_the_same_value() {
        let (mqtt, _commands) = Mqtt::new(Timing::default());
        let a = dev("a");
        mqtt.seed_last_change(&a, StatusPath::On, &Value::Bool(true), 1_000);
        mqtt.seed_last_change(&a, StatusPath::Brightness, &Value::from(5), 2_000);
        mqtt.publish_status(&a, StatusPath::On, Value::Bool(true));
        mqtt.publish_status(&a, StatusPath::Brightness, Value::from(6));
        let changes = mqtt.take_last_changes();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].path, StatusPath::Brightness);
        assert!(changes[0].lc > 2_000);
        // Seeding never overrides a value that is already known.
        mqtt.seed_last_change(&a, StatusPath::Brightness, &Value::from(5), 3_000);
        assert!(!mqtt.publish_status_if_changed(&a, StatusPath::Brightness, Value::from(6)));
    }
}
