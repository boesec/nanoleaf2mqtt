//! Access to the UI: first start, login, throttling, logout, session expiry, same-origin check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "web_support.rs"]
mod support;

use std::time::Duration;

use axum::http::StatusCode;
use nanoleaf2mqtt::web::AuthTiming;
use support::{ADMIN_PASSWORD, Ui};

/// Every page and action besides login, logout, first start and assets.
const GET_PAGES: [&str; 10] = [
    "/",
    "/fragments/overview",
    "/broker",
    "/devices",
    "/devices/new",
    "/devices/suggest-id?name=x",
    "/devices/dev",
    "/devices/dev/remove",
    "/settings",
    "/no-such-page",
];
const POST_ACTIONS: [&str; 9] = [
    "/broker",
    "/devices",
    "/devices/dev",
    "/devices/dev/enabled",
    "/devices/dev/pair",
    "/devices/dev/remove",
    "/settings/discovery",
    "/settings/password",
    "/logout",
];

/// Time jumps on tokio's clock (sessions and throttle use it) while the store keeps running on
/// its own threads.
async fn time_passes(by: Duration) {
    tokio::time::pause();
    tokio::time::advance(by).await;
    tokio::time::resume();
}

#[tokio::test]
async fn first_start_offers_only_the_password_page() {
    let mut ui = Ui::new().await;
    for path in GET_PAGES.iter().chain(&["/login"]) {
        ui.get(path).await.assert_redirect("/setup");
    }
    for path in POST_ACTIONS.iter().chain(&["/login"]) {
        ui.post(path, &[("password", ADMIN_PASSWORD)])
            .await
            .assert_redirect("/setup");
    }
    let page = ui.get("/setup").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Set the admin password"));
    assert!(!page.body.contains("href=\"/broker\""), "no navigation yet");
    assert_eq!(ui.get("/assets/htmx.min.js").await.status, StatusCode::OK);
    assert_eq!(ui.get("/assets/style.css").await.status, StatusCode::OK);

    // A bad first password is refused, nothing is stored.
    for (password, confirm) in [("short", "short"), (ADMIN_PASSWORD, "different-password")] {
        let answer = ui
            .post("/setup", &[("password", password), ("confirm", confirm)])
            .await;
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!answer.body.contains(password), "password echoed");
    }
    assert_eq!(ui.store.admin_password_hash().await.unwrap(), None);

    ui.setup(ADMIN_PASSWORD).await;
    let hash = ui.store.admin_password_hash().await.unwrap().unwrap();
    assert!(hash.starts_with("$argon2id$"), "{hash}");
    assert_eq!(ui.get("/").await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_first_start_page_cannot_replace_the_password() {
    let mut ui = Ui::signed_in().await;
    let hash = ui.store.admin_password_hash().await.unwrap();
    ui.get("/setup").await.assert_redirect("/login");
    ui.post(
        "/setup",
        &[
            ("password", "attacker-password"),
            ("confirm", "attacker-password"),
        ],
    )
    .await
    .assert_redirect("/login");
    assert_eq!(ui.store.admin_password_hash().await.unwrap(), hash);
}

#[tokio::test]
async fn every_page_needs_a_session() {
    let mut ui = Ui::signed_in().await;
    ui.cookie = None;
    for path in GET_PAGES {
        ui.get(path).await.assert_redirect("/login");
    }
    for path in POST_ACTIONS.iter().filter(|p| **p != "/logout") {
        ui.post(path, &[("enabled", "true")])
            .await
            .assert_redirect("/login");
    }
    // A forged session id is no session.
    ui.cookie = Some(format!("nanoleaf2mqtt_session={}", "ab".repeat(32)));
    ui.get("/").await.assert_redirect("/login");
    // htmx gets a redirect it follows with a full page load.
    let answer = ui.get_htmx("/fragments/overview").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.headers.get("hx-redirect").unwrap().to_str().unwrap(),
        "/login"
    );
    assert!(ui.take_changes().is_empty(), "nothing changed");

    let page = ui.get("/login").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Log in"));
}

#[tokio::test]
async fn login_sets_a_strict_session_cookie_and_logout_ends_the_session() {
    let mut ui = Ui::signed_in().await;
    ui.cookie = None;
    let answer = ui.login(ADMIN_PASSWORD).await;
    answer.assert_redirect("/");
    let cookie = answer
        .headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(cookie.contains(attribute), "{cookie}");
    }
    let id = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("nanoleaf2mqtt_session=");
    assert_eq!(id.len(), 64, "256-bit id");
    assert_eq!(ui.get("/").await.status, StatusCode::OK);
    // A logged-in user is sent on from the login page.
    ui.get("/login").await.assert_redirect("/");

    let session = ui.cookie.clone();
    let answer = ui.post("/logout", &[]).await;
    answer.assert_redirect("/login");
    assert!(
        answer
            .headers
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    // The old cookie is worth nothing now.
    ui.cookie = session;
    ui.get("/").await.assert_redirect("/login");
}

