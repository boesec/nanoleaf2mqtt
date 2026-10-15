//! Editing the configuration through the UI: the store is updated and the bridge is told.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "web_support.rs"]
mod support;

use axum::http::StatusCode;
use nanoleaf2mqtt::model::{BridgeStatus, ConfigChange, DeviceId, DeviceStatus};
use support::Ui;

fn id(raw: &str) -> DeviceId {
    DeviceId::new(raw).unwrap()
}

async fn add_device(ui: &mut Ui, name: &str, device_id: &str, token: &str) -> support::Answer {
    ui.post(
        "/devices",
        &[
            ("name", name),
            ("id", device_id),
            ("host", "192.0.2.30"),
            ("port", "16021"),
            ("token", token),
        ],
    )
    .await
}

#[tokio::test]
async fn saving_the_broker_tells_the_bridge() {
    let mut ui = Ui::signed_in().await;
    let page = ui.get("/broker").await;
    assert!(
        page.body.contains("value=\"nanoleaf\""),
        "default prefix shown"
    );
    ui.post(
        "/broker",
        &[
            ("url", "mqtt://broker.test:1884"),
            ("username", "bridge"),
            ("password", "pw-1"),
            ("prefix", "home/nl"),
        ],
    )
    .await
    .assert_redirect("/broker?notice=saved");
    let changes = ui.take_changes();
    let [ConfigChange::Broker(Some(broker))] = changes.as_slice() else {
        panic!("{changes:?}");
    };
    assert_eq!((broker.host.as_str(), broker.port), ("broker.test", 1884));
    assert_eq!(broker.prefix, "home/nl");
    assert_eq!(broker.password.as_ref().unwrap().expose(), "pw-1");
    let page = ui.get("/broker?notice=saved").await;
    assert!(page.body.contains("Saved."));
    assert!(page.body.contains("value=\"mqtt://broker.test:1884\""));

    // Remove the password, then the broker.
    ui.post(
        "/broker",
        &[
            ("url", "mqtt://broker.test:1884"),
            ("username", "bridge"),
            ("password", ""),
            ("clear_password", "true"),
            ("prefix", "home/nl"),
        ],
    )
    .await
    .assert_redirect("/broker?notice=saved");
    ui.post(
        "/broker",
        &[
            ("url", ""),
            ("username", ""),
            ("password", ""),
            ("prefix", "home/nl"),
        ],
    )
    .await
    .assert_redirect("/broker?notice=saved");
    let changes = ui.take_changes();
    assert!(matches!(&changes[0], ConfigChange::Broker(Some(b)) if b.password.is_none()));
    assert!(matches!(&changes[1], ConfigChange::Broker(None)));

    // Invalid settings: the page again, with the error, nothing stored, nothing sent.
    let answer = ui
        .post(
            "/broker",
            &[
                ("url", "mqtt://b"),
                ("username", ""),
                ("password", ""),
                ("prefix", "a/#"),
            ],
        )
        .await;
    assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(answer.body.contains("topic prefix"), "{}", answer.body);
    assert!(answer.body.contains("value=\"a/#\""), "the input is kept");
    assert!(ui.take_changes().is_empty());
    assert_eq!(ui.store.settings().await.unwrap().topic_prefix, "home/nl");
}

