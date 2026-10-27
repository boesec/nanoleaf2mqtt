//! The UI after the admin password was removed from outside (the reset command).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "web_support.rs"]
mod support;

use axum::http::StatusCode;
use support::Ui;

/// The app had seen the password before; it must still offer the set-password page.
#[tokio::test]
async fn a_reset_underneath_a_running_app_leads_to_the_set_password_page() {
    let mut ui = Ui::signed_in().await;
    assert_eq!(ui.get("/").await.status, StatusCode::OK);

    assert!(ui.store.clear_admin_password().await.unwrap());

    ui.get("/").await.assert_redirect("/setup");
    ui.get("/login").await.assert_redirect("/setup");
    let page = ui.get("/setup").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Set the admin password"));

    ui.setup("a-new-admin-password-31").await;
    assert_eq!(ui.get("/").await.status, StatusCode::OK);
}
