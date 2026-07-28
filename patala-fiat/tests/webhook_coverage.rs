//! Every feature-gated adapter must answer [`PaymentRail::verify_webhook`],
//! and answer it fail-closed.
//!
//! This file exists because of a measured defect: webhook verification lived
//! in free functions outside the trait and outside the UniFFI surface, so no
//! consumer that dispatches through `dyn PaymentRail` could reach it — the
//! downstream symptom being a Go consumer whose `Webhook` returned "patala
//! has no webhook surface" unconditionally. The trait method closes that, and
//! these tests are what stops it silently re-opening:
//!
//! 1. `every_compiled_adapter_overrides_verify_webhook` — a new adapter that
//!    forgets to implement it inherits the trait default
//!    (`Unsupported("verify_webhook")`) and fails here, rather than shipping
//!    an unreachable `webhook.rs` all over again.
//! 2. `every_compiled_adapter_fails_closed_on_a_forged_delivery` — a garbage
//!    delivery is always an `Err`, never an `Ok` event a caller could mistake
//!    for an authenticated one.
//! 3. `documented_signature_headers_are_the_ones_actually_read` — for each
//!    header-carried scheme, the delivery is rejected as *missing* without
//!    the documented header and rejected as *invalid* with it. A rail wired
//!    to the wrong header name passes neither direction.
//! 4. `*_round_trip` — full positive paths for the distinct plumbing shapes
//!    (plain header, header + replay window, static-token header, config-URL
//!    plus header, query parameter), asserting the `WebhookEvent` a consumer
//!    actually receives.
//!
//! **Skips are loud.** The adapters are Cargo features; running without
//! `--all-features` compiles only some of them in. The harness prints exactly
//! which adapters were not verified and asserts a non-zero covered count, so
//! a run that quietly checks nothing is not possible. `scripts/check-features.sh`
//! separately fails the build if an adapter directory exists that this file
//! never names — a count assertion inside a feature-gated file cannot catch
//! that on its own.

#![allow(clippy::vec_init_then_push)]

use patala_core::{Error, PaymentRail, WebhookDelivery};

/// Fixed "now" for every replay-window check here — never the system clock.
const NOW: u64 = 1_700_000_000;

/// One adapter under test: its rail, and the headers its scheme documents.
struct Adapter {
    name: &'static str,
    rail: Box<dyn PaymentRail>,
    /// Header names the rail must read. Empty for a body-signed scheme
    /// (Adyen, Midtrans, PayU, PayFast) or a re-fetch scheme (Mollie).
    signature_headers: &'static [&'static str],
}

