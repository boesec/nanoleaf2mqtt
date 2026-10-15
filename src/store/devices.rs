//! The configured devices.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{SecretUpdate, Store, StoreError, now};
use crate::model::{ConfigChange, DeviceConfig, DeviceId};
use crate::secret::Secret;

/// Longest accepted display name or host.
const MAX_TEXT_LEN: usize = 255;

/// A device as stored.
#[derive(Debug)]
pub struct StoredDevice {
    /// Id below the topic prefix; never changes.
    pub id: DeviceId,
    /// Display name.
    pub name: String,
    /// Address of the device.
    pub host: String,
    /// Port of the device's OpenAPI.
    pub port: u16,
    /// Auth token; `None` until paired or entered.
    pub token: Option<Secret<String>>,
    /// Whether the bridge follows the device.
    pub enabled: bool,
}

impl StoredDevice {
    /// The configuration to run the device with: `None` while it is disabled or has no token.
    pub fn runnable(&self) -> Option<DeviceConfig> {
        if !self.enabled {
            return None;
        }
        let token = self.token.as_ref()?;
        Some(DeviceConfig {
            id: self.id.clone(),
            host: self.host.clone(),
            port: self.port,
            token: Secret::new(token.expose().clone()),
        })
    }

    /// The message that makes the running bridge follow this device as stored.
    pub fn change(&self) -> ConfigChange {
        ConfigChange::Device {
            id: self.id.clone(),
            run: self.runnable(),
        }
    }
}

/// A device to add.
#[derive(Debug)]
pub struct NewDevice {
    /// Id below the topic prefix.
    pub id: DeviceId,
    /// Display name.
    pub name: String,
    /// Address of the device.
    pub host: String,
    /// Port of the device's OpenAPI.
    pub port: u16,
    /// Auth token, if already known.
    pub token: Option<Secret<String>>,
    /// Whether the bridge follows the device.
    pub enabled: bool,
}

/// Changed settings of a device. The id cannot change: it is the device's topic level.
#[derive(Debug)]
pub struct DeviceUpdate {
    /// Display name.
    pub name: String,
    /// Address of the device.
    pub host: String,
    /// Port of the device's OpenAPI.
    pub port: u16,
    /// The token.
    pub token: SecretUpdate,
}

impl Store {
    /// Every device, in id order.
    pub async fn devices(&self) -> Result<Vec<StoredDevice>, StoreError> {
        let rows =
            sqlx::query("SELECT id, name, host, port, token, enabled FROM devices ORDER BY id")
                .fetch_all(self.pool())
                .await?;
        rows.iter().map(device_from_row).collect()
    }

