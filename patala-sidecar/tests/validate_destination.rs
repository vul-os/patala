//! `POST /v1/rails/:rail_id/validate-destination`, driven over a real
//! loopback socket against the sidecar's real `axum::Router`.
//!
//! This is the route a consumer with no FFI uses to run the pre-flight check
//! in the two-party payout flow (`docs/compensating-payments.md`): on a final
//! rail there is no reversal, so paying a customer back is a second `charge`
//! to an address the **customer** supplies, and this is what tells a human
//! what is decidably wrong with that address before anyone approves it.
//!
//! What these tests are actually defending:
//!
//! 1. All five verdicts reach an HTTP caller as five distinct values. A
//!    surface that flattened them to a bool would defeat the design.
//! 2. `human_must_confirm` and `exchange_deposit_caveat` are on **every**
//!    verdict, including the most positive one, so a caller cannot build a UI
//!    that skips the human.
//! 3. `is_refusal` crosses as data, because it is a *method* on the core type
//!    and methods do not survive JSON.
//! 4. A malformed *request* is a fail-closed `400` and never a fabricated
//!    verdict — while a malformed *address* is a `200` carrying a refusal,
//!    which is the rail's answer, not a transport error.

use std::net::SocketAddr;
use std::sync::Arc;

use patala_core::{MockRail, PaymentRail, RailClass};
use patala_sidecar::{app, auth::SidecarToken, registry::RailRegistry};
use serde_json::json;

/// The registry these tests serve. Two rails, because one of the five verdicts
/// is only reachable from a rail that declines to check at all:
///
/// - `"mock"` parses its synthetic `<network>:<kind>:<label>` grammar, which is
///   what makes `Malformed` / `WrongNetwork` / `NotAWallet` /
///   `StructurallyValid` reachable with no chain compiled in.
/// - `"opaque"` stands in for a fiat rail, whose `destination` is a
///   processor-side token meaningful only to that processor — it answers
///   `Unknown`, and a caller must treat that as "a human must decide", never as
///   valid.
///
/// This is deliberately **not** `default_registry()`: that one is mock-only and
/// `registry::tests::registry_is_mock_only` pins it that way. Building a
/// registry here exercises the same handlers without touching that claim.
fn two_rail_registry() -> RailRegistry {
    let mut registry = RailRegistry::new();
    registry.insert(
        "mock".to_string(),
        Arc::new(MockRail::new(
            "mock",
            RailClass::NonCustodialFinal,
            vec!["USDC".to_string()],
        )) as Arc<dyn PaymentRail>,
    );
    registry.insert(
        "opaque".to_string(),
        Arc::new(
            MockRail::new(
                "opaque",
                RailClass::CustodialReversible,
                vec!["USD".to_string()],
            )
            .without_destination_checks(),
        ) as Arc<dyn PaymentRail>,
    );
    registry
}

async fn spawn_sidecar(token: &str) -> String {
    let router = app(two_rail_registry(), SidecarToken::new(token));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    format!("http://{addr}")
}

/// POST a raw body string, so a test can send something that is not valid JSON
/// at all — which `.json()` on the client could not construct.
async fn post_raw(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &'static str,
) -> reqwest::Response {
    client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .expect("validate-destination request")
}

