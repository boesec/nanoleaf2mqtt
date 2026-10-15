//! Pairing: library call and the `pair` subcommand against a mock device.

// Test helpers outside `#[test]` functions panic on purpose, like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::process::{Command, Output};

use nanoleaf2mqtt::nanoleaf::{self, Error};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NEW_TOKEN: &str = "fresh-token-abcdef0123456789";

async fn device(status: u16) -> MockServer {
    let server = MockServer::start().await;
    let response = if status == 200 {
        ResponseTemplate::new(200).set_body_raw(
            format!(r#"{{"auth_token":"{NEW_TOKEN}"}}"#),
            "application/json",
        )
    } else {
        ResponseTemplate::new(status)
    };
    Mock::given(method("POST"))
        .and(path("/api/v1/new"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    server
}

async fn run_pair(server: &MockServer) -> Output {
    let port = server.address().port().to_string();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_nanoleaf2mqtt"))
            .args(["pair", "--host", "127.0.0.1", "--port", &port])
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn pair_returns_token() {
    let server = device(200).await;
    let token = nanoleaf::pair("127.0.0.1", server.address().port())
        .await
        .unwrap();
    assert_eq!(token.expose(), NEW_TOKEN);
    server.verify().await;
}

#[tokio::test]
async fn pair_refused_gives_button_hint() {
    let server = device(403).await;
    let err = nanoleaf::pair("127.0.0.1", server.address().port())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::PairingRefused), "{err}");
    assert!(err.to_string().contains("hold the power button"), "{err}");
}

#[tokio::test]
async fn pair_without_token_in_answer_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&server)
        .await;
    let err = nanoleaf::pair("127.0.0.1", server.address().port())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnexpectedResponse { .. }), "{err}");
}

#[tokio::test]
async fn cli_pair_prints_token_once() {
    let server = device(200).await;
    let output = run_pair(&server).await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{NEW_TOKEN}\n")
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(NEW_TOKEN));
    server.verify().await;
}

#[tokio::test]
async fn cli_pair_refused_prints_hint() {
    let server = device(403).await;
    let output = run_pair(&server).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("hold the power button for 5-7 seconds"),
        "{stderr}"
    );
    server.verify().await;
}
