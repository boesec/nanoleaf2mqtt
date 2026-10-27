//! Following a device: event stream, health check, reconnect with backoff.
//!
//! The device sends no keep-alives; an idle stream is silent for as long as nothing happens, so
//! silence is not a fault. Liveness comes from a periodic `GET /` instead: when it fails, the
//! device is marked unreachable and the stream is reopened.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use super::events::decode;
use super::sse::SseParser;
use super::{Client, Error};
use crate::model::DeviceEvent;

/// Event ids subscribed to: state, layout, effects, touch.
pub const EVENT_IDS: &str = "1,2,3,4";

/// Timing of the stream supervision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    /// First delay before reconnecting after a failure.
    pub backoff_initial: Duration,
    /// Longest delay between reconnect attempts.
    pub backoff_max: Duration,
    /// A connection that stayed up this long resets the backoff.
    pub healthy_after: Duration,
    /// Interval of the `GET /` health check while connected. Its timeout is the client's
    /// request timeout.
    pub health_interval: Duration,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            healthy_after: Duration::from_secs(60),
            health_interval: Duration::from_secs(30),
        }
    }
}

/// Starts [`run`] as a task.
pub fn spawn(
    client: Client,
    options: StreamOptions,
    events: mpsc::Sender<DeviceEvent>,
) -> JoinHandle<()> {
    tokio::spawn(run(client, options, events))
}

/// Follows the device until the receiver of `events` is dropped; never gives up otherwise.
///
/// Emits [`DeviceEvent::Reachable`] followed by a [`DeviceEvent::Snapshot`] on every
/// (re)connect, the decoded stream events while connected, and [`DeviceEvent::Unreachable`]
/// once when the connection is lost or cannot be made (also at start).
pub async fn run(client: Client, options: StreamOptions, events: mpsc::Sender<DeviceEvent>) {
    let mut backoff = Backoff::new(options.backoff_initial, options.backoff_max);
    let mut reachable: Option<bool> = None;
    let mut lost_at: Option<Instant> = None;
    loop {
        let mut connected_at = None;
        let err = match session(
            &client,
            &options,
            &events,
            &mut reachable,
            &mut lost_at,
            &mut connected_at,
        )
        .await
        {
            Ended::ReceiverGone => return,
            Ended::Failed(err) => err,
        };
        if reachable == Some(false) {
            tracing::debug!(
                device = client.label(),
                error = err.chain(),
                "device still unreachable"
            );
        } else {
            tracing::warn!(
                device = client.label(),
                error = err.chain(),
                "device unreachable"
            );
            reachable = Some(false);
            lost_at = Some(Instant::now());
            if events.send(DeviceEvent::Unreachable).await.is_err() {
                return;
            }
        }
        if connected_at.is_some_and(|at: Instant| at.elapsed() >= options.healthy_after) {
            backoff.reset();
        }
        let delay = backoff.next_delay();
        tracing::debug!(
            device = client.label(),
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "reconnecting after delay"
        );
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = events.closed() => return,
        }
    }
}

enum Ended {
    ReceiverGone,
    Failed(Error),
}

impl From<Error> for Ended {
    fn from(err: Error) -> Self {
        Self::Failed(err)
    }
}

/// One connection: open the stream, read the full state, then forward events until something
/// fails.
async fn session(
    client: &Client,
    options: &StreamOptions,
    events: &mpsc::Sender<DeviceEvent>,
    reachable: &mut Option<bool>,
    lost_at: &mut Option<Instant>,
    connected_at: &mut Option<Instant>,
) -> Ended {
    // The stream is opened before the state is read, so that nothing happening in between is
    // lost.
    let response = match client.open_events(EVENT_IDS).await {
        Ok(response) => response,
        Err(err) => return err.into(),
    };
    let snapshot = match client.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(err) => return err.into(),
    };
    *connected_at = Some(Instant::now());
    if *reachable != Some(true) {
        match lost_at.take() {
            Some(since) => tracing::info!(
                device = client.label(),
                away = format_args!("{}s", since.elapsed().as_secs()),
                "device reachable"
            ),
            None => tracing::info!(device = client.label(), "device reachable"),
        }
        *reachable = Some(true);
        if events.send(DeviceEvent::Reachable).await.is_err() {
            return Ended::ReceiverGone;
        }
    }
    if events
        .send(DeviceEvent::Snapshot(Box::new(snapshot)))
        .await
        .is_err()
    {
        return Ended::ReceiverGone;
    }

    let mut body = response.bytes_stream();
    let mut parser = SseParser::new();
    let mut health = tokio::time::interval_at(
        Instant::now() + options.health_interval,
        options.health_interval,
    );
    health.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = events.closed() => return Ended::ReceiverGone,
            chunk = body.next() => match chunk {
                None => return Error::StreamClosed.into(),
                Some(Err(err)) => return Error::http("GET /events", err).into(),
                Some(Ok(bytes)) => {
                    for frame in parser.push(&bytes) {
                        for event in decode(&frame) {
                            if events.send(event).await.is_err() {
                                return Ended::ReceiverGone;
                            }
                        }
                    }
                }
            },
            _ = health.tick() => {
                if let Err(err) = client.snapshot().await {
                    return err.into();
                }
            }
        }
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
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        let delays: Vec<u64> = (0..9).map(|_| backoff.next_delay().as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn default_options() {
        let options = StreamOptions::default();
        assert_eq!(options.backoff_initial, Duration::from_secs(1));
        assert_eq!(options.backoff_max, Duration::from_secs(60));
        assert_eq!(options.health_interval, Duration::from_secs(30));
    }
}
