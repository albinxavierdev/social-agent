mod common;
use axum::{body::Body, http::Request};
use common::*;
use content_agent::testing::{harness, HarnessOptions};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(api: &Api, path: &str) -> (u16, axum::http::HeaderMap, String) {
    let r = api.router.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
    let (s, h) = (r.status().as_u16(), r.headers().clone());
    (s, h, String::from_utf8_lossy(&r.into_body().collect().await.unwrap().to_bytes()).into_owned())
}

#[tokio::test]
async fn console_is_off_by_default() {
    let h = harness(HarnessOptions::default());
    assert_eq!(get(&Api::new(&h), "/console").await.0, 404);
}

#[tokio::test]
async fn console_serves_a_locked_down_secret_free_page_when_enabled() {
    let mut cfg = content_agent::config::Config::for_tests();
    cfg.console_enabled = true;
    let h = harness(HarnessOptions { cfg, ..Default::default() });
    let (s, headers, body) = get(&Api::new(&h), "/console").await;
    assert_eq!(s, 200);
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'") && csp.contains("connect-src 'self'") && csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("img-src https: data:"), "thumbnails may load from https only");
    assert_eq!(headers["cache-control"], "no-store");
    // The page calls the real API and carries no credentials.
    assert!(body.contains("/internal/v1/tools") && body.contains("x-lwm-signature"));
    assert!(!body.contains(content_agent::testing::TEST_SECRET));
    // Every registered tool has an example so the console can run it.
    for t in content_agent::tools::TOOLS {
        assert!(body.contains(&format!("{}:", t.name)), "console has no example for {}", t.name);
    }
    // The readable content view exists and renders fetched data only as text, never as HTML.
    assert!(body.contains("What it fetched") && body.contains("function renderView"));
    assert!(!body.contains("innerHTML") || body.matches("innerHTML").count() == 1, "only the static icon constant may use innerHTML");
    // No dynamic HTML injection of server data.
    assert!(!body.contains("innerHTML = (") && !body.contains(".innerHTML=res"));
}

#[test]
fn console_is_refused_in_production() {
    let mut cfg = content_agent::config::Config::for_tests();
    cfg.env = "production".into();
    cfg.console_enabled = true;
    // Config::validate is private; exercise it through the public env path.
    std::env::set_var("APP_ENV", "production");
    std::env::set_var("CONSOLE_ENABLED", "true");
    std::env::set_var("INTERNAL_AUTH_SECRETS", "test-secret-test-secret-test-secret-123");
    let e = content_agent::config::Config::from_env().unwrap_err().to_string();
    std::env::remove_var("APP_ENV");
    std::env::remove_var("CONSOLE_ENABLED");
    assert!(e.contains("CONSOLE_ENABLED"), "{e}");
}
