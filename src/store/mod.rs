//! SQLite configuration store: broker, topic prefix, Home Assistant switch, devices, the admin
//! password hash and the persisted `lc` values.
//!
//! The database lives in `<data dir>/nanoleaf2mqtt.db`. On Unix the data directory is created
//! with mode 0700 and the file with mode 0600: secrets (broker password, device tokens) are stored
//! as plain text, protected by the file permissions. In memory they are [`Secret`]s; nothing in
//! this module logs or formats them.

mod devices;
mod settings;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

use crate::model::{ConfigError, DeviceId};
use crate::secret::Secret;

pub use devices::{DeviceUpdate, NewDevice, StoredDevice};
pub use settings::{BrokerUpdate, Settings};

/// Name of the database file inside the data directory.
pub const DB_FILE_NAME: &str = "nanoleaf2mqtt.db";

/// Error of the configuration store. Never contains a secret.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The data directory or the database file cannot be prepared.
    #[error("cannot prepare {path}: {source}")]
    Io {
        /// The directory or file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
    /// The database reported an error.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The schema could not be brought up to date.
    #[error("database migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A value to be stored is invalid.
    #[error(transparent)]
    Invalid(#[from] ConfigError),
    /// A value to be stored is invalid.
    #[error("{0}")]
    InvalidValue(&'static str),
    /// A device with this id exists already.
    #[error("a device with the id `{0}` exists already")]
    DuplicateDevice(DeviceId),
    /// A stored row cannot be read back.
    #[error("stored {table} row is invalid: {reason}")]
    InvalidRow {
        /// The table.
        table: &'static str,
        /// What is wrong.
        reason: String,
    },
}

/// How a stored secret changes on save. A form field left empty keeps the stored value: the UI
/// never shows a secret, so it cannot send it back.
#[derive(Debug, Default)]
pub enum SecretUpdate {
    /// Keep the stored value.
    #[default]
    Keep,
    /// Replace it.
    Set(Secret<String>),
    /// Remove it.
    Clear,
}

/// The last change of one status value of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastChangeRecord {
    /// The device.
    pub device: DeviceId,
    /// Status path below the device (`on`, `brightness`, ...).
    pub path: String,
    /// The value as JSON text.
    pub val: String,
    /// When it last changed, ms since the epoch.
    pub lc: i64,
}

/// Handle to the configuration store. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    path: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish()
    }
}

impl Store {
    /// Opens (or creates) the store in `data_dir` and applies pending migrations.
    pub async fn open(data_dir: &Path) -> Result<Self, StoreError> {
        let path = prepare_files(data_dir)?;
        let options = SqliteConnectOptions::from_str("sqlite://")?
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        tracing::info!(path = %path.display(), "configuration store ready");
        Ok(Self { pool, path })
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Closes the connections; pending writes are flushed.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // ----- admin password ---------------------------------------------------------------

    /// The stored admin password hash (PHC string), `None` before the first start is completed.
    pub async fn admin_password_hash(&self) -> Result<Option<String>, StoreError> {
        let row = sqlx::query("SELECT password_hash FROM admin WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.try_get("password_hash")).transpose()?)
    }

    /// Stores the first admin password hash. Returns `false`, changing nothing, when one is set
    /// already: the first-start page must never be able to replace an existing password.
    pub async fn set_initial_admin_password_hash(&self, hash: &str) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "INSERT INTO admin (id, password_hash, updated_at) VALUES (1, ?, ?) \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(hash)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Replaces the admin password hash (password change).
    pub async fn replace_admin_password_hash(&self, hash: &str) -> Result<(), StoreError> {
        let done = sqlx::query("UPDATE admin SET password_hash = ?, updated_at = ? WHERE id = 1")
            .bind(hash)
            .bind(now())
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 1 {
            Ok(())
        } else {
            Err(StoreError::InvalidValue("no admin password is set yet"))
        }
    }

    /// Removes the stored admin password, so that the first-start page can set a new one.
    /// Returns whether there was one. Sessions of a running service end at its restart.
    pub async fn clear_admin_password(&self) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM admin WHERE id = 1")
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() == 1)
    }

    // ----- last change ------------------------------------------------------------------

    /// Every persisted `lc`.
    pub async fn last_changes(&self) -> Result<Vec<LastChangeRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT device_id, path, val, lc FROM last_change ORDER BY device_id, path",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(last_change_from_row).collect()
    }

    /// The persisted `lc` values of one device.
    pub async fn last_changes_of(
        &self,
        device: &DeviceId,
    ) -> Result<Vec<LastChangeRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT device_id, path, val, lc FROM last_change WHERE device_id = ? ORDER BY path",
        )
        .bind(device.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(last_change_from_row).collect()
    }

    /// Writes a batch of `lc` values in one transaction. Records of a device that is not (or no
    /// longer) stored are skipped.
    pub async fn save_last_changes(&self, records: &[LastChangeRecord]) -> Result<(), StoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for record in records {
            sqlx::query(
                "INSERT INTO last_change (device_id, path, val, lc) \
                 SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM devices WHERE id = ?1) \
                 ON CONFLICT(device_id, path) DO UPDATE SET val = excluded.val, lc = excluded.lc",
            )
            .bind(record.device.as_str())
            .bind(&record.path)
            .bind(&record.val)
            .bind(record.lc)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

fn last_change_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<LastChangeRecord, StoreError> {
    let id: String = row.try_get("device_id")?;
    Ok(LastChangeRecord {
        device: DeviceId::new(id).map_err(|e| StoreError::InvalidRow {
            table: "last_change",
            reason: e.to_string(),
        })?,
        path: row.try_get("path")?,
        val: row.try_get("val")?,
        lc: row.try_get("lc")?,
    })
}

/// Now as stored in `created_at` / `updated_at`.
pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Creates the data directory (0700) and the database file (0600) before SQLite opens it, so the
/// file never exists with wider permissions, and tightens both if they exist already.
fn prepare_files(data_dir: &Path) -> Result<PathBuf, StoreError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| StoreError::Io { path, source }
    };
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dir.mode(0o700);
    }
    dir.create(data_dir).map_err(io(data_dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))
        {
            tracing::warn!(path = %data_dir.display(), %error, "cannot restrict the data directory to its owner");
        }
    }

    let path = data_dir.join(DB_FILE_NAME);
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    file.open(&path).map_err(io(&path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(io(&path))?;
    }
    Ok(path)
}
