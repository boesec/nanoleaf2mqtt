//! Entry point: wiring only.
#![deny(unsafe_code)]

use clap::{Parser, Subcommand};
use nanoleaf2mqtt::bridge::{self, Bridge, BridgeOptions};
use nanoleaf2mqtt::config::Config;
use nanoleaf2mqtt::nanoleaf;
use tracing_subscriber::EnvFilter;

/// Bridge between Nanoleaf light panels and MQTT.
///
/// Without a subcommand the bridge runs until Ctrl-C or SIGTERM.
///
/// Bootstrap settings come from the environment: NANOLEAF2MQTT_DATA_DIR,
/// NANOLEAF2MQTT_HTTP_BIND and NANOLEAF2MQTT_LOG. Broker and device for now as well:
/// NANOLEAF_HOST, NANOLEAF_PORT, NANOLEAF_TOKEN, NANOLEAF_DEVICE_ID, MQTT_URL, MQTT_USER,
/// MQTT_PASSWORD and NANOLEAF2MQTT_PREFIX.
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
    // Until the configuration store exists, broker and device come from the environment.
    let (broker, devices) = bridge::env::from_env()?;
    let bridge = Bridge::start(&broker, &devices, BridgeOptions::default())?;
    shutdown_signal().await?;
    tracing::info!("stopping");
    bridge.shutdown().await;
    Ok(())
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
