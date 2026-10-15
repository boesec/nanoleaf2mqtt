//! The binary's run mode, configured from the store in the data directory.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::path::Path;
use std::process::{Command, Stdio};

use nanoleaf2mqtt::model::DeviceId;
use nanoleaf2mqtt::secret::Secret;
use nanoleaf2mqtt::store::{BrokerUpdate, NewDevice, SecretUpdate, Store};
use serde_json::json;
use support::{FakeDevice, Watcher};

fn command(data_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanoleaf2mqtt"));
    command.env("NANOLEAF2MQTT_DATA_DIR", data_dir);
    command.env(
        "NANOLEAF2MQTT_HTTP_BIND",
        format!("127.0.0.1:{}", support::free_port()),
    );
    command.env("NANOLEAF2MQTT_LOG", "warn");
    command
}

#[test]
fn an_unusable_data_directory_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    let output = command(&file).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not-a-directory"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_binary_bridges_the_stored_device_and_broker() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "b1/#").await;
    let device = FakeDevice::start(support::root(false, 0, "Prism")).await;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store
        .save_broker(&BrokerUpdate {
            url: Some(format!("mqtt://127.0.0.1:{port}")),
            username: None,
            password: SecretUpdate::Keep,
            prefix: "b1".to_owned(),
        })
        .await
        .unwrap();
    store
        .insert_device(&NewDevice {
            id: DeviceId::new("dev").unwrap(),
            name: "Test".to_owned(),
            host: device.address.ip().to_string(),
            port: device.address.port(),
            token: Some(Secret::new(support::TOKEN.to_owned())),
            enabled: true,
        })
        .await
        .unwrap();
    store.close().await;

    let child = command(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = KillOnDrop(child);

    // After a connect `connected` may come before or after the device's values.
    let seen = watcher
        .until_all(&[("b1/connected", Some("2")), ("b1/status/dev/on", None)])
        .await;
    let on = seen.iter().find(|m| m.topic == "b1/status/dev/on").unwrap();
    assert_eq!(on.val(), json!(false));
    support::publish(port, "b1/set/dev/on", b"on", false).await;
    assert_eq!(
        device.next_command().await.body,
        json!({"on":{"value":true}})
    );

    // Killed without a chance to say goodbye: the last will speaks.
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    watcher.until_text("b1/connected", "0").await;
}

/// Kills the child when the test ends, also when it fails: a child left running would keep the
/// test harness's output open.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