#[tokio::test]
async fn every_verdict_reaches_an_http_caller_as_a_distinct_status() {
    let token = "test-token-validate-status";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();

    for (rail, dest, want_status, want_refusal) in [
        ("mock", "mock:wallet:alice", "StructurallyValid", false),
        ("mock", "mock:program:vault", "NotAWallet", true),
        ("mock", "stellar:wallet:alice", "WrongNetwork", true),
        ("mock", "definitely-not-an-address", "Malformed", true),
        ("opaque", "cus_opaque_processor_token", "Unknown", false),
    ] {
        let resp = client
            .post(format!("{base}/v1/rails/{rail}/validate-destination"))
            .bearer_auth(token)
            .json(&json!({ "destination": dest }))
            .send()
            .await
            .expect("validate-destination request");

        // Every verdict is a 200: a rail's honest refusal is data, exactly as
        // `verify` returns 200 with {"valid": false}. Mapping some verdicts to
        // HTTP errors would flatten a five-state answer to worked/did-not.
        assert_eq!(
            resp.status(),
            200,
            "{rail}/{dest:?} should answer 200 with a verdict"
        );
        let body: serde_json::Value = resp.json().await.expect("verdict body");

        assert_eq!(
            body["status"],
            json!(want_status),
            "{rail}/{dest:?} status; full body: {body}"
        );
        assert_eq!(
            body["is_refusal"],
            json!(want_refusal),
            "{rail}/{dest:?} is_refusal; full body: {body}"
        );
        assert_eq!(
            body["rail_id"],
            json!(rail),
            "a verdict must name the rail that formed it"
        );

        // On every verdict, including StructurallyValid.
        assert_eq!(
            body["human_must_confirm"],
            json!(true),
            "{rail}/{dest:?} must still require a human to confirm"
        );
        let caveat = body["exchange_deposit_caveat"]
            .as_str()
            .expect("exchange_deposit_caveat must be a string on every verdict");
        assert!(
            caveat.contains("exchange"),
            "{rail}/{dest:?} must carry the exchange caveat verbatim: {caveat:?}"
        );

        // A reason a UI can put in front of a person, always.
        let reason = body["reason"].as_str().expect("reason must be a string");
        assert!(
            !reason.trim().is_empty(),
            "{rail}/{dest:?} verdict has no reason a caller could show anyone"
        );
    }
}

#[tokio::test]
async fn the_json_shape_is_the_core_verdict_plus_is_refusal_and_nothing_else() {
    // Pins the wire contract a non-Rust consumer codes against. `is_refusal`
    // is here because it is a METHOD on patala_core::DestinationVerdict and a
    // method does not survive JSON — leaving it out would force every consumer
    // to re-derive it from `status`, and a switch that has not heard of a
    // status added later defaults to "not a refusal", which fails OPEN.
    let token = "test-token-validate-shape";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .post(format!("{base}/v1/rails/mock/validate-destination"))
        .bearer_auth(token)
        .json(&json!({ "destination": "mock:wallet:alice" }))
        .send()
        .await
        .expect("validate-destination request")
        .json()
        .await
        .expect("verdict body");

    let obj = body.as_object().expect("the verdict must be a JSON object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "exchange_deposit_caveat",
            "human_must_confirm",
            "is_refusal",
            "rail_id",
            "reason",
            "status",
        ],
        "the wire shape changed; update patala-go/bindingtest and \
         docs/compensating-payments.md in the same change"
    );

    // `status` is a name, not an integer: a consumer's mapping must not break
    // if a variant is ever reordered in the Rust enum.
    assert!(
        body["status"].is_string(),
        "status must serialize as a name, not a positional discriminant"
    );
}

#[tokio::test]
async fn a_malformed_request_is_a_fail_closed_400_and_never_a_verdict() {
    let token = "test-token-validate-badreq";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();
    let url = format!("{base}/v1/rails/mock/validate-destination");

    for (name, raw) in [
        ("not json at all", "this is not json"),
        ("empty body", ""),
        ("json but not an object", "\"mock:wallet:alice\""),
        ("missing the destination field", "{}"),
        (
            "destination misspelt",
            "{\"destinaton\":\"mock:wallet:alice\"}",
        ),
        ("destination is not a string", "{\"destination\":42}"),
        ("destination is null", "{\"destination\":null}"),
        (
            "an extra field the server would have ignored",
            "{\"destination\":\"mock:wallet:alice\",\"amount_minor\":100}",
        ),
    ] {
        let resp = post_raw(&client, &url, token, raw).await;
        assert_eq!(
            resp.status(),
            400,
            "{name}: a request whose meaning is unclear must be refused, not answered"
        );

        let body: serde_json::Value = resp.json().await.expect("error body");
        assert_eq!(
            body["kind"],
            json!("invalid_request"),
            "{name}: errors must come back in this sidecar's own envelope"
        );
        // The decisive property: no verdict is invented for a request the
        // server could not understand. A caller cannot mistake a rejected
        // request for a checked address.
        assert!(
            body.get("status").is_none() && body.get("is_refusal").is_none(),
            "{name}: a 400 must carry no verdict fields; got {body}"
        );
    }
}

