//! The configuration store: schema, CRUD, secrets, file permissions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nanoleaf2mqtt::model::{ConfigChange, DeviceId};
use nanoleaf2mqtt::secret::Secret;
use nanoleaf2mqtt::store::{
    BrokerUpdate, DB_FILE_NAME, DeviceUpdate, LastChangeRecord, NewDevice, SecretUpdate, Store,
    StoreError,
};

const TOKEN: &str = "store-test-token-7f3a";
const PASSWORD: &str = "store-test-password-91c2";

fn id(raw: &str) -> DeviceId {
    DeviceId::new(raw).unwrap()
}

fn new_device(raw: &str, token: Option<&str>) -> NewDevice {
    NewDevice {
        id: id(raw),
        name: format!("Panels {raw}"),
        host: "192.0.2.10".to_owned(),
        port: 16021,
        token: token.map(|t| Secret::new(t.to_owned())),
        enabled: true,
    }
}

async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("data")).await.unwrap();
    (dir, store)
}

/// A store as the first schema version left it, with discovery off, a prefix and a device: it
/// opens with the later migrations applied, its data kept and discovery switched on.
#[tokio::test]
async fn a_store_of_the_first_schema_version_keeps_working() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // Only the first migration, applied the way the store applies migrations.
    let first = dir.path().join("first");
    std::fs::create_dir_all(&first).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    std::fs::copy(
        source.join("0001_initial.sql"),
        first.join("0001_initial.sql"),
    )
    .unwrap();
    let options = SqliteConnectOptions::new()
        .filename(data.join(DB_FILE_NAME))
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(first.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE settings SET topic_prefix = 'lights', ha_discovery = 0")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO devices (id, name, host, port, token, enabled, created_at, updated_at) \
         VALUES ('hall', 'Hall', '192.0.2.10', 16021, NULL, 1, 'x', 'x')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = Store::open(&data).await.unwrap();
    let settings = store.settings().await.unwrap();
    assert!(settings.ha_discovery, "switched on by the second migration");
    assert_eq!(settings.topic_prefix, "lights");
    let devices = store.devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, id("hall"));
    // Switching it off sticks: the migration runs once.
    store.set_ha_discovery(false).await.unwrap();
    store.close().await;
    let store = Store::open(&data).await.unwrap();
    assert!(!store.settings().await.unwrap().ha_discovery);
}

#[tokio::test]
async fn migrations_apply_on_an_empty_directory_with_defaults() {
    let (_dir, store) = open().await;
    assert!(store.path().ends_with(DB_FILE_NAME));
    let settings = store.settings().await.unwrap();
    assert_eq!(settings.broker_url, None);
    assert_eq!(settings.broker_username, None);
    assert!(settings.broker_password.is_none());
    assert_eq!(settings.topic_prefix, "nanoleaf");
    assert!(
        settings.ha_discovery,
        "Home Assistant discovery is on by default"
    );
    assert_eq!(settings.ha_discovery_prefix, "homeassistant");
    assert!(
        settings.broker_config().unwrap().is_none(),
        "no broker invented"
    );
    assert!(store.devices().await.unwrap().is_empty());
    assert_eq!(store.admin_password_hash().await.unwrap(), None);
    assert!(store.last_changes().await.unwrap().is_empty());
}

#[tokio::test]
async fn reopening_keeps_everything_and_migrates_nothing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store
        .insert_device(&new_device("keller", Some(TOKEN)))
        .await
        .unwrap();
    store.close().await;
    let store = Store::open(dir.path()).await.unwrap();
    let devices = store.devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].token.as_ref().unwrap().expose(), TOKEN);
}

