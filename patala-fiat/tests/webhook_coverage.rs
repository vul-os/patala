//! Every feature-gated adapter must answer [`PaymentRail::verify_webhook`] and
//! [`PaymentRail::validate_destination`], and answer both fail-closed.
//!
//! Those are the two methods on the trait with a *default* — which means they
//! are the two an adapter can silently fail to implement while still
//! compiling, and the two whose absence is invisible until a consumer hits it
//! in production. That shared failure mode, and the adapter list below, is why
//! both live in one file. (The name is historical: webhook coverage came
//! first. See `# validate_destination coverage` further down.)
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
//! 5. `every_offline_adapter_has_an_accepted_delivery` and the three tests
//!    after it — the ACCEPTING half, fleet-wide. Everything in 1–3 feeds each
//!    rail something it must reject, and for a long time the accepting half
//!    was six hand-written round trips out of twenty. That gap was measurable:
//!    three rails read an ABSENT settlement-status field as settled, and eight
//!    could emit an empty (or, for two, a constant `"0"`) `event_id` — and
//!    this file's 58 assertions caught none of them, because none of them ever
//!    looked at an event a rail had accepted. `accepted_case()` now signs one
//!    delivery per rail, with the very secrets that rail's `Adapter` entry is
//!    configured with, plus two mutations: the id field removed, and the
//!    settlement-status field removed. A rail whose verification is a live
//!    re-fetch has no offline delivery to sign and must be named in
//!    `REFETCH_RAILS`; it cannot be quietly absent from both.
//! 6. `every_compiled_adapter_overrides_validate_destination` and the four
//!    tests after it — a new adapter that forgets `validate_destination`
//!    inherits the trait default and fails here; and no adapter, on any input,
//!    may ever report `StructurallyValid`, which would claim a redirect URL or
//!    a buyer's email had been vetted as somewhere to send a customer's money.
//!
//! **Skips are loud.** The adapters are Cargo features; running without
//! `--all-features` compiles only some of them in. The harness prints exactly
//! which adapters were not verified and asserts a non-zero covered count, so
//! a run that quietly checks nothing is not possible. `scripts/check-features.sh`
//! separately fails the build if an adapter directory exists that this file
//! never names — a count assertion inside a feature-gated file cannot catch
//! that on its own.

#![allow(clippy::vec_init_then_push)]

use patala_core::{DestinationStatus, Error, PaymentRail, WebhookDelivery, WebhookStatus};

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

// ==========================================================================
// Fleet-wide POSITIVE round trips.
//
// Everything above this line feeds each rail a delivery it must REJECT. That
// half was fleet-wide; the accepting half was six hand-written round trips out
// of twenty, and the gap is measurable: three rails read an ABSENT settlement
// status as settled and eight could emit an empty (or, for two, a constant
// `"0"`) `event_id`, and this file — 58 assertions — noticed none of them,
// because it never once looked at an event a rail had ACCEPTED.
//
// So: one signed delivery per rail, signed with the very secrets that rail's
// `Adapter` entry above is configured with, plus two mutations of it, and
// three fleet-wide assertions over all three. A new adapter that does not
// appear here fails `every_offline_adapter_has_an_accepted_delivery` by name.
// ==========================================================================

/// A delivery a rail must ACCEPT, and two mutations of it that it must not
/// accept *silently*.
struct Accepted {
    /// Genuinely signed, describing this scheme's settled case — or, for a
    /// signature-only scheme, its "here is an object" case.
    good: WebhookDelivery,
    /// The same delivery, re-signed, with the field this scheme's event id
    /// comes from removed. The rail must either refuse it or still name it:
    /// `WebhookEvent::event_id` is documented "Never empty: a caller cannot
    /// suppress a duplicate it cannot name."
    unnameable: Option<WebhookDelivery>,
    /// The same delivery, re-signed, with the field the processor uses to
    /// report the OUTCOME removed. The rail must not read that as settled.
    /// `None` for a signature-only scheme (BTCPay, Coinbase Commerce, LNbits,
    /// OpenNode), which carries no settlement claim to remove and reports
    /// `Unconfirmed` either way.
    status_absent: Option<WebhookDelivery>,
}