#[tokio::test]
async fn an_empty_address_is_the_rails_refusal_not_a_request_error() {
    // The distinction the 400 path above must not swallow: `{"destination": ""}`
    // is a perfectly well-formed REQUEST, so it gets a 200 — carrying the
    // rail's `Malformed` refusal, because an empty string is undeliverable on
    // every rail there is. Guards fail closed: this is a refusal, never a shrug.
    let token = "test-token-validate-empty";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();

    for dest in ["", "   ", "\t\n"] {
        let resp = client
            .post(format!("{base}/v1/rails/mock/validate-destination"))
            .bearer_auth(token)
            .json(&json!({ "destination": dest }))
            .send()
            .await
            .expect("validate-destination request");
        assert_eq!(resp.status(), 200, "{dest:?} is a well-formed request");

        let body: serde_json::Value = resp.json().await.expect("verdict body");
        assert_eq!(body["status"], json!("Malformed"), "{dest:?}");
        assert_eq!(
            body["is_refusal"],
            json!(true),
            "{dest:?} must be a refusal a caller cannot charge past"
        );
    }
}

#[tokio::test]
async fn the_route_is_token_gated_and_404s_on_an_unknown_rail() {
    // Same guarantees as every other /v1 route — asserted here rather than
    // assumed, because this route was added after the others and a route
    // registered outside the protected sub-router would be a silent hole.
    let token = "test-token-validate-auth";
    let base = spawn_sidecar(token).await;
    let client = reqwest::Client::new();
    let body = json!({ "destination": "mock:wallet:alice" });

    let no_auth = client
        .post(format!("{base}/v1/rails/mock/validate-destination"))
        .json(&body)
        .send()
        .await
        .expect("request without auth");
    assert_eq!(no_auth.status(), 401, "this route must be behind the token");

    let wrong_auth = client
        .post(format!("{base}/v1/rails/mock/validate-destination"))
        .bearer_auth("not-the-token")
        .json(&body)
        .send()
        .await
        .expect("request with wrong token");
    assert_eq!(wrong_auth.status(), 401);

    let unknown_rail = client
        .post(format!(
            "{base}/v1/rails/does-not-exist/validate-destination"
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("request against an unknown rail id");
    assert_eq!(
        unknown_rail.status(),
        404,
        "an unregistered rail must 404, never answer with someone else's verdict"
    );
}

#[tokio::test]
async fn the_route_never_touches_a_rails_network_path() {
    // The purity contract, asserted where it is easiest to break: this handler
    // must work on a sidecar whose rails have no reachable RPC or processor.
    // A `failing` MockRail errors on every quote/charge; validate_destination
    // must still answer, because it is not allowed to go anywhere.
    let mut registry = RailRegistry::new();
    registry.insert(
        "offline".to_string(),
        Arc::new(
            MockRail::new(
                "offline",
                RailClass::NonCustodialFinal,
                vec!["USDC".to_string()],
            )
            .failing(),
        ) as Arc<dyn PaymentRail>,
    );

    let token = "test-token-validate-offline";
    let router = app(registry, SidecarToken::new(token));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // charge fails on this rail...
    let charge = client
        .post(format!("{base}/v1/rails/offline/charge"))
        .bearer_auth(token)
        .json(&json!({
            "amount_minor": 100,
            "currency": "USDC",
            "destination": "offline:wallet:alice",
            "reference": "sidecar-offline-1",
        }))
        .send()
        .await
        .expect("charge request");
    assert_eq!(charge.status(), 502, "this rail is configured to fail");

    // ...and validate-destination still answers, because it never asked
    // anything of the network.
    let verdict: serde_json::Value = client
        .post(format!("{base}/v1/rails/offline/validate-destination"))
        .bearer_auth(token)
        .json(&json!({ "destination": "offline:wallet:alice" }))
        .send()
        .await
        .expect("validate-destination request")
        .json()
        .await
        .expect("verdict body");
    assert_eq!(verdict["status"], json!("StructurallyValid"));
    assert_eq!(verdict["human_must_confirm"], json!(true));
}