#[tokio::test]
async fn broker_settings_round_trip_and_password_is_write_only() {
    let (_dir, store) = open().await;
    store
        .save_broker(&BrokerUpdate {
            url: Some(" mqtt://broker.test:1884 ".to_owned()),
            username: Some("bridge".to_owned()),
            password: SecretUpdate::Set(Secret::new(PASSWORD.to_owned())),
            prefix: "home/nanoleaf".to_owned(),
        })
        .await
        .unwrap();
    let broker = store.broker_config().await.unwrap().unwrap();
    assert_eq!(broker.host, "broker.test");
    assert_eq!(broker.port, 1884);
    assert_eq!(broker.username.as_deref(), Some("bridge"));
    assert_eq!(broker.password.as_ref().unwrap().expose(), PASSWORD);
    assert_eq!(broker.prefix, "home/nanoleaf");
    assert!(broker.client_id.starts_with("nanoleaf2mqtt-"));

    // An empty password field keeps the stored password.
    store
        .save_broker(&BrokerUpdate {
            url: Some("mqtt://other.test".to_owned()),
            username: Some("bridge".to_owned()),
            password: SecretUpdate::Keep,
            prefix: "nanoleaf".to_owned(),
        })
        .await
        .unwrap();
    let settings = store.settings().await.unwrap();
    assert_eq!(
        settings.broker_password.as_ref().unwrap().expose(),
        PASSWORD
    );
    let debug = format!("{settings:?} {:?}", settings.broker_config().unwrap());
    assert!(!debug.contains(PASSWORD), "{debug}");

    // Removing the URL removes the broker; clearing removes the password.
    store
        .save_broker(&BrokerUpdate {
            url: Some("  ".to_owned()),
            username: None,
            password: SecretUpdate::Clear,
            prefix: "nanoleaf".to_owned(),
        })
        .await
        .unwrap();
    let settings = store.settings().await.unwrap();
    assert!(settings.broker_url.is_none() && settings.broker_password.is_none());
    assert!(store.broker_config().await.unwrap().is_none());
}

#[tokio::test]
async fn invalid_broker_settings_are_rejected_and_nothing_is_stored() {
    let (_dir, store) = open().await;
    for (url, prefix) in [
        ("mqtts://broker", "nanoleaf"),
        ("http://broker", "nanoleaf"),
        ("mqtt://user:secret@broker", "nanoleaf"),
        ("mqtt://broker", "nano/#"),
        ("mqtt://broker", ""),
    ] {
        let err = store
            .save_broker(&BrokerUpdate {
                url: Some(url.to_owned()),
                username: None,
                password: SecretUpdate::Keep,
                prefix: prefix.to_owned(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Invalid(_)),
            "{url} {prefix}: {err}"
        );
        assert!(!err.to_string().contains("secret"), "{err}");
    }
    assert!(store.settings().await.unwrap().broker_url.is_none());
}

#[tokio::test]
async fn ha_discovery_switch_is_stored() {
    let (_dir, store) = open().await;
    store.set_ha_discovery(true).await.unwrap();
    assert!(store.settings().await.unwrap().ha_discovery);
    store.set_ha_discovery(false).await.unwrap();
    assert!(!store.settings().await.unwrap().ha_discovery);
}

#[tokio::test]
async fn device_crud() {
    let (_dir, store) = open().await;
    store
        .insert_device(&new_device("b", Some(TOKEN)))
        .await
        .unwrap();
    store.insert_device(&new_device("a", None)).await.unwrap();
    let err = store
        .insert_device(&new_device("a", None))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::DuplicateDevice(_)), "{err}");

    let devices = store.devices().await.unwrap();
    let ids: Vec<&str> = devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    // Without a token a device cannot run; the bridge is told to leave it alone.
    assert!(devices[0].runnable().is_none());
    assert!(matches!(
        devices[0].change(),
        ConfigChange::Device { run: None, .. }
    ));
    let run = devices[1].runnable().unwrap();
    assert_eq!(run.token.expose(), TOKEN);
    assert_eq!((run.host.as_str(), run.port), ("192.0.2.10", 16021));

    // Update: the empty token field keeps the token.
    assert!(
        store
            .update_device(
                &id("b"),
                &DeviceUpdate {
                    name: "Hall".to_owned(),
                    host: "panels.lan".to_owned(),
                    port: 16022,
                    token: SecretUpdate::Keep,
                },
            )
            .await
            .unwrap()
    );
    let b = store.device(&id("b")).await.unwrap().unwrap();
    assert_eq!(
        (b.name.as_str(), b.host.as_str(), b.port),
        ("Hall", "panels.lan", 16022)
    );
    assert_eq!(b.token.as_ref().unwrap().expose(), TOKEN);

    // Token set by pairing, then disabled: not runnable any more.
    assert!(
        store
            .set_device_token(&id("a"), &Secret::new("paired-token".to_owned()))
            .await
            .unwrap()
    );
    assert!(
        store
            .device(&id("a"))
            .await
            .unwrap()
            .unwrap()
            .runnable()
            .is_some()
    );
    assert!(store.set_device_enabled(&id("a"), false).await.unwrap());
    let a = store.device(&id("a")).await.unwrap().unwrap();
    assert!(!a.enabled && a.runnable().is_none());

    // Unknown devices.
    assert!(!store.set_device_enabled(&id("zz"), true).await.unwrap());
    assert!(store.device(&id("zz")).await.unwrap().is_none());

    assert!(store.delete_device(&id("a")).await.unwrap());
    assert!(!store.delete_device(&id("a")).await.unwrap());
    assert_eq!(store.devices().await.unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_device_values_are_rejected() {
    let (_dir, store) = open().await;
    let cases: Vec<NewDevice> = vec![
        NewDevice {
            name: " ".to_owned(),
            ..new_device("a", None)
        },
        NewDevice {
            host: "".to_owned(),
            ..new_device("a", None)
        },
        NewDevice {
            host: "evil.test/path?".to_owned(),
            ..new_device("a", None)
        },
        NewDevice {
            port: 0,
            ..new_device("a", None)
        },
        NewDevice {
            token: Some(Secret::new("with space".to_owned())),
            ..new_device("a", None)
        },
        NewDevice {
            token: Some(Secret::new("a/b".to_owned())),
            ..new_device("a", None)
        },
    ];
    for device in cases {
        let err = store.insert_device(&device).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidValue(_)), "{err}");
    }
    assert!(store.devices().await.unwrap().is_empty());
}

