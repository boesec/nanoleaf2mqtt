//! `nanoleaf2mqtt reset-password`: removes the stored admin password and nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

use nanoleaf2mqtt::store::Store;

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$aGFzaC1mb3ItdGhlLXRlc3Q";

fn run(data_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nanoleaf2mqtt"))
        .arg("reset-password")
        .env("NANOLEAF2MQTT_DATA_DIR", data_dir)
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stored_password_is_removed_and_nothing_is_printed_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    assert!(store.set_initial_admin_password_hash(HASH).await.unwrap());

    let output = run(dir.path());
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "admin password removed; open the UI to set a new one"
    );
    let everything = format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(!everything.contains("argon2"), "{everything}");
    assert!(!everything.contains(HASH), "{everything}");

    assert_eq!(store.admin_password_hash().await.unwrap(), None);

    // Idempotent: a second run finds nothing and still succeeds.
    let again = run(dir.path());
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        String::from_utf8_lossy(&again.stdout).trim(),
        "no admin password is set; nothing to remove"
    );
    store.close().await;
}