/// The rails whose "signature check" IS an authenticated re-fetch of the
/// processor's own API, so no delivery can be verified offline and there is
/// nothing for this table to sign. Named rather than silently absent, so the
/// count assertion below can tell "cannot" from "nobody wrote one". Each has
/// its own wiremock-backed round trip in its rail.rs.
const REFETCH_RAILS: &[&str] = &["iyzico", "mercadopago", "mollie", "payfast", "paypal"];

#[cfg(any(
    feature = "btcpay",
    feature = "checkoutcom",
    feature = "coinbasecommerce",
    feature = "opennode",
    feature = "razorpay",
    feature = "stripe"
))]
fn hmac_sha256_hex_for(key: &[u8], msg: &[u8]) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(any(feature = "adyen", feature = "square", feature = "yoco"))]
fn hmac_sha256_b64_for(key: &[u8], msg: &[u8]) -> String {
    use base64::Engine as _;
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(msg);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(any(feature = "midtrans", feature = "payu"))]
fn sha512_hex_for(parts: &[&str]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha512::new();
    for p in parts {
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

#[cfg(feature = "adyen")]
fn adyen_delivery(psp: &str, merchant_ref: &str, success: &str) -> WebhookDelivery {
    let key = b"test-adyen-hmac-key-32-bytes!!!!";
    let signing = [
        psp,
        "",
        "",
        merchant_ref,
        "5000",
        "EUR",
        "AUTHORISATION",
        success,
    ]
    .join(":");
    let sig = hmac_sha256_b64_for(key, signing.as_bytes());
    let body = format!(
        r#"{{"live":"false","notificationItems":[{{"NotificationRequestItem":{{"additionalData":{{"hmacSignature":"{sig}"}},"amount":{{"value":5000,"currency":"EUR"}},"eventCode":"AUTHORISATION","merchantReference":"{merchant_ref}","pspReference":"{psp}","success":"{success}"}}}}]}}"#
    );
    WebhookDelivery::new(body.into_bytes(), NOW)
}

#[cfg(feature = "midtrans")]
fn midtrans_delivery(transaction_id: &str, status: &str) -> WebhookDelivery {
    let sig = sha512_hex_for(&["ord_1", "200", "10000.00", "midtrans-server-key"]);
    let body = format!(
        r#"{{"order_id":"ord_1","transaction_id":"{transaction_id}","transaction_status":"{status}","gross_amount":"10000.00","currency":"IDR","status_code":"200","signature_key":"{sig}"}}"#
    );
    WebhookDelivery::new(body.into_bytes(), NOW)
}

#[cfg(feature = "payu")]
fn payu_delivery(mihpayid: &str, status: &str) -> WebhookDelivery {
    // PayU's reverse hash: SALT|status|udf5..udf1|email|firstname|productinfo|amount|txnid|key
    let hash = sha512_hex_for(&[
        "payu-salt|",
        status,
        "|||||",
        "|a@b.com|Jane|Order txn_1|100.00|txn_1|payu-merchant-key",
    ]);
    let body = url_encode_pairs(&[
        ("status", status),
        ("txnid", "txn_1"),
        ("amount", "100.00"),
        ("productinfo", "Order txn_1"),
        ("firstname", "Jane"),
        ("email", "a@b.com"),
        ("mihpayid", mihpayid),
        ("hash", &hash),
    ]);
    WebhookDelivery::new(body.into_bytes(), NOW)
}

/// `application/x-www-form-urlencoded`, for the two rails that sign a form
/// body. Deliberately hand-rolled: the `url` crate is behind the `payu`
/// feature, and OpenNode's body must be buildable without it.
#[cfg(any(feature = "payu", feature = "opennode"))]
fn url_encode_pairs(pairs: &[(&str, &str)]) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", esc(k), esc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The accepted-delivery case for one adapter, or `None` for a re-fetch rail.
fn accepted_case(name: &str) -> Option<Accepted> {
    match name {
        #[cfg(feature = "adyen")]
        "adyen" => Some(Accepted {
            good: adyen_delivery("psp_1", "ord_1", "true"),
            unnameable: Some(adyen_delivery("", "ord_1", "false")),
            // Adyen's settlement claim is `success`; an absent one deserialises
            // to "" and must not read as "true".
            status_absent: Some(adyen_delivery("psp_1", "ord_1", "")),
        }),

        #[cfg(feature = "btcpay")]
        "btcpay" => {
            let deliver = |invoice: &str| {
                let body = format!(
                    r#"{{"type":"InvoiceSettled","invoiceId":"{invoice}","storeId":"store1"}}"#
                )
                .into_bytes();
                let sig = format!(
                    "sha256={}",
                    hmac_sha256_hex_for(b"btcpay-webhook-secret", &body)
                );
                WebhookDelivery::new(body, NOW).with_header("BTCPay-Sig", sig)
            };
            Some(Accepted {
                good: deliver("inv_wh"),
                unnameable: Some(deliver("")),
                status_absent: None,
            })
        }

        #[cfg(feature = "checkoutcom")]
        "checkoutcom" => {
            let deliver = |id: &str, status_field: &str| {
                let body = format!(
                    r#"{{"id":"{id}","type":"payment_captured","data":{{"id":"pay_1"{status_field},"amount":5000,"currency":"USD","reference":"ord_1"}}}}"#
                )
                .into_bytes();
                let sig = hmac_sha256_hex_for(b"cko-webhook-secret", &body);
                WebhookDelivery::new(body, NOW).with_header("Cko-Signature", sig)
            };
            Some(Accepted {
                good: deliver("evt_1", r#","status":"Captured""#),
                unnameable: Some(deliver("", r#","status":"Captured""#)),
                status_absent: Some(deliver("evt_1", "")),
            })
        }

        #[cfg(feature = "coinbasecommerce")]
        "coinbasecommerce" => {
            let deliver = |charge: &str| {
                let body = format!(
                    r#"{{"event":{{"type":"charge:confirmed","data":{{"id":"{charge}"}}}}}}"#
                )
                .into_bytes();
                let sig = hmac_sha256_hex_for(b"cc-webhook-secret", &body);
                WebhookDelivery::new(body, NOW).with_header("X-CC-Webhook-Signature", sig)
            };
            Some(Accepted {
                good: deliver("charge_1"),
                unnameable: Some(deliver("")),
                status_absent: None,
            })
        }

        #[cfg(feature = "flutterwave")]
        "flutterwave" => {
            let deliver = |id_field: &str, status: &str| {
                let body = format!(
                    r#"{{"event":"charge.completed","data":{{{id_field}"tx_ref":"ord_1","amount":100,"currency":"NGN","status":"{status}"}}}}"#
                )
                .into_bytes();
                WebhookDelivery::new(body, NOW).with_header("verif-hash", "flw-webhook-hash")
            };
            Some(Accepted {
                good: deliver(r#""id":9,"#, "successful"),
                // `id` is an i64 with a serde default, so an absent one is not
                // "empty" -- it is the CONSTANT "0" that every such delivery
                // shares. Caught by the same assertion.
                unnameable: Some(deliver("", "failed")),
                status_absent: Some(deliver(r#""id":9,"#, "")),
            })
        }

        #[cfg(feature = "lnbits")]
        "lnbits" => {
            let deliver = |hash: &str| {
                WebhookDelivery::new(format!(r#"{{"payment_hash":"{hash}"}}"#).into_bytes(), NOW)
                    .with_query_param("secret", "lnbits-webhook-secret")
            };
            Some(Accepted {
                good: deliver("hash123"),
                unnameable: Some(deliver("")),
                status_absent: None,
            })
        }

        #[cfg(feature = "midtrans")]
        "midtrans" => Some(Accepted {
            good: midtrans_delivery("txn_1", "settlement"),
            unnameable: Some(midtrans_delivery("", "deny")),
            status_absent: Some(midtrans_delivery("txn_1", "")),
        }),

        #[cfg(feature = "opennode")]
        "opennode" => {
            let deliver = |charge: &str| {
                let body = url_encode_pairs(&[
                    ("id", charge),
                    (
                        "hashed_order",
                        &hmac_sha256_hex_for(b"opennode-api-key", charge.as_bytes()),
                    ),
                ]);
                WebhookDelivery::new(body.into_bytes(), NOW)
            };
            Some(Accepted {
                good: deliver("charge_1"),
                unnameable: Some(deliver("")),
                status_absent: None,
            })
        }

        #[cfg(feature = "paystack")]
        "paystack" => {
            let deliver = |id_field: &str, status: &str| {
                let body = format!(
                    r#"{{"event":"charge.success","data":{{{id_field}"status":"{status}","reference":"ord_1","amount":5000,"currency":"NGN"}}}}"#
                )
                .into_bytes();
                let sig = {
                    use hmac::Mac;
                    let mut mac =
                        hmac::Hmac::<sha2::Sha512>::new_from_slice(b"sk_test_paystack").unwrap();
                    mac.update(&body);
                    hex::encode(mac.finalize().into_bytes())
                };
                WebhookDelivery::new(body, NOW).with_header("X-Paystack-Signature", sig)
            };
            Some(Accepted {
                good: deliver(r#""id":555,"#, "success"),
                unnameable: Some(deliver("", "success")),
                // Paystack refuses a charge.success whose data.status
                // disagrees, including an absent one, so this doubles as the
                // absent-status case.
                status_absent: Some(deliver(r#""id":555,"#, "")),
            })
        }

        #[cfg(feature = "payu")]
        "payu" => Some(Accepted {
            good: payu_delivery("mihpay123", "success"),
            unnameable: Some(payu_delivery("", "failure")),
            status_absent: Some(payu_delivery("mihpay123", "")),
        }),

        #[cfg(feature = "razorpay")]
        "razorpay" => {
            let deliver = |id: &str, status_field: &str| {
                let body = format!(
                    r#"{{"event":"payment.captured","payload":{{"payment":{{"entity":{{"id":"{id}","order_id":"order_1","amount":5000,"currency":"INR"{status_field},"created_at":1753000000}}}}}}}}"#
                )
                .into_bytes();
                let sig = hmac_sha256_hex_for(b"rzp-webhook-secret", &body);
                WebhookDelivery::new(body, NOW).with_header("X-Razorpay-Signature", sig)
            };
            Some(Accepted {
                good: deliver("pay_1", r#","status":"captured""#),
                unnameable: Some(deliver("", r#","status":"authorized""#)),
                status_absent: Some(deliver("pay_1", "")),
            })
        }

        #[cfg(feature = "square")]
        "square" => {
            let deliver = |event_id: &str, status: &str| {
                let body = format!(
                    r#"{{"event_id":"{event_id}","type":"payment.updated","data":{{"object":{{"payment":{{"id":"sqpay_1","status":"{status}","reference_id":"ord_1","amount_money":{{"amount":5000,"currency":"USD"}}}}}}}}}}"#
                )
                .into_bytes();
                let mut signed = b"https://example.com/webhooks/square".to_vec();
                signed.extend_from_slice(&body);
                let sig = hmac_sha256_b64_for(b"square-signature-key", &signed);
                WebhookDelivery::new(body, NOW).with_header("x-square-hmacsha256-signature", sig)
            };
            Some(Accepted {
                good: deliver("evt_1", "COMPLETED"),
                unnameable: Some(deliver("", "COMPLETED")),
                status_absent: Some(deliver("evt_1", "")),
            })
        }

        #[cfg(feature = "stripe")]
        "stripe" => {
            let deliver = |id: &str, status_field: &str| {
                let body = format!(
                    r#"{{"id":"{id}","type":"checkout.session.completed","data":{{"object":{{"id":"cs_test_1"{status_field},"amount_total":5000,"currency":"usd","client_reference_id":"ord_1"}}}}}}"#
                )
                .into_bytes();
                let mut signed = format!("{NOW}.").into_bytes();
                signed.extend_from_slice(&body);
                let sig = format!(
                    "t={NOW},v1={}",
                    hmac_sha256_hex_for(b"whsec_fake_secret_for_unit_tests", &signed)
                );
                WebhookDelivery::new(body, NOW).with_header("Stripe-Signature", sig)
            };
            Some(Accepted {
                good: deliver("evt_1", r#","payment_status":"paid""#),
                unnameable: Some(deliver("", r#","payment_status":"paid""#)),
                status_absent: Some(deliver("evt_1", "")),
            })
        }

        #[cfg(feature = "xendit")]
        "xendit" => {
            let deliver = |id: &str, status: &str| {
                let body = format!(
                    r#"{{"id":"{id}","external_id":"ord_1","status":"{status}","amount":10000,"paid_amount":10000,"currency":"IDR"}}"#
                )
                .into_bytes();
                WebhookDelivery::new(body, NOW)
                    .with_header("x-callback-token", "xendit-callback-token")
            };
            Some(Accepted {
                good: deliver("inv_9", "PAID"),
                unnameable: Some(deliver("", "PAID")),
                status_absent: Some(deliver("inv_9", "")),
            })
        }

        #[cfg(feature = "yoco")]
        "yoco" => {
            let deliver = |checkout: &str, status_field: &str| {
                let body = format!(
                    r#"{{"type":"payment.succeeded","payload":{{"id":"{checkout}"{status_field},"amount":1000,"currency":"ZAR"}}}}"#
                )
                .into_bytes();
                let mut signed = format!("msg_1.{NOW}.").into_bytes();
                signed.extend_from_slice(&body);
                let sig = format!(
                    "v1,{}",
                    hmac_sha256_b64_for(b"yoco-raw-webhook-key", &signed)
                );
                WebhookDelivery::new(body, NOW)
                    .with_header("webhook-id", "msg_1")
                    .with_header("webhook-timestamp", NOW.to_string())
                    .with_header("webhook-signature", sig)
            };
            Some(Accepted {
                good: deliver("chk_abc", r#","status":"completed""#),
                unnameable: Some(deliver("", r#","status":"completed""#)),
                status_absent: Some(deliver("chk_abc", "")),
            })
        }

        _ => None,
    }
}

/// Every adapter that CAN be verified offline has a signed delivery in the
/// table above. A new adapter is either in that table or in `REFETCH_RAILS`,
/// deliberately; it cannot be quietly absent from both.
#[test]
fn every_offline_adapter_has_an_accepted_delivery() {
    let compiled = covered_or_loudly_skip("every_offline_adapter_has_an_accepted_delivery");
    let missing: Vec<&str> = compiled
        .iter()
        .map(|a| a.name)
        .filter(|n| !REFETCH_RAILS.contains(n) && accepted_case(n).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "no signed delivery in accepted_case() for: {}. Every rail whose \
         verification is offline must have one, or the three assertions below \
         silently skip it -- which is how three rails came to read an absent \
         settlement status as settled. If verification genuinely needs a live \
         re-fetch, add the rail to REFETCH_RAILS and give it a wiremock round \
         trip in its own rail.rs.",
        missing.join(", ")
    );
    println!(
        "every_offline_adapter_has_an_accepted_delivery: {} offline rails signed, \
         {} re-fetch rails excluded by name.",
        compiled.len()
            - compiled
                .iter()
                .filter(|a| REFETCH_RAILS.contains(&a.name))
                .count(),
        compiled
            .iter()
            .filter(|a| REFETCH_RAILS.contains(&a.name))
            .count()
    );
}

#[tokio::test]
async fn every_accepted_event_is_nameable_and_carries_money_only_when_settled() {
    let compiled = covered_or_loudly_skip(
        "every_accepted_event_is_nameable_and_carries_money_only_when_settled",
    );
    let mut checked = 0usize;
    for adapter in &compiled {
        let Some(case) = accepted_case(adapter.name) else {
            continue;
        };
        checked += 1;
        let event = adapter
            .rail
            .verify_webhook(&case.good)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "{}: rejected a delivery signed with its own configured secret: {e}",
                    adapter.name
                )
            });
        assert_eq!(
            event.rail_id, adapter.name,
            "{}: the event names a different rail",
            adapter.name
        );
        assert!(
            !event.event_id.is_empty(),
            "{}: WebhookEvent::event_id is empty -- 'a caller cannot suppress a \
             duplicate it cannot name'",
            adapter.name
        );
        if event.status != WebhookStatus::Settled {
            assert_eq!(
                event.amount_minor, 0,
                "{}: {:?} carries amount_minor {} -- money is reported only for \
                 Settled",
                adapter.name, event.status, event.amount_minor
            );
            assert!(
                event.currency.is_empty(),
                "{}: {:?} carries currency {:?}",
                adapter.name,
                event.status,
                event.currency
            );
        }
    }
    assert!(
        checked > 0,
        "no accepted delivery ran, so this verified NOTHING"
    );
    println!("accepted round trips: {checked} rails.");
}

#[tokio::test]
async fn no_compiled_adapter_emits_an_event_it_cannot_name() {
    let compiled = covered_or_loudly_skip("no_compiled_adapter_emits_an_event_it_cannot_name");
    let mut checked = 0usize;
    for adapter in &compiled {
        let Some(delivery) = accepted_case(adapter.name).and_then(|c| c.unnameable) else {
            continue;
        };
        checked += 1;
        match adapter.rail.verify_webhook(&delivery).await {
            Err(_) => {}
            Ok(event) => {
                assert!(
                    !event.event_id.is_empty(),
                    "{}: a correctly signed delivery with no id reached Ok with an \
                     EMPTY event_id -- 'a caller cannot suppress a duplicate it \
                     cannot name'",
                    adapter.name
                );
                assert_ne!(
                    event.event_id, "0",
                    "{}: a correctly signed delivery with no id was named \"0\" -- \
                     an absent integer id collapses every distinct delivery onto \
                     the same key, so deduplicating on it discards them all but \
                     the first",
                    adapter.name
                );
            }
        }
    }
    assert!(
        checked > 0,
        "no unnameable delivery ran, so this verified NOTHING"
    );
    println!("unnameable deliveries: {checked} rails.");
}

#[tokio::test]
async fn no_compiled_adapter_reads_an_absent_status_as_settled() {
    let compiled = covered_or_loudly_skip("no_compiled_adapter_reads_an_absent_status_as_settled");
    let mut checked = 0usize;
    for adapter in &compiled {
        let Some(delivery) = accepted_case(adapter.name).and_then(|c| c.status_absent) else {
            continue;
        };
        checked += 1;
        if let Ok(event) = adapter.rail.verify_webhook(&delivery).await {
            assert_ne!(
                event.status,
                WebhookStatus::Settled,
                "{}: a correctly signed delivery whose settlement-status field is \
                 ABSENT reported Settled for {} {} -- the event type says what the \
                 processor called this delivery, the status field says what the \
                 money did, and only the second may answer the second question",
                adapter.name,
                event.amount_minor,
                event.currency
            );
        }
    }
    assert!(
        checked > 0,
        "no absent-status delivery ran, so this verified NOTHING"
    );
    println!("absent-status deliveries: {checked} rails.");
}

// --------------------------------------------------------------------------
// Full positive round-trips, one per distinct plumbing shape. These are what
// prove a consumer receives the WebhookEvent it should — not just that the
// call is wired up.
// --------------------------------------------------------------------------

/// Gated on exactly its two callers below. It used to name four features,
/// including `paystack` (which signs with SHA-512) and `square` (base64) —
/// neither of which calls this — so `cargo clippy --features paystack` and
/// `--features square` failed on `-D dead-code` alone.
#[cfg(any(feature = "stripe", feature = "coinbasecommerce"))]
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

// ── validate_destination coverage ────────────────────────────────────────────
//
// The second trait method a rail can silently fail to implement. `PaymentRail`
// gives `validate_destination` a default that answers `Unknown` for anything
// non-empty, which is the right default (it never blesses a token no one
// checked) but the wrong answer for a rail that knows what its own
// `destination` field is. These tests are what stops a new adapter shipping
// with that default, and — more importantly — what stops one ever claiming
// `StructurallyValid`, a status that means "a well-formed address for the
// network this rail pays on" and that no custodial fiat rail can truthfully
// report, because its `destination` is not an address at all.

/// What a given rail's `destination` actually is. Mirrors the table in
/// `patala_fiat::destination`'s module docs; a rail wired to the wrong helper
/// fails the assertions below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DestShape {
    /// The post-checkout URL the buyer's browser returns to.
    RedirectUrl,
    /// The buyer's email address.
    BuyerEmail,
    /// Nothing — the rail never reads the field.
    Ignored,
}

/// Deliberately exhaustive with a panicking fallback rather than a `_ =>` arm:
/// a new adapter must be classified here on purpose. Falling through to some
/// default would let it ship with an unexamined destination contract, which is
/// exactly the failure this file exists to prevent.
fn dest_shape(name: &str) -> DestShape {
    match name {
        "adyen" | "checkoutcom" | "iyzico" | "mercadopago" | "mollie" | "payfast" | "paypal"
        | "square" | "stripe" | "xendit" | "yoco" => DestShape::RedirectUrl,
        "flutterwave" | "midtrans" | "paystack" | "payu" => DestShape::BuyerEmail,
        "btcpay" | "coinbasecommerce" | "lnbits" | "opennode" | "razorpay" => DestShape::Ignored,
        other => panic!(
            "adapter {other:?} has no entry in dest_shape(). Decide what its \
             PayRequest::destination actually is (a redirect URL, the buyer's email, or \
             nothing), wire validate_destination to the matching patala_fiat::destination \
             helper, and add it here."
        ),
    }
}

/// One real address per chain — the cross-rail pastes these checks exist for.
const FOREIGN_ADDRESSES: &[(&str, &str)] = &[
    ("6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs", "Solana"),
    (
        "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7",
        "Stellar",
    ),
    ("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "Ethereum"),
    ("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", "Bitcoin"),
];

#[test]
fn every_compiled_adapter_overrides_validate_destination() {
    let compiled = covered_or_loudly_skip("every_compiled_adapter_overrides_validate_destination");
    for adapter in &compiled {
        let shape = dest_shape(adapter.name);
        match shape {
            // The trait default answers `Unknown` for any non-empty string and
            // can never produce `WrongNetwork`, so this positively proves the
            // rail implemented the method rather than inheriting it.
            DestShape::RedirectUrl | DestShape::BuyerEmail => {
                let v = adapter.rail.validate_destination(FOREIGN_ADDRESSES[0].0);
                assert_eq!(
                    v.status,
                    DestinationStatus::WrongNetwork,
                    "{} inherits PaymentRail::validate_destination's default instead of saying \
                     what its own `destination` field is — wire it to the matching \
                     patala_fiat::destination helper.",
                    adapter.name
                );
            }
            // An ignoring rail correctly answers `Unknown` like the default
            // does, so it is identified by its reason instead.
            DestShape::Ignored => {
                let v = adapter.rail.validate_destination("unused-placeholder");
                assert_eq!(v.status, DestinationStatus::Unknown, "{}", adapter.name);
                assert!(
                    v.reason.contains("never reads"),
                    "{} inherits the trait default's generic reason instead of saying that it \
                     never reads `destination`: {}",
                    adapter.name,
                    v.reason
                );
            }
        }
        // A verdict must name the rail that formed it: `mock`'s opinion of a
        // Stripe token is worth nothing, and neither is Stripe's of a Solana
        // address.
        assert_eq!(
            adapter.rail.validate_destination("anything").rail_id,
            adapter.name,
            "a verdict must say whose opinion it is"
        );
    }
}

#[test]
fn no_compiled_adapter_ever_claims_a_destination_is_structurally_valid() {
    // The single most important property here. `StructurallyValid` means "a
    // well-formed address for the network this rail pays on"; a custodial
    // fiat rail has no such network and no such address, so there is no input
    // for which that status is a true statement.
    let compiled = covered_or_loudly_skip(
        "no_compiled_adapter_ever_claims_a_destination_is_structurally_valid",
    );
    let inputs = [
        "https://shop.example.com/thanks",
        "buyer@example.com",
        "unused-placeholder",
        "cs_test_a1B2c3D4e5F6g7H8i9J0",
        FOREIGN_ADDRESSES[0].0,
        FOREIGN_ADDRESSES[1].0,
        "",
        "   ",
        "nonsense",
    ];
    for adapter in &compiled {
        for input in inputs {
            let v = adapter.rail.validate_destination(input);
            assert_ne!(
                v.status,
                DestinationStatus::StructurallyValid,
                "{} claimed {input:?} is a structurally valid destination. It cannot be: this \
                 rail's `destination` is not a payout address.",
                adapter.name
            );
            // Every verdict, on every rail, carries both of these.
            assert!(v.human_must_confirm, "{} / {input:?}", adapter.name);
            assert_eq!(
                v.exchange_deposit_caveat,
                patala_core::EXCHANGE_DEPOSIT_CAVEAT,
                "{} / {input:?}",
                adapter.name
            );
            assert!(
                !v.reason.trim().is_empty(),
                "{} / {input:?} — a refusal a UI cannot explain is barely a refusal",
                adapter.name
            );
        }
    }
}

#[test]
fn every_compiled_adapter_fails_closed_on_a_blank_destination() {
    let compiled =
        covered_or_loudly_skip("every_compiled_adapter_fails_closed_on_a_blank_destination");
    for adapter in &compiled {
        for blank in ["", " ", "\t\n"] {
            let v = adapter.rail.validate_destination(blank);
            assert_eq!(
                v.status,
                DestinationStatus::Malformed,
                "{} / {blank:?} — a blank destination is a refusal, never a shrug",
                adapter.name
            );
            assert!(v.is_refusal(), "{} / {blank:?}", adapter.name);
        }
    }
}

#[test]
fn adapters_that_read_destination_refuse_every_other_rails_address_by_name() {
    // The cross-rail case: the message has to name what was pasted. "Invalid"
    // sends someone back to re-type the same wrong thing.
    let compiled = covered_or_loudly_skip(
        "adapters_that_read_destination_refuse_every_other_rails_address_by_name",
    );
    for adapter in &compiled {
        if dest_shape(adapter.name) == DestShape::Ignored {
            continue;
        }
        for (address, chain) in FOREIGN_ADDRESSES {
            let v = adapter.rail.validate_destination(address);
            assert_eq!(
                v.status,
                DestinationStatus::WrongNetwork,
                "{} / {chain}",
                adapter.name
            );
            assert!(v.is_refusal(), "{} / {chain}", adapter.name);
            assert!(
                v.reason.contains(chain),
                "{} must name {chain} rather than say 'invalid': {}",
                adapter.name,
                v.reason
            );
        }
    }
}

#[test]
fn adapters_accept_the_format_their_own_processor_documents() {
    // The other direction: the format checks above must not be refusing the
    // thing the rail actually wants. A guard that fires on correct input is
    // worse than none.
    let compiled =
        covered_or_loudly_skip("adapters_accept_the_format_their_own_processor_documents");
    for adapter in &compiled {
        let good = match dest_shape(adapter.name) {
            DestShape::RedirectUrl => "https://shop.example.com/orders/1234/thanks",
            DestShape::BuyerEmail => "buyer@example.com",
            DestShape::Ignored => "unused-placeholder",
        };
        let v = adapter.rail.validate_destination(good);
        assert_eq!(
            v.status,
            DestinationStatus::Unknown,
            "{} refused {good:?}, which is exactly what its processor documents this field as: {}",
            adapter.name,
            v.reason
        );
        assert!(!v.is_refusal(), "{}", adapter.name);
    }
}
