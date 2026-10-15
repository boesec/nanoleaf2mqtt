//! Integration tests of the binary.

use std::process::Command;

#[test]
fn cli_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_nanoleaf2mqtt"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "nanoleaf2mqtt 0.1.0"
    );
}