#[tokio::test]
async fn device_token_never_appears_in_debug() {
    let (_dir, store) = open().await;
    store
        .insert_device(&new_device("a", Some(TOKEN)))
        .await
        .unwrap();
    let device = store.device(&id("a")).await.unwrap().unwrap();
    let debug = format!("{device:?} {:?} {:?}", device.runnable(), device.change());
    assert!(!debug.contains(TOKEN), "{debug}");
    assert!(debug.contains("[redacted]"), "{debug}");
}

#[tokio::test]
async fn the_first_admin_password_cannot_be_replaced_by_the_first_start() {
    let (_dir, store) = open().await;
    assert!(store.replace_admin_password_hash("x").await.is_err());
    assert!(
        store
            .set_initial_admin_password_hash("hash-1")
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_initial_admin_password_hash("hash-2")
            .await
            .unwrap()
    );
    assert_eq!(
        store.admin_password_hash().await.unwrap().as_deref(),
        Some("hash-1")
    );
    store.replace_admin_password_hash("hash-3").await.unwrap();
    assert_eq!(
        store.admin_password_hash().await.unwrap().as_deref(),
        Some("hash-3")
    );
}

#[tokio::test]
async fn last_changes_are_stored_per_device_and_go_with_it() {
    let (_dir, store) = open().await;
    store
        .insert_device(&new_device("a", Some(TOKEN)))
        .await
        .unwrap();
    store
        .insert_device(&new_device("b", Some(TOKEN)))
        .await
        .unwrap();
    let record = |device: &str, path: &str, val: &str, lc: i64| LastChangeRecord {
        device: id(device),
        path: path.to_owned(),
        val: val.to_owned(),
        lc,
    };
    store
        .save_last_changes(&[
            record("a", "on", "true", 1),
            record("a", "brightness", "30", 2),
            record("b", "on", "false", 3),
            // Not stored: a device the store does not know.
            record("ghost", "on", "true", 4),
        ])
        .await
        .unwrap();
    store
        .save_last_changes(&[record("a", "brightness", "40", 5)])
        .await
        .unwrap();
    assert_eq!(
        store.last_changes_of(&id("a")).await.unwrap(),
        vec![
            record("a", "brightness", "40", 5),
            record("a", "on", "true", 1)
        ]
    );
    assert_eq!(store.last_changes().await.unwrap().len(), 3);
    store.delete_device(&id("a")).await.unwrap();
    assert_eq!(
        store.last_changes().await.unwrap(),
        vec![record("b", "on", "false", 3)]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn database_file_is_private_to_the_service_user() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let store = Store::open(&data).await.unwrap();
    store
        .insert_device(&new_device("a", Some(TOKEN)))
        .await
        .unwrap();
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(store.path()), 0o600);
    for entry in std::fs::read_dir(&data).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(
            mode(&path) & 0o077,
            0,
            "{} is readable by others",
            path.display()
        );
    }
    store.close().await;

    // An existing file with wider permissions is tightened.
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let store = Store::open(&data).await.unwrap();
    assert_eq!(mode(store.path()), 0o600);
}
