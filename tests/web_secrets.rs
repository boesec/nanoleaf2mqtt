//! Secrets are write-only: no page and no answer ever contains a stored token or password.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "web_support.rs"]
mod support;

use axum::http::StatusCode;
use nanoleaf2mqtt::model::{BridgeStatus, DeviceId, DeviceStatus};
use support::{ADMIN_PASSWORD, Answer, PairAnswer, Ui};

const BROKER_PASSWORD: &str = "broker-secret-6b2f";
const DEVICE_TOKEN: &str = "device-token-0e8d";
const PAIRED_TOKEN: &str = "paired-token-c71a";
const REJECTED_PASSWORD: &str = "rejected-secret-93aa";
const REJECTED_TOKEN: &str = "rejected-token-4f10";

fn assert_clean(what: &str, answer: &Answer, secrets: &[&str]) {
    let text = answer.everything();
    for secret in secrets {
        assert!(!text.contains(secret), "{what} shows a secret:\n{text}");
    }
}

#[tokio::test]
async fn no_page_or_answer_contains_a_stored_secret() {
    let mut ui = Ui::signed_in().await;
    let hash = ui.store.admin_password_hash().await.unwrap().unwrap();
    let mut answers: Vec<(String, Answer)> = Vec::new();

    // Store every kind of secret through the UI.
    answers.push((
        "save broker".to_owned(),
        ui.post(
            "/broker",
            &[
                ("url", "mqtt://broker.test"),
                ("username", "bridge"),
                ("password", BROKER_PASSWORD),
                ("prefix", "nanoleaf"),
            ],
        )
        .await,
    ));
    answers.push((
        "add device".to_owned(),
        ui.post(
            "/devices",
            &[
                ("name", "Hall"),
                ("id", "hall"),
                ("host", "192.0.2.20"),
                ("port", "16021"),
                ("token", DEVICE_TOKEN),
            ],
        )
        .await,
    ));
    answers.push((
        "add device without token".to_owned(),
        ui.post(
            "/devices",
            &[
                ("name", "Stairs"),
                ("id", ""),
                ("host", "192.0.2.21"),
                ("port", "16021"),
                ("token", ""),
            ],
        )
        .await,
    ));
    *ui.backend.pair.lock().unwrap() = PairAnswer::Token(PAIRED_TOKEN.to_owned());
    answers.push((
        "pair".to_owned(),
        ui.post("/devices/stairs/pair", &[]).await,
    ));
    let broker = ui.store.broker_config().await.unwrap().unwrap();
    assert_eq!(broker.password.unwrap().expose(), BROKER_PASSWORD);
    let stairs = ui
        .store
        .device(&DeviceId::new("stairs").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stairs.token.unwrap().expose(), PAIRED_TOKEN);

    // Forms sent back with an error must not echo what was typed into a secret field.
    let rejected = ui
        .post(
            "/broker",
            &[
                ("url", "mqtts://broker.test"),
                ("username", "bridge"),
                ("password", REJECTED_PASSWORD),
                ("prefix", "nanoleaf"),
            ],
        )
        .await;
    assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY);
    answers.push(("rejected broker".to_owned(), rejected));
    let rejected = ui
        .post(
            "/devices/hall",
            &[
                ("name", "Hall"),
                ("host", "192.0.2.20"),
                ("port", "0"),
                ("token", REJECTED_TOKEN),
            ],
        )
        .await;
    assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY);
    answers.push(("rejected device".to_owned(), rejected));
    let rejected = ui
        .post(
            "/devices",
            &[
                ("name", "Hall"),
                ("id", "hall"),
                ("host", "192.0.2.20"),
                ("port", "16021"),
                ("token", REJECTED_TOKEN),
            ],
        )
        .await;
    assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY);
    answers.push(("duplicate device".to_owned(), rejected));
    answers.push((
        "wrong current password".to_owned(),
        ui.post(
            "/settings/password",
            &[
                ("current", REJECTED_PASSWORD),
                ("password", "whatever-1234"),
                ("confirm", "whatever-1234"),
            ],
        )
        .await,
    ));

    // The bridge reports values for both devices.
    *ui.backend.status.lock().unwrap() = BridgeStatus {
        broker_connected: true,
        devices: ["hall", "stairs"]
            .into_iter()
            .map(|id| DeviceStatus {
                id: DeviceId::new(id).unwrap(),
                reachable: Some(true),
                values: vec![("on".to_owned(), "true".to_owned())],
            })
            .collect(),
    };

    for path in [
        "/",
        "/fragments/overview",
        "/broker",
        "/devices",
        "/devices/new",
        "/devices/hall",
        "/devices/hall/remove",
        "/devices/stairs",
        "/settings",
        "/devices/suggest-id?name=Hall",
    ] {
        let answer = ui.get(path).await;
        assert_eq!(answer.status, StatusCode::OK, "{path}");
        answers.push((path.to_owned(), answer));
    }
    // A set secret shows as "set", nothing more.
    let broker_page = &answers.iter().find(|(p, _)| p == "/broker").unwrap().1;
    assert!(broker_page.body.contains("set – type to replace it"));
    let hall_page = &answers
        .iter()
        .find(|(p, _)| p == "/devices/hall")
        .unwrap()
        .1;
    assert!(hall_page.body.contains("set – type to replace it"));

    answers.push(("logout".to_owned(), ui.post("/logout", &[]).await));
    answers.push(("login page".to_owned(), ui.get("/login").await));
    answers.push(("login".to_owned(), ui.login(ADMIN_PASSWORD).await));

    let secrets = [
        BROKER_PASSWORD,
        DEVICE_TOKEN,
        PAIRED_TOKEN,
        REJECTED_PASSWORD,
        REJECTED_TOKEN,
        ADMIN_PASSWORD,
        hash.as_str(),
    ];
    for (what, answer) in &answers {
        assert_clean(what, answer, &secrets);
    }

    // The messages to the bridge carry the secrets, redacted in their debug form.
    let changes = format!("{:?}", ui.take_changes());
    assert!(changes.contains("[redacted]"), "{changes}");
    for secret in secrets {
        assert!(!changes.contains(secret), "{changes}");
    }
}
