//! The broker connection: one `rumqttc` event loop task and the handle that publishes through it.
//!
//! Every retained status message is remembered; after a (re)connect `connected`, `info` and all
//! of them are published again, so a broker that lost its retained store is complete again.
//! While the broker is unreachable nothing is queued: the remembered values go out on reconnect.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rumqttc::{AsyncClient, ConnectReturnCode, Event, EventLoop, LastWill, Outgoing, Packet, QoS};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::map::touch_value;
use super::payload::{EventPayload, InfoPayload, LastChange, StatePayload, to_ms};
use super::topic::{CommandPath, StatusPath, Topics};
use crate::model::{BrokerConfig, ConfigError, DeviceId, Gesture};

const REQUEST_QUEUE: usize = 1024;
const SET_QUEUE: usize = 64;
const MAX_PACKET: usize = 256 * 1024;

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

/// Error setting up the broker connection.
#[derive(Debug, thiserror::Error)]
pub enum MqttError {
    /// The configuration cannot be used.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

struct State {
    /// A session with the broker exists (ConnAck received, no error since).
    connected: bool,
    all_reachable: bool,
    last_change: LastChange,
    /// Last payload of every retained status topic, for the republish after a reconnect.
    retained: BTreeMap<String, Vec<u8>>,
}

struct Inner {
    client: AsyncClient,
    topics: Topics,
    started_at: DateTime<Utc>,
    state: Mutex<State>,
    ready: watch::Sender<bool>,
    stopping: AtomicBool,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // Never held across an await; the state stays usable after a panic elsewhere.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn try_publish(&self, topic: &str, qos: QoS, retain: bool, payload: Vec<u8>) {
        if let Err(error) = self.client.try_publish(topic, qos, retain, payload) {
            tracing::warn!(topic, %error, "could not queue publish");
        }
    }

    fn connected_value(state: &State) -> &'static [u8] {
        if state.all_reachable { b"2" } else { b"1" }
    }

    /// Subscribes and publishes `connected`, `info` and every remembered status topic.
    fn on_connect(&self, state: &mut State) {
        state.connected = true;
        let filter = self.topics.set_wildcard();
        if let Err(error) = self.client.try_subscribe(&filter, QoS::AtLeastOnce) {
            tracing::warn!(filter, %error, "could not subscribe");
        }
        self.try_publish(
            &self.topics.connected(),
            QoS::AtLeastOnce,
            true,
            Self::connected_value(state).to_vec(),
        );
        match serde_json::to_vec(&InfoPayload::new(self.started_at)) {
            Ok(info) => self.try_publish(&self.topics.info(), QoS::AtLeastOnce, true, info),
            Err(error) => tracing::warn!(%error, "could not serialise info"),
        }
        for (topic, payload) in &state.retained {
            self.try_publish(topic, QoS::AtLeastOnce, true, payload.clone());
        }
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
            .field("prefix", &self.inner.topics.prefix())
            .field("session", &self.is_connected())
            .finish()
    }
}

impl Mqtt {
    /// Starts the connection task. It connects, reconnects forever with backoff until
    /// [`Mqtt::shutdown`], and forwards non-retained commands to the returned receiver.
    pub fn spawn(
        config: &BrokerConfig,
        timing: Timing,
    ) -> Result<(Self, mpsc::Receiver<SetMessage>, JoinHandle<()>), MqttError> {
        let topics = Topics::new(config.prefix.clone())?;
        let mut options =
            rumqttc::MqttOptions::new(config.client_id.clone(), config.host.clone(), config.port);
        options.set_keep_alive(timing.keep_alive);
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
        let (set_tx, set_rx) = mpsc::channel(SET_QUEUE);
        let inner = Arc::new(Inner {
            client,
            topics,
            started_at: Utc::now(),
            state: Mutex::new(State {
                connected: false,
                all_reachable: false,
                last_change: LastChange::new(),
                retained: BTreeMap::new(),
            }),
            ready: watch::channel(false).0,
            stopping: AtomicBool::new(false),
        });
        tracing::info!(
            broker = %format_args!("{}:{}", config.host, config.port),
            prefix = inner.topics.prefix(),
            "connecting to the broker"
        );
        let task = tokio::spawn(run(eventloop, Arc::clone(&inner), set_tx, timing));
        Ok((Self { inner }, set_rx, task))
    }