#[tokio::test]
async fn a_wrong_password_is_rejected_delayed_and_locks_out_for_a_while() {
    let timing = AuthTiming {
        failure_delay: Duration::from_millis(300),
        lockout_initial: Duration::from_secs(10),
        ..AuthTiming::default()
    };
    let mut ui = Ui::with_timing(timing).await;
    ui.setup(ADMIN_PASSWORD).await;
    ui.cookie = None;

    let started = std::time::Instant::now();
    let answer = ui.login("wrong-password").await;
    assert!(started.elapsed() >= Duration::from_millis(300), "delayed");
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert!(answer.body.contains("Wrong password"));
    assert!(!answer.body.contains("wrong-password"), "password echoed");
    assert!(ui.cookie.is_none());

    // Locked out: even the right password is not checked now.
    let answer = ui.login(ADMIN_PASSWORD).await;
    assert_eq!(answer.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(answer.body.contains("Too many failed attempts"));
    assert!(ui.cookie.is_none());

    time_passes(Duration::from_secs(11)).await;
    ui.login(ADMIN_PASSWORD).await.assert_redirect("/");

    // A success resets the count: the next failure locks out for the first period again.
    ui.cookie = None;
    assert_eq!(
        ui.login("wrong-again").await.status,
        StatusCode::UNAUTHORIZED
    );
    time_passes(Duration::from_secs(11)).await;
    ui.login(ADMIN_PASSWORD).await.assert_redirect("/");
}

#[tokio::test]
async fn a_session_expires_when_idle() {
    let mut ui = Ui::signed_in().await;
    // Used within the idle limit, the session stays.
    time_passes(Duration::from_secs(11 * 3600)).await;
    assert_eq!(ui.get("/").await.status, StatusCode::OK);
    time_passes(Duration::from_secs(11 * 3600)).await;
    assert_eq!(ui.get("/").await.status, StatusCode::OK);
    // Twelve hours without a request end it.
    time_passes(Duration::from_secs(12 * 3600 + 1)).await;
    ui.get("/").await.assert_redirect("/login");
}

#[tokio::test]
async fn a_session_ends_after_seven_days_however_busy() {
    let mut ui = Ui::signed_in().await;
    let mut elapsed = Duration::ZERO;
    let step = Duration::from_secs(10 * 3600);
    let week = Duration::from_secs(7 * 24 * 3600);
    while elapsed + step < week {
        time_passes(step).await;
        elapsed += step;
        assert_eq!(
            ui.get("/").await.status,
            StatusCode::OK,
            "after {elapsed:?}"
        );
    }
    time_passes(step).await;
    ui.get("/").await.assert_redirect("/login");
}

#[tokio::test]
async fn a_post_from_another_origin_is_refused() {
    let mut ui = Ui::signed_in().await;
    for origin in ["https://evil.example", "null", "http://ui.test:9999"] {
        let answer = ui
            .post_from(
                "/broker",
                &[
                    ("url", "mqtt://evil.example"),
                    ("username", ""),
                    ("password", ""),
                    ("prefix", "nanoleaf"),
                ],
                Some(origin),
            )
            .await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN, "{origin}");
    }
    // A foreign Referer without Origin counts as well.
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/settings/discovery")
        .header("host", support::HOST_NAME)
        .header("referer", "https://evil.example/page")
        .header("cookie", ui.cookie.clone().unwrap())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from("ha_discovery=true"))
        .unwrap();
    let response = tower::ServiceExt::oneshot(ui.router.clone(), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // Also the login cannot be posted to from elsewhere.
    ui.cookie = None;
    let answer = ui
        .post_from(
            "/login",
            &[("password", ADMIN_PASSWORD)],
            Some("https://evil.example"),
        )
        .await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);

    let settings = ui.store.settings().await.unwrap();
    assert!(settings.broker_url.is_none() && !settings.ha_discovery);
    assert!(ui.take_changes().is_empty());
}

#[tokio::test]
async fn changing_the_password_logs_out_every_other_session() {
    let mut ui = Ui::signed_in().await;
    let other_session = ui.cookie.clone();

    let answer = ui
        .post(
            "/settings/password",
            &[
                ("current", "not-the-password"),
                ("password", "new-password-1234"),
                ("confirm", "new-password-1234"),
            ],
        )
        .await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert!(answer.body.contains("current password is wrong"));

    time_passes(Duration::from_secs(2)).await;
    ui.post(
        "/settings/password",
        &[
            ("current", ADMIN_PASSWORD),
            ("password", "new-password-1234"),
            ("confirm", "new-password-1234"),
        ],
    )
    .await
    .assert_redirect("/settings?notice=password");
    assert_ne!(ui.cookie, other_session, "a new session for this browser");
    assert_eq!(ui.get("/settings").await.status, StatusCode::OK);

    ui.cookie = other_session;
    ui.get("/").await.assert_redirect("/login");
    ui.cookie = None;
    assert_eq!(
        ui.login(ADMIN_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    time_passes(Duration::from_secs(2)).await;
    ui.login("new-password-1234").await.assert_redirect("/");
}

#[tokio::test]
async fn answers_carry_security_headers() {
    let mut ui = Ui::signed_in().await;
    for path in ["/", "/login", "/assets/style.css"] {
        let answer = ui.get(path).await;
        let header = |name: &str| {
            answer
                .headers
                .get(name)
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default()
        };
        assert_eq!(header("cache-control"), "no-store", "{path}");
        assert_eq!(header("x-frame-options"), "DENY", "{path}");
        assert!(header("content-security-policy").contains("default-src 'self'"));
    }
}
