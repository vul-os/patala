//! The webhook route, over real HTTP.
//!
//! Two properties matter here and nothing else does:
//!
//! 1. **The bytes and headers a processor sent arrive at the rail unchanged.**
//!    Every webhook scheme signs the exact body that was transmitted, so a
//!    sidecar that decodes and re-encodes JSON on the way through would
//!    invalidate the signature of every genuine delivery — and the failure
//!    would look like "the processor is sending bad signatures", which is a
//!    miserable thing to debug. `SpyRail` below asserts byte-equality of the
//!    body and presence of the forwarded headers/query.
//! 2. **A rail with no push delivery says so, and never fakes an answer.**
//!    The offline `"mock"` rail leaves `verify_webhook` at the trait default,
//!    which must surface as `501 unsupported`, not `200`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use patala_core::{
    Error, MockRail, PayRequest, PaymentRail, Quote, RailCapabilities, RailClass, Receipt, Result,
    Settlement, WebhookDelivery, WebhookEvent,
};
use patala_sidecar::{app, auth::SidecarToken, registry::RailRegistry};

/// A rail that accepts any delivery and records exactly what it was handed,
/// so the test can assert the sidecar forwarded it verbatim.
struct SpyRail {
    capabilities: RailCapabilities,
    seen: Arc<Mutex<Option<WebhookDelivery>>>,
}

#[async_trait]
impl PaymentRail for SpyRail {
    fn id(&self) -> &str {
        "spy"
    }

    fn capabilities(&self) -> &RailCapabilities {
        &self.capabilities
    }

    async fn quote(&self, _req: &PayRequest) -> Result<Quote> {
        Err(Error::Unsupported("quote"))
    }

    async fn charge(&self, _req: &PayRequest) -> Result<Receipt> {
        Err(Error::Unsupported("charge"))
    }

    async fn verify(&self, _receipt: &Receipt) -> Result<bool> {
        Ok(false)
    }

    async fn verify_webhook(&self, delivery: &WebhookDelivery) -> Result<WebhookEvent> {
        *self.seen.lock().unwrap() = Some(delivery.clone());
        Ok(
            WebhookEvent::settlement("spy", "evt_spy", "ord_spy", true, 4200, "ZAR")
                .with_object_id("obj_spy"),
        )
    }
}

async fn spawn(token: &str, seen: Arc<Mutex<Option<WebhookDelivery>>>) -> String {
    let mut registry: RailRegistry = std::collections::HashMap::new();
    registry.insert(
        "mock".to_string(),
        Arc::new(MockRail::new(
            "mock",
            RailClass::NonCustodialFinal,
            vec!["USDC".to_string()],
        )) as Arc<dyn PaymentRail>,
    );
    registry.insert(
        "spy".to_string(),
        Arc::new(SpyRail {
            capabilities: RailCapabilities {
                class: RailClass::CustodialReversible,
                reversible: true,
                requires_kyc: true,
                holds_funds: true,
                currencies: vec!["ZAR".to_string()],
                settlement: Settlement::Days(2),
                atomic_multi_party: false, // this webhook-test rail implements no atomic operation (B3)
            },
            seen,
        }) as Arc<dyn PaymentRail>,
    );

    let router = app(registry, SidecarToken::new(token));
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
async fn delivery_reaches_the_rail_byte_for_byte() {
    let token = "test-token-webhook";
    let seen = Arc::new(Mutex::new(None));
    let base = spawn(token, Arc::clone(&seen)).await;
    let client = reqwest::Client::new();

    // Deliberately not canonical JSON: extra whitespace and a key order no
    // serializer would reproduce. If anything on the path re-encodes the
    // body, these bytes change and every real signature check would fail.
    let body = b"{ \"z\":1,  \"a\" : \"\xc3\xa9\" }".to_vec();

    let resp = client
        .post(format!("{base}/v1/rails/spy/webhook?secret=s3cr3t"))
        .bearer_auth(token)
        .header("Stripe-Signature", "t=1,v1=deadbeef")
        .header("Content-Type", "application/json")
        .body(body.clone())
        .send()
        .await
        .expect("webhook request");
    assert_eq!(resp.status(), 200);

    let event: serde_json::Value = resp.json().await.expect("webhook body");
    assert_eq!(event["rail_id"], serde_json::json!("spy"));
    assert_eq!(event["status"], serde_json::json!("Settled"));
    assert_eq!(event["amount_minor"], serde_json::json!(4200));
    assert_eq!(event["object_id"], serde_json::json!("obj_spy"));
    assert!(
        event["amount_minor"].is_number(),
        "amount must cross as a JSON number, never a float-shaped string"
    );

    let delivery = seen.lock().unwrap().clone().expect("rail saw a delivery");
    assert_eq!(
        delivery.raw_body, body,
        "the sidecar must forward the body verbatim — re-encoding it would \
         invalidate every genuine webhook signature"
    );
    // Header lookup is case-insensitive on the rail side, so whatever casing
    // the HTTP stack normalised to still resolves.
    assert_eq!(delivery.header("Stripe-Signature"), Some("t=1,v1=deadbeef"));
    assert_eq!(delivery.query_param("secret"), Some("s3cr3t"));
    assert!(
        delivery.now_unix > 1_700_000_000,
        "the rail must be handed a real `now` to check replay windows against"
    );
}

#[tokio::test]
async fn a_rail_with_no_push_delivery_answers_501_not_200() {
    let token = "test-token-webhook-unsupported";
    let seen = Arc::new(Mutex::new(None));
    let base = spawn(token, seen).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/v1/rails/mock/webhook"))
        .bearer_auth(token)
        .body("{}")
        .send()
        .await
        .expect("webhook request");
    assert_eq!(
        resp.status(),
        501,
        "the mock rail has no processor and must say so, never answer 200"
    );
    let err: serde_json::Value = resp.json().await.expect("error body");
    assert_eq!(err["kind"], serde_json::json!("unsupported"));
}

#[tokio::test]
async fn the_webhook_route_is_behind_the_token_like_everything_else() {
    let seen = Arc::new(Mutex::new(None));
    let base = spawn("the-real-token", seen).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/v1/rails/spy/webhook"))
        .body("{}")
        .send()
        .await
        .expect("webhook request without auth");
    assert_eq!(resp.status(), 401);
}