#[tokio::test]
async fn devices_are_added_edited_switched_and_removed() {
    let mut ui = Ui::signed_in().await;
    assert!(
        ui.get("/devices/new")
            .await
            .body
            .contains("value=\"16021\"")
    );
    assert_eq!(
        ui.get("/devices/suggest-id?name=Küche%20Wand").await.body,
        "kueche-wand"
    );

    // Added with an id derived from the name, with a token: the bridge starts it.
    add_device(&mut ui, "Küche Wand", "", "tok-1")
        .await
        .assert_redirect("/devices/kueche-wand?notice=added");
    let changes = ui.take_changes();
    let [
        ConfigChange::Device {
            id: changed,
            run: Some(run),
        },
    ] = changes.as_slice()
    else {
        panic!("{changes:?}");
    };
    assert_eq!(changed, &id("kueche-wand"));
    assert_eq!(run.token.expose(), "tok-1");
    let page = ui.get("/devices/kueche-wand?notice=added").await;
    assert!(page.body.contains("Device added."));
    assert!(page.body.contains("Küche Wand"));

    // Same id twice, a bad id, a bad port: refused.
    for (name, device_id, expected) in [
        ("Other", "kueche-wand", "exists already"),
        ("Other", "Bad Id", "is invalid"),
        ("!!!", "", "Enter an id"),
    ] {
        let answer = add_device(&mut ui, name, device_id, "").await;
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            answer.body.contains(expected),
            "{expected}: {}",
            answer.body
        );
    }
    assert!(ui.take_changes().is_empty());

    // Edited: the empty token field keeps the token.
    ui.post(
        "/devices/kueche-wand",
        &[
            ("name", "Kitchen"),
            ("host", "192.0.2.31"),
            ("port", "16022"),
            ("token", ""),
        ],
    )
    .await
    .assert_redirect("/devices/kueche-wand?notice=saved");
    let changes = ui.take_changes();
    let [ConfigChange::Device { run: Some(run), .. }] = changes.as_slice() else {
        panic!("{changes:?}");
    };
    assert_eq!((run.host.as_str(), run.port), ("192.0.2.31", 16022));
    assert_eq!(run.token.expose(), "tok-1");

    // Disabled: the bridge stops it; enabled: starts it again.
    ui.post("/devices/kueche-wand/enabled", &[("enabled", "false")])
        .await
        .assert_redirect("/devices/kueche-wand?notice=disabled");
    ui.post("/devices/kueche-wand/enabled", &[("enabled", "true")])
        .await
        .assert_redirect("/devices/kueche-wand?notice=enabled");
    let changes = ui.take_changes();
    assert!(matches!(
        &changes[0],
        ConfigChange::Device { run: None, .. }
    ));
    assert!(matches!(
        &changes[1],
        ConfigChange::Device { run: Some(_), .. }
    ));

    // The token removed: the bridge stops it, the page says "not paired".
    ui.post(
        "/devices/kueche-wand",
        &[
            ("name", "Kitchen"),
            ("host", "192.0.2.31"),
            ("port", "16022"),
            ("token", ""),
            ("clear_token", "true"),
        ],
    )
    .await
    .assert_redirect("/devices/kueche-wand?notice=saved");
    assert!(matches!(
        ui.take_changes().as_slice(),
        [ConfigChange::Device { run: None, .. }]
    ));
    assert!(ui.get("/devices").await.body.contains("not paired"));

    // Removed after confirmation.
    let confirm = ui.get("/devices/kueche-wand/remove").await;
    assert!(confirm.body.contains("Remove “Kitchen”?"));
    ui.post("/devices/kueche-wand/remove", &[])
        .await
        .assert_redirect("/devices?notice=removed");
    assert!(matches!(
        ui.take_changes().as_slice(),
        [ConfigChange::Device { run: None, .. }]
    ));
    assert!(ui.store.devices().await.unwrap().is_empty());
    assert_eq!(
        ui.get("/devices/kueche-wand").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        ui.post("/devices/kueche-wand/remove", &[]).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_overview_shows_broker_and_device_state() {
    let mut ui = Ui::signed_in().await;
    let page = ui.get("/").await;
    assert!(page.body.contains("No broker configured yet"));
    assert!(page.body.contains("No device yet"));

    ui.post(
        "/broker",
        &[
            ("url", "mqtt://broker.test"),
            ("username", ""),
            ("password", ""),
            ("prefix", "nanoleaf"),
        ],
    )
    .await;
    add_device(&mut ui, "Hall", "hall", "tok").await;
    add_device(&mut ui, "Stairs", "stairs", "").await;
    *ui.backend.status.lock().unwrap() = BridgeStatus {
        broker_connected: true,
        devices: vec![DeviceStatus {
            id: id("hall"),
            reachable: Some(false),
            values: vec![
                ("brightness".to_owned(), "42".to_owned()),
                ("effects".to_owned(), "[\"A\"]".to_owned()),
            ],
        }],
    };
    let fragment = ui.get("/fragments/overview").await;
    for expected in [
        "broker.test:1883",
        "connected",
        "unreachable",
        "not paired",
        "<code>brightness</code> 42",
        "hx-trigger=\"every 5s\"",
    ] {
        assert!(
            fragment.body.contains(expected),
            "{expected}: {}",
            fragment.body
        );
    }
    assert!(!fragment.body.contains("<code>effects</code>"));
    assert!(!fragment.body.contains("<html"), "a fragment, not a page");
}

#[tokio::test]
async fn the_home_assistant_switch_is_stored_and_sent() {
    let mut ui = Ui::signed_in().await;
    assert!(!ui.get("/settings").await.body.contains("checked"));
    ui.post("/settings/discovery", &[("ha_discovery", "true")])
        .await
        .assert_redirect("/settings?notice=saved");
    assert!(ui.store.settings().await.unwrap().ha_discovery);
    assert!(ui.get("/settings").await.body.contains("checked"));
    ui.post("/settings/discovery", &[]).await;
    assert!(!ui.store.settings().await.unwrap().ha_discovery);
    let changes = ui.take_changes();
    assert!(matches!(
        changes.as_slice(),
        [
            ConfigChange::HaDiscovery(true),
            ConfigChange::HaDiscovery(false)
        ]
    ));
}

#[tokio::test]
async fn names_are_escaped() {
    let mut ui = Ui::signed_in().await;
    add_device(&mut ui, "<script>alert(1)</script>", "x", "").await;
    let page = ui.get("/devices").await;
    assert!(!page.body.contains("<script>alert"), "{}", page.body);
    assert!(
        page.body.contains("script&#62;alert(1)") || page.body.contains("script&gt;alert(1)"),
        "{}",
        page.body
    );
}
