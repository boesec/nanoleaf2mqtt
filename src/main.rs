//! Entry point: wiring only.
#![deny(unsafe_code)]

use clap::Parser;
use nanoleaf2mqtt::config::Config;
use tracing_subscriber::EnvFilter;

/// Bridge between Nanoleaf light panels and MQTT.
///
/// Bootstrap settings come from the environment: NANOLEAF2MQTT_DATA_DIR,
/// NANOLEAF2MQTT_HTTP_BIND and NANOLEAF2MQTT_LOG.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    Cli::parse();
    let config = Config::from_env()?;
    let filter = EnvFilter::try_new(&config.log)
        .map_err(|e| anyhow::anyhow!("invalid NANOLEAF2MQTT_LOG filter: {e}"))?;
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "nanoleaf2mqtt starting"
    );
    Ok(())
}