/// With no adapter feature enabled this has no caller — that build is the one
/// the harness reports as verifying NOTHING and fails on, so the allow is
/// about the shape of a fully feature-gated file, not a genuinely dead helper.
#[allow(dead_code)]
fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Every adapter compiled into this build. Each entry is `#[cfg]`-gated on
/// its own feature, so this list is exact for whatever feature set the run
/// used — and `covered_names()` reports it.
fn adapters() -> Vec<Adapter> {
    // `mut` is unused only in the degenerate no-adapter build; see `strings`.
    #[allow(unused_mut)]
    let mut out: Vec<Adapter> = Vec::new();

    #[cfg(feature = "adyen")]
    out.push(Adapter {
        name: "adyen",
        rail: Box::new(
            patala_fiat::AdyenRail::new(patala_fiat::AdyenConfig {
                api_key: "adyen-api-key".into(),
                merchant_account: "MerchantAcct".into(),
                hmac_key_hex: hex::encode(b"test-adyen-hmac-key-32-bytes!!!!"),
                api_base_url: "https://checkout-test.adyen.com".into(),
                requires_kyc: false,
                currencies: strings(&["EUR"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // Adyen signs inside the body (`additionalData.hmacSignature`).
        signature_headers: &[],
    });

    #[cfg(feature = "btcpay")]
    out.push(Adapter {
        name: "btcpay",
        rail: Box::new(
            patala_fiat::BTCPayRail::new(patala_fiat::BTCPayConfig {
                base_url: "https://btcpay.example".into(),
                api_key: "btcpay-api-key".into(),
                store_id: "store_1".into(),
                webhook_secret: "btcpay-webhook-secret".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                settlement_seconds: Some(600),
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["BTCPay-Sig"],
    });

    #[cfg(feature = "checkoutcom")]
    out.push(Adapter {
        name: "checkoutcom",
        rail: Box::new(
            patala_fiat::CheckoutComRail::new(patala_fiat::CheckoutComConfig {
                secret_key: "sk_test".into(),
                webhook_secret: "cko-webhook-secret".into(),
                api_base_url: "https://api.sandbox.checkout.com".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["Cko-Signature"],
    });

    #[cfg(feature = "coinbasecommerce")]
    out.push(Adapter {
        name: "coinbasecommerce",
        rail: Box::new(
            patala_fiat::CoinbaseCommerceRail::new(patala_fiat::CoinbaseCommerceConfig {
                api_key: "cc-api-key".into(),
                webhook_secret: "cc-webhook-secret".into(),
                base_url: "https://api.commerce.coinbase.com".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["X-CC-Webhook-Signature"],
    });

    #[cfg(feature = "flutterwave")]
    out.push(Adapter {
        name: "flutterwave",
        rail: Box::new(
            patala_fiat::FlutterwaveRail::new(patala_fiat::FlutterwaveConfig {
                secret_key: "FLWSECK_TEST".into(),
                webhook_hash: "flw-webhook-hash".into(),
                requires_kyc: false,
                currencies: strings(&["NGN"]),
                settlement_days: 1,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["verif-hash"],
    });

    #[cfg(feature = "iyzico")]
    out.push(Adapter {
        name: "iyzico",
        rail: Box::new(
            patala_fiat::IyzicoRail::new(patala_fiat::IyzicoConfig {
                api_key: "iyz-api-key".into(),
                secret_key: "iyz-secret".into(),
                base_url: "https://sandbox-api.iyzipay.com".into(),
                requires_kyc: false,
                currencies: strings(&["TRY"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // iyzico's callback carries no signature; Content-Type decides how
        // the token is extracted, and it is the only header read.
        signature_headers: &[],
    });

    #[cfg(feature = "lnbits")]
    out.push(Adapter {
        name: "lnbits",
        rail: Box::new(
            patala_fiat::LNbitsRail::new(patala_fiat::LNbitsConfig {
                base_url: "https://lnbits.example".into(),
                api_key: "lnbits-api-key".into(),
                webhook_secret: "lnbits-webhook-secret".into(),
                webhook_url: None,
                quote_ttl_secs: 300,
                requires_kyc: false,
                currencies: strings(&["SAT"]),
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // LNbits' compensating secret rides in the URL, not a header.
        signature_headers: &[],
    });

    #[cfg(feature = "mercadopago")]
    out.push(Adapter {
        name: "mercadopago",
        rail: Box::new(
            patala_fiat::MercadoPagoRail::new(patala_fiat::MercadoPagoConfig {
                access_token: "mp-access-token".into(),
                webhook_secret: "mp-webhook-secret".into(),
                requires_kyc: false,
                currencies: strings(&["ARS"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["x-signature", "x-request-id"],
    });

    #[cfg(feature = "midtrans")]
    out.push(Adapter {
        name: "midtrans",
        rail: Box::new(
            patala_fiat::MidtransRail::new(patala_fiat::MidtransConfig {
                server_key: "midtrans-server-key".into(),
                requires_kyc: false,
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // Midtrans's signature_key is a body field, not a header.
        signature_headers: &[],
    });

    #[cfg(feature = "mollie")]
    out.push(Adapter {
        name: "mollie",
        rail: Box::new(
            patala_fiat::MollieRail::new(patala_fiat::MollieConfig {
                api_key: "test_mollie_key".into(),
                webhook_url: "https://example.com/webhooks/mollie".into(),
                requires_kyc: false,
                currencies: strings(&["EUR"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // Mollie sends no signature at all; verification IS the re-fetch.
        signature_headers: &[],
    });

    #[cfg(feature = "opennode")]
    out.push(Adapter {
        name: "opennode",
        rail: Box::new(
            patala_fiat::OpenNodeRail::new(patala_fiat::OpenNodeConfig {
                api_key: "opennode-api-key".into(),
                base_url: "https://api.opennode.com".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // OpenNode signs a form field (`hashed_order`) in the body.
        signature_headers: &[],
    });

    #[cfg(feature = "payfast")]
    out.push(Adapter {
        name: "payfast",
        rail: Box::new(
            patala_fiat::PayFastRail::new(patala_fiat::PayFastConfig {
                merchant_id: "10000100".into(),
                merchant_key: "46f0cd694581a".into(),
                passphrase: "payfast-passphrase".into(),
                requires_kyc: false,
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // PayFast signs form fields in the ITN body.
        signature_headers: &[],
    });

    #[cfg(feature = "paypal")]
    out.push(Adapter {
        name: "paypal",
        rail: Box::new(
            patala_fiat::PayPalRail::new(patala_fiat::PayPalConfig {
                client_id: "paypal-client-id".into(),
                client_secret: "paypal-client-secret".into(),
                webhook_id: "WH-TEST".into(),
                base_url: "https://api-m.sandbox.paypal.com".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &[
            "PAYPAL-TRANSMISSION-ID",
            "PAYPAL-TRANSMISSION-TIME",
            "PAYPAL-TRANSMISSION-SIG",
            "PAYPAL-CERT-URL",
            "PAYPAL-AUTH-ALGO",
        ],
    });

    #[cfg(feature = "paystack")]
    out.push(Adapter {
        name: "paystack",
        rail: Box::new(
            patala_fiat::PaystackRail::new(patala_fiat::PaystackConfig {
                secret_key: "sk_test_paystack".into(),
                requires_kyc: false,
                currencies: strings(&["NGN"]),
                settlement_days: 1,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["X-Paystack-Signature"],
    });

    #[cfg(feature = "payu")]
    out.push(Adapter {
        name: "payu",
        rail: Box::new(
            patala_fiat::PayURail::new(patala_fiat::PayUConfig {
                merchant_key: "payu-merchant-key".into(),
                salt: "payu-salt".into(),
                requires_kyc: false,
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        // PayU's reverse response hash is a body field.
        signature_headers: &[],
    });

    #[cfg(feature = "razorpay")]
    out.push(Adapter {
        name: "razorpay",
        rail: Box::new(
            patala_fiat::RazorpayRail::new(patala_fiat::RazorpayConfig {
                key_id: "rzp_test".into(),
                key_secret: "rzp-secret".into(),
                webhook_secret: "rzp-webhook-secret".into(),
                requires_kyc: false,
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["X-Razorpay-Signature"],
    });

    #[cfg(feature = "square")]
    out.push(Adapter {
        name: "square",
        rail: Box::new(
            patala_fiat::SquareRail::new(patala_fiat::SquareConfig {
                access_token: "square-access-token".into(),
                webhook_signature_key: "square-signature-key".into(),
                location_id: "L1".into(),
                notification_url: "https://example.com/webhooks/square".into(),
                api_base_url: "https://connect.squareupsandbox.com".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["x-square-hmacsha256-signature"],
    });

    #[cfg(feature = "stripe")]
    out.push(Adapter {
        name: "stripe",
        rail: Box::new(
            patala_fiat::StripeRail::new(patala_fiat::StripeConfig {
                secret_key: "sk_test_stripe".into(),
                webhook_secret: "whsec_fake_secret_for_unit_tests".into(),
                requires_kyc: false,
                currencies: strings(&["USD"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["Stripe-Signature"],
    });

    #[cfg(feature = "xendit")]
    out.push(Adapter {
        name: "xendit",
        rail: Box::new(
            patala_fiat::XenditRail::new(patala_fiat::XenditConfig {
                secret_key: "xnd_development".into(),
                webhook_token: "xendit-callback-token".into(),
                requires_kyc: false,
                currencies: strings(&["IDR"]),
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["x-callback-token"],
    });

    #[cfg(feature = "yoco")]
    out.push(Adapter {
        name: "yoco",
        rail: Box::new(
            patala_fiat::YocoRail::new(patala_fiat::YocoConfig {
                secret_key: "sk_test_yoco".into(),
                webhook_secret: yoco_whsec(),
                requires_kyc: false,
                settlement_days: 2,
                timeout_secs: 5,
            })
            .unwrap(),
        ),
        signature_headers: &["webhook-id", "webhook-timestamp", "webhook-signature"],
    });

    out
}

/// Yoco (Svix) webhook secrets are `whsec_` + base64 of the raw key.
#[cfg(feature = "yoco")]
fn yoco_whsec() -> String {
    use base64::Engine as _;
    format!(
        "whsec_{}",
        base64::engine::general_purpose::STANDARD.encode(b"yoco-raw-webhook-key")
    )
}

/// Every adapter directory that exists in the source tree — the ground truth
/// a feature-gated list is measured against.
fn all_adapter_dirs() -> Vec<String> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut names: Vec<String> = std::fs::read_dir(&src)
        .expect("patala-fiat/src must be readable")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Print a loud, itemised skip line and return the compiled adapters. Never
/// returns an empty set: a run that verifies nothing is a failure, not a pass.
fn covered_or_loudly_skip(test: &str) -> Vec<Adapter> {
    let compiled = adapters();
    let names: Vec<&str> = compiled.iter().map(|a| a.name).collect();
    let all = all_adapter_dirs();
    let missing: Vec<&String> = all
        .iter()
        .filter(|d| !names.contains(&d.as_str()))
        .collect();

    if missing.is_empty() {
        println!(
            "{test}: verifying all {} patala-fiat adapters ({}).",
            names.len(),
            names.join(", ")
        );
    } else {
        println!(
            "{test}: SKIPPING {} of {} adapters — NOT VERIFIED: {}. \
             Their Cargo features are off in this build; \
             run `cargo test -p patala-fiat --all-features` to verify them.",
            missing.len(),
            all.len(),
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    assert!(
        !compiled.is_empty(),
        "no patala-fiat adapter was compiled in, so this test verified NOTHING. \
         A harness that passes by doing nothing is worse than no harness: build \
         with at least one adapter feature."
    );
    compiled
}

/// A delivery no rail can accept: not JSON, no signature anywhere.
fn forged() -> WebhookDelivery {
    WebhookDelivery::new(b"not-a-real-delivery".to_vec(), NOW)
}

#[tokio::test]
async fn every_compiled_adapter_overrides_verify_webhook() {
    let compiled = covered_or_loudly_skip("every_compiled_adapter_overrides_verify_webhook");
    for adapter in &compiled {
        let err = adapter
            .rail
            .verify_webhook(&forged())
            .await
            .expect_err("a forged delivery must never be accepted");
        assert!(
            !matches!(err, Error::Unsupported(_)),
            "{} inherits PaymentRail::verify_webhook's default (Unsupported). \
             Its webhook.rs is therefore unreachable through the trait, the \
             UniFFI binding and the sidecar — implement verify_webhook on the \
             rail (see PORTING.md §6b).",
            adapter.name
        );
    }
}

#[tokio::test]
async fn every_compiled_adapter_fails_closed_on_a_forged_delivery() {
    let compiled =
        covered_or_loudly_skip("every_compiled_adapter_fails_closed_on_a_forged_delivery");
    for adapter in &compiled {
        let outcome = adapter.rail.verify_webhook(&forged()).await;
        assert!(
            outcome.is_err(),
            "{} returned Ok for a forged delivery. Reaching Ok means \
             'this genuinely came from my processor' — it must be an Err.",
            adapter.name
        );
    }
}

#[tokio::test]
async fn documented_signature_headers_are_the_ones_actually_read() {
    let compiled =
        covered_or_loudly_skip("documented_signature_headers_are_the_ones_actually_read");
    let mut checked = 0usize;
    for adapter in &compiled {
        if adapter.signature_headers.is_empty() {
            continue;
        }
        checked += 1;

        // Without the headers, the rail must say something is MISSING.
        let without = adapter
            .rail
            .verify_webhook(&forged())
            .await
            .expect_err("forged delivery")
            .to_string();
        assert!(
            without.contains("missing"),
            "{}: with no signature headers the error was {without:?}, which does \
             not report anything missing",
            adapter.name
        );

        // With every documented header present (but bogus), the rail must
        // have moved past the missing-header check. If the rail read a
        // DIFFERENT header name than the one documented here, it would still
        // report the header missing and this fails.
        let mut delivery = forged();
        for h in adapter.signature_headers {
            delivery = delivery.with_header(h, "bogus-but-present");
        }
        let with = adapter
            .rail
            .verify_webhook(&delivery)
            .await
            .expect_err("still forged")
            .to_string();
        assert!(
            !with.contains("missing"),
            "{}: supplying {:?} still produced {with:?} — the rail is reading a \
             different header name than the one it documents",
            adapter.name,
            adapter.signature_headers
        );
    }
    assert!(
        checked > 0,
        "no header-carried scheme was compiled in, so this test verified NOTHING"
    );
    println!("documented_signature_headers: pinned {checked} header-carried schemes.");
}

// --------------------------------------------------------------------------
// Full positive round-trips, one per distinct plumbing shape. These are what
// prove a consumer receives the WebhookEvent it should — not just that the
// call is wired up.
// --------------------------------------------------------------------------

#[cfg(any(
    feature = "stripe",
    feature = "paystack",
    feature = "coinbasecommerce",
    feature = "square"
))]
fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(feature = "stripe")]
#[tokio::test]
async fn stripe_round_trip_through_the_trait() {
    let rail = patala_fiat::StripeRail::new(patala_fiat::StripeConfig {
        secret_key: "sk_test_stripe".into(),
        webhook_secret: "whsec_fake_secret_for_unit_tests".into(),
        requires_kyc: false,
        currencies: strings(&["USD"]),
        settlement_days: 2,
        timeout_secs: 5,
    })
    .unwrap();

    let body = br#"{"id":"evt_1","type":"checkout.session.completed","data":{"object":{"id":"cs_test_1","payment_status":"paid","amount_total":5000,"currency":"usd","client_reference_id":"ord_1"}}}"#.to_vec();
    let mut signed = format!("{NOW}.").into_bytes();
    signed.extend_from_slice(&body);
    let sig = format!(
        "t={NOW},v1={}",
        hmac_sha256_hex(b"whsec_fake_secret_for_unit_tests", &signed)
    );

    let delivery = WebhookDelivery::new(body.clone(), NOW).with_header("Stripe-Signature", &sig);
    let event = rail.verify_webhook(&delivery).await.unwrap();
    assert_eq!(event.rail_id, "stripe");
    assert_eq!(event.event_id, "evt_1");
    assert_eq!(event.reference, "ord_1");
    assert_eq!(event.status, patala_core::WebhookStatus::Settled);
    assert!(event.is_settled());
    assert_eq!(event.amount_minor, 5000);
    assert_eq!(event.currency, "USD");

    // The replay window is read off the delivery, not the system clock: the
    // same bytes an hour later must be rejected.
    let stale = WebhookDelivery::new(body, NOW + 3600).with_header("Stripe-Signature", sig);
    assert!(rail.verify_webhook(&stale).await.is_err());
}

#[cfg(feature = "paystack")]
#[tokio::test]
async fn paystack_round_trip_through_the_trait() {
    use hmac::Mac;

    let rail = patala_fiat::PaystackRail::new(patala_fiat::PaystackConfig {
        secret_key: "sk_test_paystack".into(),
        requires_kyc: false,
        currencies: strings(&["NGN"]),
        settlement_days: 1,
        timeout_secs: 5,
    })
    .unwrap();

    let body = br#"{"event":"charge.success","data":{"id":42,"status":"success","reference":"ord_1","amount":150000,"currency":"NGN"}}"#.to_vec();
    let mut mac = hmac::Hmac::<sha2::Sha512>::new_from_slice(b"sk_test_paystack").unwrap();
    mac.update(&body);
    let sig = hex::encode(mac.finalize().into_bytes());

    let delivery = WebhookDelivery::new(body, NOW).with_header("X-Paystack-Signature", sig);
    let event = rail.verify_webhook(&delivery).await.unwrap();
    assert_eq!(event.rail_id, "paystack");
    assert_eq!(event.reference, "ord_1");
    assert_eq!(event.status, patala_core::WebhookStatus::Settled);
    assert_eq!(event.amount_minor, 150000);
    assert_eq!(event.currency, "NGN");
}

#[cfg(feature = "xendit")]
#[tokio::test]
async fn xendit_static_token_round_trip_through_the_trait() {
    let rail = patala_fiat::XenditRail::new(patala_fiat::XenditConfig {
        secret_key: "xnd_development".into(),
        webhook_token: "xendit-callback-token".into(),
        requires_kyc: false,
        currencies: strings(&["IDR"]),
        settlement_days: 2,
        timeout_secs: 5,
    })
    .unwrap();

    let body =
        br#"{"id":"inv_1","external_id":"ord_1","status":"PAID","amount":250000,"currency":"IDR"}"#
            .to_vec();
    let delivery = WebhookDelivery::new(body.clone(), NOW)
        .with_header("x-callback-token", "xendit-callback-token");
    let event = rail.verify_webhook(&delivery).await.unwrap();
    assert_eq!(event.rail_id, "xendit");
    assert_eq!(event.reference, "ord_1");
    assert_eq!(event.status, patala_core::WebhookStatus::Settled);
    assert_eq!(event.currency, "IDR");

    // Wrong token, same body: rejected.
    let bad = WebhookDelivery::new(body, NOW).with_header("x-callback-token", "not-the-token");
    assert!(rail.verify_webhook(&bad).await.is_err());
}

#[cfg(feature = "coinbasecommerce")]
#[tokio::test]
async fn coinbasecommerce_signature_only_delivery_is_unconfirmed_not_settled() {
    let rail = patala_fiat::CoinbaseCommerceRail::new(patala_fiat::CoinbaseCommerceConfig {
        api_key: "cc-api-key".into(),
        webhook_secret: "cc-webhook-secret".into(),
        base_url: "https://api.commerce.coinbase.com".into(),
        requires_kyc: false,
        currencies: strings(&["USD"]),
        timeout_secs: 5,
    })
    .unwrap();

    let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"charge_1"}}}"#.to_vec();
    let sig = hmac_sha256_hex(b"cc-webhook-secret", &body);
    let delivery = WebhookDelivery::new(body, NOW).with_header("X-CC-Webhook-Signature", sig);

    let event = rail.verify_webhook(&delivery).await.unwrap();
    // The delivery is authentic AND says nothing about money. Reporting it
    // as `NotSettled` would be a different, false claim — see WebhookStatus.
    assert_eq!(event.status, patala_core::WebhookStatus::Unconfirmed);
    assert!(!event.is_settled());
    assert_eq!(event.object_id, "charge_1");
    assert_eq!(event.amount_minor, 0);
    assert!(event.currency.is_empty());
    assert!(!event.event_id.is_empty(), "dedup needs a stable event id");
}

#[cfg(feature = "square")]
#[tokio::test]
async fn square_signs_over_the_configured_notification_url() {
    use base64::Engine as _;
    use hmac::Mac;

    const URL: &str = "https://example.com/webhooks/square";
    let rail = patala_fiat::SquareRail::new(patala_fiat::SquareConfig {
        access_token: "square-access-token".into(),
        webhook_signature_key: "square-signature-key".into(),
        location_id: "L1".into(),
        notification_url: URL.into(),
        api_base_url: "https://connect.squareupsandbox.com".into(),
        requires_kyc: false,
        currencies: strings(&["USD"]),
        settlement_days: 2,
        timeout_secs: 5,
    })
    .unwrap();

    let body = br#"{"event_id":"evt_1","type":"payment.updated","data":{"object":{"payment":{"id":"pay_1","status":"COMPLETED","reference_id":"ord_1","amount_money":{"amount":5000,"currency":"USD"}}}}}"#.to_vec();
    let mut signed = URL.as_bytes().to_vec();
    signed.extend_from_slice(&body);
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"square-signature-key").unwrap();
    mac.update(&signed);
    let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    let delivery =
        WebhookDelivery::new(body, NOW).with_header("x-square-hmacsha256-signature", sig);
    let event = rail.verify_webhook(&delivery).await.unwrap();
    assert_eq!(event.rail_id, "square");
    assert_eq!(event.reference, "ord_1");
    assert_eq!(event.object_id, "pay_1");
    assert_eq!(event.status, patala_core::WebhookStatus::Settled);
    assert_eq!(event.amount_minor, 5000);
}

#[cfg(feature = "lnbits")]
#[tokio::test]
async fn lnbits_reads_its_secret_from_the_query_string_not_a_header() {
    let rail = patala_fiat::LNbitsRail::new(patala_fiat::LNbitsConfig {
        base_url: "https://lnbits.example".into(),
        api_key: "lnbits-api-key".into(),
        webhook_secret: "lnbits-webhook-secret".into(),
        webhook_url: None,
        quote_ttl_secs: 300,
        requires_kyc: false,
        currencies: strings(&["SAT"]),
        timeout_secs: 5,
    })
    .unwrap();

    let body = br#"{"payment_hash":"ph_1"}"#.to_vec();

    // Right secret, right place.
    let ok =
        WebhookDelivery::new(body.clone(), NOW).with_query_param("secret", "lnbits-webhook-secret");
    let event = rail.verify_webhook(&ok).await.unwrap();
    assert_eq!(event.status, patala_core::WebhookStatus::Unconfirmed);
    assert_eq!(event.object_id, "ph_1");

    // Right secret, wrong place: a header is not the URL, and this must fail
    // closed rather than quietly accept.
    let as_header =
        WebhookDelivery::new(body.clone(), NOW).with_header("secret", "lnbits-webhook-secret");
    assert!(rail.verify_webhook(&as_header).await.is_err());

    // Wrong secret.
    let wrong = WebhookDelivery::new(body, NOW).with_query_param("secret", "nope");
    assert!(rail.verify_webhook(&wrong).await.is_err());
}