    /// The topic builder in use.
    pub fn topics(&self) -> &Topics {
        &self.inner.topics
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
        if path == StatusPath::Touch {
            tracing::warn!("touch is an event; use publish_touch");
            return;
        }
        let topic = self.inner.topics.status(device, path);
        let ts = to_ms(Utc::now());
        let mut state = self.inner.state();
        let lc = state.last_change.observe(&topic, &val, ts);
        let payload = match serde_json::to_vec(&StatePayload { val, ts, lc }) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(topic, %error, "could not serialise status");
                return;
            }
        };
        if state.connected {
            self.inner
                .try_publish(&topic, QoS::AtLeastOnce, true, payload.clone());
        }
        state.retained.insert(topic, payload);
    }

    /// Publishes a touch event on `status/<device>/touch`: not retained, QoS 0, `{val, ts}`.
    /// Dropped while the broker is unreachable.
    pub fn publish_touch(&self, device: &DeviceId, panel: Option<u32>, gesture: Gesture) {
        let topic = self.inner.topics.status(device, StatusPath::Touch);
        let payload = EventPayload {
            val: touch_value(panel, gesture),
            ts: to_ms(Utc::now()),
        };
        let state = self.inner.state();
        if !state.connected {
            tracing::debug!(topic, "broker unreachable, touch dropped");
            return;
        }
        match serde_json::to_vec(&payload) {
            Ok(payload) => self
                .inner
                .try_publish(&topic, QoS::AtMostOnce, false, payload),
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
        if state.connected {
            self.inner.try_publish(
                &self.inner.topics.connected(),
                QoS::AtLeastOnce,
                true,
                Inner::connected_value(&state).to_vec(),
            );
        }
    }

    /// Clears every retained status topic of a removed device (empty retained publish) and
    /// forgets its values.
    pub fn clear_device(&self, device: &DeviceId) {
        let prefix = self.inner.topics.status_prefix(device);
        let mut state = self.inner.state();
        state
            .retained
            .retain(|topic, _| !topic.starts_with(&prefix));
        state.last_change.forget_under(&prefix);
        if !state.connected {
            tracing::warn!(
                device = device.as_str(),
                "broker unreachable, retained topics of the removed device stay"
            );
            return;
        }
        for path in StatusPath::ALL.into_iter().filter(|p| p.retained()) {
            self.inner.try_publish(
                &self.inner.topics.status(device, path),
                QoS::AtLeastOnce,
                true,
                Vec::new(),
            );
        }
    }

    /// Publishes `connected = 0`, disconnects and lets the task end.
    pub async fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        if self.is_connected() {
            let topic = self.inner.topics.connected();
            if let Err(error) = self
                .inner
                .client
                .publish(topic, QoS::AtLeastOnce, true, "0")
                .await
            {
                tracing::debug!(%error, "could not publish connected = 0");
            }
        }
        if let Err(error) = self.inner.client.disconnect().await {
            tracing::debug!(%error, "disconnect request failed");
        }
    }
}

async fn run(
    mut eventloop: EventLoop,
    inner: Arc<Inner>,
    set_tx: mpsc::Sender<SetMessage>,
    timing: Timing,
) {
    let mut backoff = Backoff::new(timing.backoff_initial, timing.backoff_max);
    let mut failing = false;
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(ack))) => {
                if ack.code != ConnectReturnCode::Success {
                    // rumqttc reports a refusal as an error on the next poll as well.
                    tracing::warn!(code = ?ack.code, "broker refused the connection");
                    continue;
                }
                tracing::info!("connected to the broker");
                failing = false;
                backoff.reset();
                let mut state = inner.state();
                inner.on_connect(&mut state);
            }
            Ok(Event::Incoming(Packet::SubAck(_))) => {
                inner.ready.send_replace(true);
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                dispatch(
                    &inner,
                    &set_tx,
                    &publish.topic,
                    publish.retain,
                    &publish.payload,
                );
            }
            Ok(Event::Outgoing(Outgoing::Disconnect)) => {
                if inner.stopping.load(Ordering::SeqCst) {
                    inner.state().connected = false;
                    inner.ready.send_replace(false);
                    tracing::info!("disconnected from the broker");
                    return;
                }
            }
            Ok(_) => {}
            Err(error) => {
                let was_connected = std::mem::replace(&mut inner.state().connected, false);
                inner.ready.send_replace(false);
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
    inner: &Inner,
    set_tx: &mpsc::Sender<SetMessage>,
    topic: &str,
    retain: bool,
    payload: &[u8],
) {
    let Some(set) = inner.topics.parse_set(topic) else {
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
}