    /// One device.
    pub async fn device(&self, id: &DeviceId) -> Result<Option<StoredDevice>, StoreError> {
        let row =
            sqlx::query("SELECT id, name, host, port, token, enabled FROM devices WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(self.pool())
                .await?;
        row.as_ref().map(device_from_row).transpose()
    }

    /// Adds a device.
    pub async fn insert_device(&self, device: &NewDevice) -> Result<(), StoreError> {
        let name = checked_text(&device.name, "the name must not be empty")?;
        let host = checked_host(&device.host)?;
        checked_port(device.port)?;
        let token = checked_token(device.token.as_ref())?;
        let stamp = now();
        let result = sqlx::query(
            "INSERT INTO devices (id, name, host, port, token, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(device.id.as_str())
        .bind(name)
        .bind(host)
        .bind(device.port)
        .bind(token)
        .bind(device.enabled)
        .bind(&stamp)
        .bind(&stamp)
        .execute(self.pool())
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(StoreError::DuplicateDevice(device.id.clone()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Changes a device. Returns `false` when it does not exist.
    pub async fn update_device(
        &self,
        id: &DeviceId,
        update: &DeviceUpdate,
    ) -> Result<bool, StoreError> {
        let name = checked_text(&update.name, "the name must not be empty")?;
        let host = checked_host(&update.host)?;
        checked_port(update.port)?;
        let mut tx = self.pool().begin().await?;
        let done = sqlx::query(
            "UPDATE devices SET name = ?, host = ?, port = ?, updated_at = ? WHERE id = ?",
        )
        .bind(name)
        .bind(host)
        .bind(update.port)
        .bind(now())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 0 {
            return Ok(false);
        }
        match &update.token {
            SecretUpdate::Keep => {}
            SecretUpdate::Set(token) => {
                let token = checked_token(Some(token))?;
                sqlx::query("UPDATE devices SET token = ? WHERE id = ?")
                    .bind(token)
                    .bind(id.as_str())
                    .execute(&mut *tx)
                    .await?;
            }
            SecretUpdate::Clear => {
                sqlx::query("UPDATE devices SET token = NULL WHERE id = ?")
                    .bind(id.as_str())
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Stores a new token for a device (after pairing). Returns `false` when it does not exist.
    pub async fn set_device_token(
        &self,
        id: &DeviceId,
        token: &Secret<String>,
    ) -> Result<bool, StoreError> {
        let token = checked_token(Some(token))?;
        let done = sqlx::query("UPDATE devices SET token = ?, updated_at = ? WHERE id = ?")
            .bind(token)
            .bind(now())
            .bind(id.as_str())
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Enables or disables a device. Returns `false` when it does not exist.
    pub async fn set_device_enabled(
        &self,
        id: &DeviceId,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query("UPDATE devices SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(enabled)
            .bind(now())
            .bind(id.as_str())
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Removes a device and its persisted `lc` values. Returns `false` when it does not exist.
    pub async fn delete_device(&self, id: &DeviceId) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM devices WHERE id = ?")
            .bind(id.as_str())
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() == 1)
    }
}

fn device_from_row(row: &SqliteRow) -> Result<StoredDevice, StoreError> {
    let invalid = |reason: String| StoreError::InvalidRow {
        table: "devices",
        reason,
    };
    let id: String = row.try_get("id")?;
    let port: i64 = row.try_get("port")?;
    let token: Option<String> = row.try_get("token")?;
    Ok(StoredDevice {
        id: DeviceId::new(id).map_err(|e| invalid(e.to_string()))?,
        name: row.try_get("name")?,
        host: row.try_get("host")?,
        port: u16::try_from(port).map_err(|_| invalid(format!("port {port}")))?,
        token: token.map(Secret::new),
        enabled: row.try_get("enabled")?,
    })
}

fn checked_text<'a>(value: &'a str, empty: &'static str) -> Result<&'a str, StoreError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(StoreError::InvalidValue(empty));
    }
    if value.len() > MAX_TEXT_LEN || value.chars().any(char::is_control) {
        return Err(StoreError::InvalidValue(
            "names and hosts are at most 255 characters without control characters",
        ));
    }
    Ok(value)
}

/// A host name, IPv4 address or IPv6 address (with or without brackets): it becomes part of
/// every request URL, so nothing that could change the URL's structure is accepted.
fn checked_host(host: &str) -> Result<&str, StoreError> {
    let host = checked_text(host, "the host must not be empty")?;
    let valid = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    if valid {
        Ok(host)
    } else {
        Err(StoreError::InvalidValue(
            "the host may contain letters, digits, '.', '-' and ':' only",
        ))
    }
}

fn checked_port(port: u16) -> Result<(), StoreError> {
    if port == 0 {
        return Err(StoreError::InvalidValue("the port must be 1-65535"));
    }
    Ok(())
}

fn checked_token(token: Option<&Secret<String>>) -> Result<Option<&str>, StoreError> {
    match token.map(|t| t.expose().trim()) {
        None => Ok(None),
        Some("") => Err(StoreError::InvalidValue("the token must not be empty")),
        // The token is part of every request path: anything outside printable ASCII would
        // break the requests, and `/` would change their target.
        Some(t) if !t.bytes().all(|b| b.is_ascii_graphic() && b != b'/') => Err(
            StoreError::InvalidValue("the token may contain printable ASCII characters only"),
        ),
        Some(t) => Ok(Some(t)),
    }
}
