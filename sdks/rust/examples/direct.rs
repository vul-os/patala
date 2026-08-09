//! patala from Rust, **direct** — in-process, no FFI, no shared library.
//!
//! There is nothing to load and nothing to locate. `patala-core` is the core;
//! a Rust host does not reach it through a binding, it *is* the host. Every
//! other language in `sdks/` is trying to get back to what this file has for
//! free: real types, real errors, real ownership.
//!
//! Run it:
//!
//! ```text
//! cd sdks/rust && cargo run --example direct
//! ```
//!
//! Everything here drives `MockRail` — deterministic, offline, no credentials.
//! This is a payments library; an example that moves real value is not an
//! example.

use std::time::Instant;

use patala_core::{
    DestinationStatus, Error, FailoverRail, MockRail, PayRequest, PaymentRail, RailClass,
    Settlement,
};

/// Money is integer minor units plus a currency string. Never a float, and
/// never anywhere in this file — including in the printing.
fn minor(amount_minor: u64, currency: &str) -> String {
    format!(
        "{}.{:02} {}",
        amount_minor / 100,
        amount_minor % 100,
        currency
    )
}

#[tokio::main]
async fn main() {
    println!("patala direct — in-process, no FFI, no shared library");

    // ---------------------------------------------------------------- rail
    // No handle, no `open`, no version probe against a library that may be
    // stale on the load path: the rail is a value, and the compiler already
    // checked that this crate and patala-core agree about its shape.
    let rail = MockRail::new(
        "mock",
        RailClass::NonCustodialFinal,
        vec!["USDC".into(), "USD".into()],
    );
    println!("rail:      {}", rail.id());

    // ------------------------------------------------------- capabilities
    // The settlement class is a `RailClass`, not a bool and not a string, so
    // this `match` is exhaustive and a new class would be a compile error
    // here rather than a wrong UX in production. That is the thing direct
    // mode buys that no JSON boundary can give back.
    let caps = rail.capabilities();
    let ux = match caps.class {
        RailClass::CustodialReversible => "card form, refundable pending state",
        RailClass::NonCustodialFinal => "wallet address, signed final receipt",
    };
    println!(
        "caps:      {:?} / {ux}\n           settlement={:?} holds_funds={} reversible={} currencies={:?}",
        caps.class, caps.settlement, caps.holds_funds, caps.reversible, caps.currencies,
    );
    assert!(!caps.holds_funds, "patala itself never holds funds");
    assert_eq!(caps.settlement, Settlement::Instant);

    // -------------------------------------------------------------- quote
    let req = PayRequest {
        amount_minor: 1250, // 12.50 USDC
        currency: "USDC".into(),
        destination: "mock:wallet:alice".into(),
        reference: "order-1".into(),
    };

    let quote = rail.quote(&req).await.expect("quote");
    println!(
        "quote:     {} + {} fee = {} (expires {}s)",
        minor(quote.amount_minor, &quote.currency),
        minor(quote.fee_minor, &quote.currency),
        minor(quote.total_minor, &quote.currency),
        quote.expires_at_unix.saturating_sub(now_unix()),
    );

    // ------------------------------------------------- charge -> verify
    let t0 = Instant::now();
    let receipt = rail.charge(&req).await.expect("charge");
    let charged_in = t0.elapsed();
    println!(
        "charge:    {} ref={} rail={} proof={}B  [{charged_in:?}]",
        minor(receipt.amount_minor, &receipt.currency),
        receipt.reference,
        receipt.rail_id,
        receipt.proof.len(),
    );

    // `charge` having returned `Ok` is NOT the entitlement. The receipt is,
    // and `verify` is what re-derives that it still holds.
    let valid = rail.verify(&receipt).await.expect("verify");
    println!("verify:    Ok({valid})  <- gate entitlement on exactly this");
    assert!(valid);

    // A tampered receipt is `Ok(false)`, not `Err`. The distinction is the
    // whole fail-closed contract: `Err` is "I could not check", which you may
    // retry; `Ok(false)` is "I checked, and it does not hold", which you must
    // not. In Rust these cannot be confused — they are different variants.
    let mut tampered = receipt.clone();
    tampered.amount_minor = 999_999;
    match rail.verify(&tampered).await {
        Ok(false) => println!("tampered:  Ok(false) — a refusal is DATA, not an error"),
        Ok(true) => panic!("a tampered receipt verified true"),
        Err(e) => panic!("tampering must not become an error: {e}"),
    }

    // ------------------------------------------------- destination check
    // Offline, pure, and it never returns a Result — "I cannot check this" is
    // a verdict, because a caller must handle it as carefully as a refusal.
    for dest in [
        "mock:wallet:alice",
        "stellar:wallet:alice",
        "not-an-address",
    ] {
        let v = rail.validate_destination(dest);
        println!(
            "dest:      {dest:<22} {:?}{}",
            v.status,
            if v.is_refusal() {
                "  (DO NOT SEND)"
            } else {
                ""
            },
        );
        // True on EVERY verdict, including StructurallyValid: patala does not
        // detect exchange-owned addresses and will not guess.
        assert!(v.human_must_confirm);
        assert!(!v.exchange_deposit_caveat.is_empty());
    }
    let ok = rail.validate_destination("mock:wallet:alice");
    assert_eq!(ok.status, DestinationStatus::StructurallyValid);

    // A rail whose destination is an opaque processor-side token — the fiat
    // shape — answers Unknown rather than erroring, and Unknown is not a
    // green light.
    let opaque = MockRail::new("mock", RailClass::NonCustodialFinal, vec!["USD".into()])
        .without_destination_checks();
    let u = opaque.validate_destination("cus_opaque_token");
    println!(
        "dest:      {:<22} {:?}  is_refusal={} — and not an approval either",
        "cus_opaque_token",
        u.status,
        u.is_refusal()
    );
    assert_eq!(u.status, DestinationStatus::Unknown);

    // ---------------------------------------------------- the error path
    // Typed errors, matched by variant, with the offending value in the
    // message. Nothing here is a string to grep.
    let eur = PayRequest {
        currency: "EUR".into(),
        ..req.clone()
    };
    match rail.charge(&eur).await {
        Err(Error::InvalidRequest(msg)) => println!("refused:   Error::InvalidRequest({msg:?})"),
        other => panic!("expected a refusal naming the currency, got {other:?}"),
    }

    // Paying a customer back on a final rail is not a reversal — it is a
    // second charge to an address the CUSTOMER supplies. The rail says so in
    // the type system rather than pretending.
    match rail.refund(&receipt).await {
        Err(Error::Unsupported(op)) => {
            println!("refund:    Error::Unsupported({op:?}) — see docs/compensating-payments.md")
        }
        other => panic!("a NonCustodialFinal rail must not reverse: {other:?}"),
    }

    // The mock has no processor, so it invents no webhook event.
    let delivery = patala_core::WebhookDelivery::new(b"{}".to_vec(), 1_700_000_000);
    match rail.verify_webhook(&delivery).await {
        Err(Error::Unsupported(op)) => println!("webhook:   Error::Unsupported({op:?})"),
        other => panic!("the mock must not invent an event: {other:?}"),
    }

    // ------------------------------------------------ what only Rust gets
    // `FailoverRail` composes `Box<dyn PaymentRail>`s and refuses to cross the
    // settlement-class boundary without an explicit opt-in. It is reachable
    // from Rust and from nowhere else: the C ABI and the sidecar both hand
    // out one rail at a time, so a caller in C or over HTTP has to rebuild
    // this — including the guard — by hand.
    let chain = FailoverRail::new(vec![
        Box::new(
            MockRail::new("primary", RailClass::NonCustodialFinal, vec!["USDC".into()]).failing(),
        ),
        Box::new(MockRail::new(
            "backup",
            RailClass::NonCustodialFinal,
            vec!["USDC".into()],
        )),
    ]);
    let r = chain.charge(&req).await.expect("failover");
    println!("failover:  primary failed -> settled on {:?}", r.rail_id);

    let crossing = FailoverRail::new(vec![
        Box::new(
            MockRail::new("primary", RailClass::NonCustodialFinal, vec!["USDC".into()]).failing(),
        ),
        Box::new(MockRail::new(
            "card",
            RailClass::CustodialReversible,
            vec!["USDC".into()],
        )),
    ]);
    match crossing.charge(&req).await {
        Err(Error::CrossClassFailover { from, to }) => {
            println!("guard:     refused {from:?} -> {to:?} — the payer was promised one of these")
        }
        other => panic!("the class boundary must not be crossed silently: {other:?}"),
    }

    println!("\nOK — offline, MockRail only, no value moved.");
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
