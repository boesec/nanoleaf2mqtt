//! Request shape of every command and the range check, against a mock device.

// Test helpers outside `#[test]` functions panic on purpose, like the tests themselves.
#![allow(clippy::unwrap_used)]

use nanoleaf2mqtt::model::Command;
use nanoleaf2mqtt::nanoleaf::{Client, Error};
use nanoleaf2mqtt::secret::Secret;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "test-token-0123456789";
const ROOT: &str = include_str!("../fixtures/nanoleaf/root.json");
const EFFECTS_LIST: &str = include_str!("../fixtures/nanoleaf/effects-list.json");

fn api(suffix: &str) -> String {
    format!("/api/v1/{TOKEN}/{suffix}")
}

async fn device() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(api("")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ROOT, "application/json"))
        .mount(&server)
        .await;
    server
}

fn client(server: &MockServer) -> Client {
    let address = server.address();
    Client::new(
        &address.ip().to_string(),
        address.port(),
        Secret::new(TOKEN.to_owned()),
    )
    .unwrap()
}

async fn puts(server: &MockServer) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect()
}

#[tokio::test]
async fn request_shape_per_command() {
    let cases: Vec<(Command, &str, Option<Value>)> = vec![
        (
            Command::On(true),
            "state",
            Some(json!({"on": {"value": true}})),
        ),
        (
            Command::On(false),
            "state",
            Some(json!({"on": {"value": false}})),
        ),
        (
            Command::Brightness {
                value: 60,
                transition: Some(2),
            },
            "state",
            Some(json!({"brightness": {"value": 60, "duration": 2}})),
        ),
        (
            Command::Brightness {
                value: 0,
                transition: None,
            },
            "state",
            Some(json!({"brightness": {"value": 0}})),
        ),
        (
            Command::Hue(120),
            "state",
            Some(json!({"hue": {"value": 120}})),
        ),
        (
            Command::Saturation(80),
            "state",
            Some(json!({"sat": {"value": 80}})),
        ),
        (
            Command::ColorTemperature(4000),
            "state",
            Some(json!({"ct": {"value": 4000}})),
        ),
        (
            Command::Effect("Northern Lights".to_owned()),
            "effects",
            Some(json!({"select": "Northern Lights"})),
        ),
        (Command::Identify, "identify", None),
    ];
    for (command, target, body) in cases {
        let server = device().await;
        let mock = Mock::given(method("PUT")).and(path(api(target)));
        let mock = match &body {
            Some(body) => mock.and(body_json(body)),
            None => mock,
        };
        // The device answers 204 to state and effect commands and 200 to identify.
        let status = if target == "identify" { 200 } else { 204 };
        mock.respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        client(&server).send(&command).await.unwrap();
        server.verify().await;
        let puts = puts(&server).await;
        assert_eq!(puts.len(), 1, "{command:?}");
        if body.is_none() {
            assert!(puts[0].body.is_empty(), "{command:?} sends no body");
        } else {
            assert_eq!(
                puts[0]
                    .headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
                Some("application/json"),
            );
        }
    }
}

#[tokio::test]
async fn out_of_range_is_rejected_before_any_request() {
    let server = device().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let client = client(&server);
    client.snapshot().await.unwrap();
    let before = server.received_requests().await.unwrap().len();
    for command in [
        Command::Brightness {
            value: 101,
            transition: None,
        },
        Command::Hue(361),
        Command::Saturation(101),
        Command::ColorTemperature(1199),
        Command::ColorTemperature(6501),
    ] {
        let err = client.send(&command).await.unwrap_err();
        assert!(matches!(err, Error::OutOfRange(_)), "{command:?}: {err}");
    }
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    server.verify().await;
}

#[tokio::test]
async fn ranges_are_read_first_when_unknown() {
    let server = device().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let err = client(&server).send(&Command::Hue(400)).await.unwrap_err();
    assert!(matches!(err, Error::OutOfRange(_)), "{err}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    server.verify().await;
}

#[tokio::test]
async fn device_rejection_is_an_error() {
    let server = device().await;
    // The device answers 400 to an effect it cannot select (seen for `*Shuffle*`).
    Mock::given(method("PUT"))
        .and(path(api("effects")))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let err = client(&server)
        .send(&Command::Effect("*Shuffle*".to_owned()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Status {
                operation: "PUT /effects",
                status: 400
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn snapshot_and_effects_list() {
    let server = device().await;
    Mock::given(method("GET"))
        .and(path(api("effects/effectsList")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(EFFECTS_LIST, "application/json"))
        .mount(&server)
        .await;
    let client = client(&server);
    let snapshot = client.snapshot().await.unwrap();
    assert_eq!(snapshot.info.model.as_deref(), Some("NL42"));
    assert_eq!(snapshot.state.brightness, Some(30));
    let effects = client.effects_list().await.unwrap();
    assert_eq!(effects.len(), 20);
}

#[tokio::test]
async fn non_json_answer_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let err = client(&server).snapshot().await.unwrap_err();
    assert!(matches!(err, Error::UnexpectedResponse { .. }), "{err}");
}
