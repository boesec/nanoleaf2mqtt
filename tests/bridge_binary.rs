//! The binary's run mode, configured from the environment.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "bridge_support.rs"]
mod support;

use std::process::{Command, Stdio};

use serde_json::json;
use support::{FakeDevice, Watcher};

const VARS: [&str; 8] = [
    "NANOLEAF_HOST",
    "NANOLEAF_PORT",
    "NANOLEAF_TOKEN",
    "NANOLEAF_DEVICE_ID",
    "MQTT_URL",
    "MQTT_USER",
    "MQTT_PASSWORD",
    "NANOLEAF2MQTT_PREFIX",
];

fn command(vars: &[(&str, String)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanoleaf2mqtt"));
    for var in VARS {
        command.env_remove(var);
    }
    command.envs(vars.iter().map(|(k, v)| (*k, v.as_str())));
    command.env("NANOLEAF2MQTT_LOG", "warn");
    command
}

#[test]
fn a_missing_device_id_is_an_error_not_a_default() {
    let output = command(&[
        ("NANOLEAF_HOST", "127.0.0.1".to_owned()),
        ("NANOLEAF_TOKEN", support::TOKEN.to_owned()),
        ("MQTT_URL", "mqtt://127.0.0.1:1".to_owned()),
    ])
    .output()
    .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("NANOLEAF_DEVICE_ID is not set"), "{stderr}");
    assert!(!stderr.contains(support::TOKEN), "{stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_binary_bridges_device_and_broker() {
    let port = support::start_broker();
    let mut watcher = Watcher::start(port, "b1/#").await;
    let device = FakeDevice::start(support::root(false, 0, "Prism")).await;
    let child = command(&[
        ("NANOLEAF_HOST", device.address.ip().to_string()),
        ("NANOLEAF_PORT", device.address.port().to_string()),
        ("NANOLEAF_TOKEN", support::TOKEN.to_owned()),
        ("NANOLEAF_DEVICE_ID", "dev".to_owned()),
        ("MQTT_URL", format!("mqtt://127.0.0.1:{port}")),
        ("NANOLEAF2MQTT_PREFIX", "b1".to_owned()),
    ])
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
