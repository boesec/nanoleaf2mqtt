//! The pair button: the service's pairing against a mock device, the token stored, never shown.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "web_support.rs"]
mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use nanoleaf2mqtt::bridge;
use nanoleaf2mqtt::model::{BridgeStatus, ConfigChange, DeviceId, PairError};
use nanoleaf2mqtt::secret::Secret;
use nanoleaf2mqtt::web::{Backend, BoxFuture};
use support::{ADMIN_PASSWORD, Ui};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NEW_TOKEN: &str = "fresh-token-from-pairing-41b7";

/// Pairs the way the service does.
struct ServicePairing;

impl Backend for ServicePairing {
    fn status(&self) -> BridgeStatus {
        BridgeStatus::default()
    }

    fn pair<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<Secret<String>, PairError>> {
        Box::pin(bridge::pair(host, port))
    }
}

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

/// A signed-in UI with the device `hall` (no token) pointing at `server`.
async fn ui_with_device(server: &MockServer) -> Ui {
    let mut ui = Ui::with_backend(Arc::new(ServicePairing)).await;
    ui.setup(ADMIN_PASSWORD).await;
    let port = server.address().port().to_string();
    ui.post(
        "/devices",
        &[
            ("name", "Hall"),
            ("id", "hall"),
            ("host", "127.0.0.1"),
            ("port", &port),
            ("token", ""),
        ],
    )
    .await
    .assert_redirect("/devices/hall?notice=added");
    ui.take_changes();
    ui
}

#[tokio::test]
async fn pairing_stores_the_token_without_showing_it() {
    let server = device(200).await;
    let mut ui = ui_with_device(&server).await;
    let page = ui.get("/devices/hall").await;
    assert!(
        page.body
            .contains("Hold the device's power button for 5–7 seconds")
    );

    let answer = ui.post("/devices/hall/pair", &[]).await;
    answer.assert_redirect("/devices/hall?notice=paired");
    assert!(!answer.everything().contains(NEW_TOKEN));
    let page = ui.get("/devices/hall?notice=paired").await;
    assert!(page.body.contains("Paired"));
    assert!(!page.everything().contains(NEW_TOKEN));

    let stored = ui
        .store
        .device(&DeviceId::new("hall").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.token.unwrap().expose(), NEW_TOKEN);
    // The bridge starts following the device with the new token.
    let changes = ui.take_changes();
    let [ConfigChange::Device { run: Some(run), .. }] = changes.as_slice() else {
        panic!("{changes:?}");
    };
    assert_eq!(run.token.expose(), NEW_TOKEN);
    server.verify().await;
}

#[tokio::test]
async fn a_device_not_in_pairing_mode_shows_the_button_hint() {
    let server = device(403).await;
    let mut ui = ui_with_device(&server).await;
    let answer = ui.post("/devices/hall/pair", &[]).await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert!(
        answer
            .body
            .contains("Hold its power button for 5–7 seconds"),
        "{}",
        answer.body
    );
    let stored = ui
        .store
        .device(&DeviceId::new("hall").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(stored.token.is_none());
    assert!(ui.take_changes().is_empty());
    server.verify().await;
}

#[tokio::test]
async fn an_unreachable_device_is_reported() {
    let server = MockServer::start().await;
    let port = server.address().port();
    let mut ui = ui_with_device(&server).await;
    drop(server);
    let answer = ui.post("/devices/hall/pair", &[]).await;
    assert_eq!(answer.status, StatusCode::BAD_GATEWAY);
    assert!(answer.body.contains("pairing failed"), "{}", answer.body);
    assert!(!answer.body.contains(&format!("{port}/api")), "no URL");
}
