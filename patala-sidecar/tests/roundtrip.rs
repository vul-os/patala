//! Boots the sidecar's real `axum::Router` (the same one `main.rs` serves)
//! in-process, on an OS-assigned loopback port, and drives a full
//! charge -> verify round trip over actual HTTP against the offline
//! `"mock"` rail — no network beyond localhost, no real rail required
//! (`PATALA.md` §8).

use std::net::SocketAddr;

use patala_core::{PayRequest, RailCapabilities};
use patala_sidecar::{app, auth::SidecarToken, registry::default_registry};
use serde_json::json;

/// Starts the sidecar router on `127.0.0.1:0` (OS picks a free port) and
/// returns its base URL plus the token a caller must present. The server
/// task is detached; the process exits with the test.
async fn spawn_sidecar(token: &str) -> String {
    let router = app(default_registry(), SidecarToken::new(token));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn charge_then_verify_round_trips_over_http() {
    let token = "test-token-charge-verify";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();

    // Unauthenticated health check needs no token at all.
    let health = client
        .get(format!("{base}/healthz"))
        .send()
        .await
        .expect("healthz request");
    assert_eq!(health.status(), 200);

    // Capabilities are readable before any money moves, and carry the
    // settlement class in the type — PATALA.md §3 — even across JSON.
    let caps: RailCapabilities = client
        .get(format!("{base}/v1/rails/mock"))
        .bearer_auth(token)
        .send()
        .await
        .expect("capabilities request")
        .json()
        .await
        .expect("capabilities body");
    assert_eq!(caps.currencies, vec!["USDC".to_string(), "USD".to_string()]);

    let req = PayRequest {
        amount_minor: 1_250,
        currency: "USDC".to_string(),
        destination: "dest-anything".to_string(),
        reference: "sidecar-order-1".to_string(),
    };

    let quote: serde_json::Value = client
        .post(format!("{base}/v1/rails/mock/quote"))
        .bearer_auth(token)
        .json(&req)
        .send()
        .await
        .expect("quote request")
        .json()
        .await
        .expect("quote body");
    assert_eq!(quote["total_minor"], json!(1_250));
    assert!(
        quote["total_minor"].is_number(),
        "amount must be a JSON number, never a float-shaped string"
    );

    let receipt: serde_json::Value = client
        .post(format!("{base}/v1/rails/mock/charge"))
        .bearer_auth(token)
        .json(&req)
        .send()
        .await
        .expect("charge request")
        .json()
        .await
        .expect("charge body");
    assert_eq!(receipt["amount_minor"], json!(1_250));
    assert_eq!(receipt["rail_id"], json!("mock"));

    let verify: serde_json::Value = client
        .post(format!("{base}/v1/rails/mock/verify"))
        .bearer_auth(token)
        .json(&receipt)
        .send()
        .await
        .expect("verify request")
        .json()
        .await
        .expect("verify body");
    assert_eq!(
        verify["valid"],
        json!(true),
        "a genuine receipt must verify over HTTP"
    );

    // A tampered receipt must fail closed, not error out.
    let mut tampered = receipt.clone();
    tampered["amount_minor"] = json!(999_999);
    let verify_tampered: serde_json::Value = client
        .post(format!("{base}/v1/rails/mock/verify"))
        .bearer_auth(token)
        .json(&tampered)
        .send()
        .await
        .expect("verify request")
        .json()
        .await
        .expect("verify body");
    assert_eq!(
        verify_tampered["valid"],
        json!(false),
        "a tampered receipt must never verify, even over HTTP"
    );
}

#[tokio::test]
async fn missing_or_wrong_token_is_rejected_before_reaching_a_rail() {
    let base = spawn_sidecar("the-real-token").await;
    let client = reqwest::Client::new();
    let req = PayRequest {
        amount_minor: 100,
        currency: "USDC".to_string(),
        destination: "dest".to_string(),
        reference: "sidecar-order-2".to_string(),
    };

    // No Authorization header at all.
    let no_auth = client
        .post(format!("{base}/v1/rails/mock/charge"))
        .json(&req)
        .send()
        .await
        .expect("request without auth");
    assert_eq!(no_auth.status(), 401);

    // Wrong token.
    let wrong_auth = client
        .post(format!("{base}/v1/rails/mock/charge"))
        .bearer_auth("not-the-token")
        .json(&req)
        .send()
        .await
        .expect("request with wrong token");
    assert_eq!(wrong_auth.status(), 401);

    // Even a read-only capabilities lookup is gated — the token guards the
    // whole `/v1` surface, not just money-moving endpoints.
    let caps_no_auth = client
        .get(format!("{base}/v1/rails/mock"))
        .send()
        .await
        .expect("capabilities request without auth");
    assert_eq!(caps_no_auth.status(), 401);
}

#[tokio::test]
async fn unknown_rail_id_is_reported_as_not_found() {
    let token = "test-token-unknown-rail";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();
    let req = PayRequest {
        amount_minor: 100,
        currency: "USDC".to_string(),
        destination: "dest".to_string(),
        reference: "sidecar-order-3".to_string(),
    };

    let resp = client
        .post(format!("{base}/v1/rails/does-not-exist/charge"))
        .bearer_auth(token)
        .json(&req)
        .send()
        .await
        .expect("request against an unknown rail id");
    assert_eq!(resp.status(), 404);
}
