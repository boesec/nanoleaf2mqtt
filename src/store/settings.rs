//! Broker, topic prefix and Home Assistant discovery.

use sqlx::Row;

use super::{SecretUpdate, Store, StoreError, now};
use crate::model::{BrokerConfig, parse_broker_url, process_client_id, validate_prefix};
use crate::secret::Secret;

/// The stored settings.
#[derive(Debug)]
pub struct Settings {
    /// `mqtt://host[:port]`; `None` while no broker is configured.
    pub broker_url: Option<String>,
    /// Broker user; `None` connects anonymously.
    pub broker_username: Option<String>,
    /// Password of the broker user.
    pub broker_password: Option<Secret<String>>,
    /// Topic prefix.
    pub topic_prefix: String,
    /// Whether Home Assistant MQTT discovery is wanted.
    pub ha_discovery: bool,
    /// Topic prefix Home Assistant listens on for discovery messages.
    pub ha_discovery_prefix: String,
}

impl Settings {
    /// The broker connection these settings describe, with this process's client id; `None`
    /// while no broker is configured.
    pub fn broker_config(&self) -> Result<Option<BrokerConfig>, StoreError> {
        let Some(url) = &self.broker_url else {
            return Ok(None);
        };
        let (host, port) = parse_broker_url(url)?;
        validate_prefix(&self.topic_prefix)?;
        Ok(Some(BrokerConfig {
            host,
            port,
            username: self.broker_username.clone(),
            password: self
                .broker_password
                .as_ref()
                .map(|p| Secret::new(p.expose().clone())),
            client_id: process_client_id(),
            prefix: self.topic_prefix.clone(),
        }))
    }
}

/// New broker settings.
#[derive(Debug, Default)]
pub struct BrokerUpdate {
    /// `mqtt://host[:port]`; `None` (or blank) removes the broker.
    pub url: Option<String>,
    /// Broker user; `None` (or blank) connects anonymously.
    pub username: Option<String>,
    /// The password.
    pub password: SecretUpdate,
    /// Topic prefix.
    pub prefix: String,
}

impl Store {
    /// The stored settings.
    pub async fn settings(&self) -> Result<Settings, StoreError> {
        let row = sqlx::query(
            "SELECT broker_url, broker_username, broker_password, topic_prefix, ha_discovery, \
             ha_discovery_prefix FROM settings WHERE id = 1",
        )
        .fetch_one(self.pool())
        .await?;
        let password: Option<String> = row.try_get("broker_password")?;
        Ok(Settings {
            broker_url: row.try_get("broker_url")?,
            broker_username: row.try_get("broker_username")?,
            broker_password: password.map(Secret::new),
            topic_prefix: row.try_get("topic_prefix")?,
            ha_discovery: row.try_get("ha_discovery")?,
            ha_discovery_prefix: row.try_get("ha_discovery_prefix")?,
        })
    }

    /// The configured broker connection, if any (see [`Settings::broker_config`]).
    pub async fn broker_config(&self) -> Result<Option<BrokerConfig>, StoreError> {
        self.settings().await?.broker_config()
    }

    /// Stores broker and prefix after validating them.
    pub async fn save_broker(&self, update: &BrokerUpdate) -> Result<(), StoreError> {
        let url = blank_to_none(update.url.as_deref());
        if let Some(url) = &url {
            parse_broker_url(url)?;
        }
        let prefix = update.prefix.trim();
        validate_prefix(prefix)?;
        let username = blank_to_none(update.username.as_deref());
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE settings SET broker_url = ?, broker_username = ?, topic_prefix = ?, \
             updated_at = ? WHERE id = 1",
        )
        .bind(url)
        .bind(username)
        .bind(prefix)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        match &update.password {
            SecretUpdate::Keep => {}
            SecretUpdate::Set(password) => {
                sqlx::query("UPDATE settings SET broker_password = ? WHERE id = 1")
                    .bind(password.expose().as_str())
                    .execute(&mut *tx)
                    .await?;
            }
            SecretUpdate::Clear => {
                sqlx::query("UPDATE settings SET broker_password = NULL WHERE id = 1")
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Switches Home Assistant discovery on or off.
    pub async fn set_ha_discovery(&self, on: bool) -> Result<(), StoreError> {
        sqlx::query("UPDATE settings SET ha_discovery = ?, updated_at = ? WHERE id = 1")
            .bind(on)
            .bind(now())
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

fn blank_to_none(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
