//! Entry point: wiring only.
#![deny(unsafe_code)]

use std::sync::Arc;

use clap::{Parser, Subcommand};
use nanoleaf2mqtt::bridge::{self, Bridge, BridgeHandle, BridgeOptions, BridgeSetup};
use nanoleaf2mqtt::config::Config;
use nanoleaf2mqtt::model::{BridgeStatus, PairError};
use nanoleaf2mqtt::nanoleaf;
use nanoleaf2mqtt::secret::Secret;
use nanoleaf2mqtt::store::Store;
use nanoleaf2mqtt::web::{self, AppState, AuthTiming, Backend, BoxFuture};
use tokio::sync::oneshot;
use tracing_subscriber::EnvFilter;

/// Bridge between Nanoleaf light panels and MQTT.
///
/// Without a subcommand the service runs until Ctrl-C or SIGTERM. Broker and devices are
/// configured in the web UI and stored in the data directory.
///
/// Bootstrap settings come from the environment: NANOLEAF2MQTT_DATA_DIR (default ./data),
/// NANOLEAF2MQTT_HTTP_BIND (default 127.0.0.1:8080) and NANOLEAF2MQTT_LOG (default info).
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Request an auth token from a device and print it once.
    ///
    /// Hold the device's power button for 5-7 seconds until the lights flash, then run this
    /// within 30 seconds.
    Pair {
        /// Address of the device.
        #[arg(long)]
        host: String,
        /// Port of the device's OpenAPI.
        #[arg(long, default_value_t = nanoleaf::DEFAULT_PORT)]
        port: u16,
    },
}

/// What the web UI reaches of the rest of the service.
struct Wiring {
    bridge: BridgeHandle,
}

impl Backend for Wiring {
    fn status(&self) -> BridgeStatus {
        self.bridge.status()
    }

    fn pair<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<Secret<String>, PairError>> {
        Box::pin(bridge::pair(host, port))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(CliCommand::Pair { host, port }) = cli.command {
        let token = nanoleaf::pair(&host, port).await?;
        // The one place the token is printed, on purpose: the user has to copy it.
        println!("{}", token.expose());
        return Ok(());
    }
    let config = Config::from_env()?;
    let filter = EnvFilter::try_new(&config.log)
        .map_err(|e| anyhow::anyhow!("invalid NANOLEAF2MQTT_LOG filter: {e}"))?;
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "nanoleaf2mqtt starting"
    );

    let store = Store::open(&config.data_dir).await?;
    let bridge = Bridge::launch(initial_setup(&store).await?);
    let state = AppState::new(
        store.clone(),
        bridge.handle().changes(),
        Arc::new(Wiring {
            bridge: bridge.handle(),
        }),
        AuthTiming::default(),
    );
    let (stop_ui, ui_stopped) = oneshot::channel::<()>();
    let mut ui = tokio::spawn(web::serve(config.http_bind, state, async {
        let _ = ui_stopped.await;
    }));

    let outcome = tokio::select! {
        signal = shutdown_signal() => signal,
        served = &mut ui => match served {
            Ok(Ok(())) => Err(anyhow::anyhow!("the configuration UI stopped")),
            Ok(Err(error)) => Err(anyhow::anyhow!(
                "the configuration UI cannot listen on {}: {error}",
                config.http_bind
            )),
            Err(error) => Err(error.into()),
        },
    };
    tracing::info!("stopping");
    let _ = stop_ui.send(());
    if !ui.is_finished() {
        let _ = ui.await;
    }
    bridge.shutdown().await;
    store.close().await;
    outcome
}

/// Broker and devices as stored. A stored broker that cannot be used (it could only have been
/// written by hand) is logged and left out, so the UI still starts to fix it.
async fn initial_setup(store: &Store) -> anyhow::Result<BridgeSetup> {
    let settings = store.settings().await?;
    let broker = settings.broker_config().unwrap_or_else(|error| {
        tracing::error!(%error, "the stored broker settings cannot be used");
        None
    });
    let devices = store
        .devices()
        .await?
        .iter()
        .filter_map(|device| device.runnable())
        .collect();
    Ok(BridgeSetup {
        broker,
        devices,
        ha_discovery: settings.ha_discovery,
        store: Some(store.clone()),
        options: BridgeOptions::default(),
    })
}

/// Waits for Ctrl-C, or SIGTERM where it exists.
async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
