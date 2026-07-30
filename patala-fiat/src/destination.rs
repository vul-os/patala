//! Offline destination checks shared by every rail in this crate —
//! [`patala_core::PaymentRail::validate_destination`]'s implementation.
//!
//! # On a fiat rail, `destination` is not a payout address
//!
//! This is the whole point of this module, and the thing a caller is most
//! likely to get wrong. `patala_core::PayRequest::destination` is documented
//! as "a wallet address for a crypto rail, or an **opaque processor-side
//! destination token** for a fiat rail", and every rail here is the second
//! kind. Concretely, `destination` on these rails is one of exactly three
//! things, each documented in the rail's own module:
//!
//! | Shape | Rails | What the string actually is |
//! |---|---|---|
//! | [`redirect_url`] | adyen, checkoutcom, iyzico, mercadopago, mollie, payfast, paypal, square, stripe, xendit, yoco | The URL the **buyer's browser** returns to after the hosted checkout — Stripe's `success_url`, Adyen's `returnUrl`, Mollie's `redirectUrl`, Square's `redirect_url`. |
//! | [`buyer_email`] | flutterwave, midtrans, paystack, payu | The **buyer's email address**, which these processors require (or use) to open a transaction. |
//! | [`ignored`] | btcpay, coinbasecommerce, lnbits, manual, opennode, razorpay | Nothing. The rail never reads it; `PayRequest::validate()` merely requires it be non-empty. |
//!
//! **None of the three is a place money goes.** So the honest ceiling for a
//! verdict here is [`patala_core::DestinationStatus::Unknown`] — "a human must
//! decide" — and never `StructurallyValid`, which on a crypto rail means "this
//! is a well-formed address for the network this rail pays on". Claiming it
//! here would tell a caller that a `success_url` had been vetted as somewhere
//! to send a customer's money. It has not, and it is not.
//!
//! # Then why check anything at all?
//!
//! Because the failure this method exists to catch — a person pasting the
//! wrong thing into the field — is very much possible on these rails, and
//! catching it at the moment they type is the only cheap time to do it. A
//! wallet address in Stripe's `success_url` produces a charge Stripe rejects;
//! an address in Paystack's `email` produces a transaction that never reaches
//! the buyer. Both are decidable offline against a format the processor
//! documents, so both are refused here, by name — see
//! [`looks_like_a_wallet_address`], which exists purely so that refusal can
//! say *which* chain the pasted address belongs to rather than "invalid".
//!
//! What is **not** invented: nothing here claims to know whether a URL is
//! reachable, whether an email belongs to the buyer, whether the processor
//! will accept the token, or anything at all about the [`ignored`] rails'
//! destinations. The token's meaning belongs to the processor.
//!
//! # Giving a customer their money back on these rails
//!
//! Unlike the crypto rails, this is **not** a compensating payment to a
//! customer-supplied address. Every rail here is
//! `RailClass::CustodialReversible`, so the processor can reverse the original
//! payment: use [`patala_core::PaymentRail::refund`] on the rails that
//! implement it, which needs no destination at all because the money goes back
//! the way it came. The rails in this crate whose processor scheme has no
//! refund API return `Error::Unsupported("refund")` and say so; for those, the
//! refund happens in the processor's own dashboard. In neither case is
//! `destination` involved.
//!
//! # The purity contract
//!
//! Every function here is **pure and offline**: no network, no clock, no
//! filesystem, no global state, and no optional dependency — this module
//! compiles in the crate's default (empty) feature set, so the checks are
//! available in a build that links no HTTP client at all.

use patala_core::DestinationVerdict;

/// The base58 alphabet Solana and Bitcoin use.
const BASE58_ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Is every character in the RFC4648 base32 alphabet Stellar's StrKey uses?
/// Case-insensitive, matching the decoder `patala-stellar` defers to.
fn is_base32(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphabetic() || ('2'..='7').contains(&c))
}

/// A pasted **private key**, recognized by shape. Split out from
/// [`looks_like_a_wallet_address`] because it is not a wrong-field problem, it
/// is a disclosure — and it is one on a Stripe rail just as much as on a
/// Stellar one.
fn looks_like_a_secret_key(dest: &str) -> bool {
    // A Stellar secret seed: StrKey, 56 characters, version byte `S`.
    dest.len() == 56 && dest.starts_with(['S', 's']) && is_base32(dest)
}

/// Does this look like a blockchain wallet address, and if so, whose?
///
/// Recognition is by **shape alone** — length plus alphabet, no decoding and
/// no checksum — which is why every string it returns says "looks like". That
/// is all it needs to be: this is never used to accept anything, only to turn
/// a refusal that would have read "invalid" into one that reads "this looks
/// like a Solana address, and this field is a redirect URL". The second is
/// what stops the person doing it again.
///
/// The authoritative validators for these formats live in the rails that
/// actually pay on those networks — `patala_solana::destination` and
/// `patala_stellar::destination`. This crate deliberately does not depend on
/// either: it would put a chain SDK in the dependency graph of a fiat
/// processor adapter to improve an error message.
pub fn looks_like_a_wallet_address(dest: &str) -> Option<&'static str> {
    let len = dest.len();
    let all_base58 = |s: &str| s.chars().all(|c| BASE58_ALPHABET.contains(c));

    // Solana: 32 bytes of base58 is always 43 or 44 characters.
    if matches!(len, 43 | 44) && all_base58(dest) {
        return Some("a Solana wallet address");
    }

    // Stellar StrKey: 56 characters for the 32-byte payload types, 69 for a
    // muxed account.
    if is_base32(dest) {
        match (dest.as_bytes().first().map(u8::to_ascii_uppercase), len) {
            (Some(b'G'), 56) => return Some("a Stellar account address (G…)"),
            (Some(b'C'), 56) => return Some("a Stellar contract address (C…)"),
            (Some(b'M'), 69) => return Some("a Stellar muxed account address (M…)"),
            _ => {}
        }
    }

    // Hex-with-0x families.
    if let Some(hex) = dest.strip_prefix("0x").or_else(|| dest.strip_prefix("0X")) {
        if hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            match hex.len() {
                40 => return Some("an Ethereum/EVM address"),
                64 => return Some("a Sui or Aptos address"),
                _ => {}
            }
        }
    }

    // Bitcoin, both eras.
    if matches!(len, 26..=35)
        && (dest.starts_with('1') || dest.starts_with('3'))
        && all_base58(dest)
    {
        return Some("a legacy Bitcoin address");
    }
    if let Some(rest) = dest
        .strip_prefix("bc1")
        .or_else(|| dest.strip_prefix("tb1"))
        .or_else(|| dest.strip_prefix("bcrt1"))
    {
        if !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Some("a bech32 Bitcoin address (bc1…)");
        }
    }

    None
}

/// The checks every rail in this crate runs before its own format check: a
/// blank destination, a disclosed private key, and a wallet address pasted
/// into a field that is not one.
///
/// `field` names what this rail's `destination` actually is, in a phrase that
/// completes "this rail's destination is …".
fn common_refusals(rail_id: &str, dest: &str, field: &str) -> Option<DestinationVerdict> {
    if dest.trim().is_empty() {
        return Some(DestinationVerdict::malformed(
            rail_id,
            format!(
                "an empty destination is not usable on any rail; on the {rail_id} rail this field \
                 is {field}"
            ),
        ));
    }

    // Never repeats the value: a verdict is meant to be shown to a person and
    // is very likely to be logged on the way there.
    if looks_like_a_secret_key(dest) {
        return Some(DestinationVerdict::malformed(
            rail_id,
            "STOP — this is not a destination, it is a Stellar SECRET SEED (S…). You have just \
             pasted a PRIVATE KEY into a payment field. Treat it as disclosed: create a new \
             account, move everything the old key controls to it, and never use the old key \
             again. patala has not logged the value and this message does not repeat it, but \
             wherever you copied it from may have. Nothing on this rail ever wants a key or an \
             address of any kind.",
        ));
    }

    if let Some(what) = looks_like_a_wallet_address(dest) {
        return Some(DestinationVerdict::wrong_network(
            rail_id,
            format!(
                "this looks like {what}, and the {rail_id} rail does not pay to wallet addresses \
                 at all — it is a custodial fiat rail, and its `destination` is {field}. Nothing \
                 sent through this rail arrives at a blockchain address. If you meant to pay \
                 someone on-chain, use a crypto rail (patala-solana, patala-stellar); if you \
                 meant to give a customer their money back on THIS rail, that is `refund`, which \
                 sends it back the way it came and needs no destination."
            ),
        ));
    }

    None
}

/// The tail every fiat verdict ends with: what could not be established, and
/// why not.
fn processor_owns_this(rail_id: &str) -> String {
    format!(
        "Beyond that nothing more can be checked offline: what this string means is the \
         {rail_id} processor's to decide, not patala's, and this rail can neither accept nor \
         reject it on the processor's behalf. A human must confirm it."
    )
}

/// For rails whose `destination` is the **post-checkout redirect URL** the
/// buyer's browser returns to — Stripe's `success_url`/`cancel_url`, Adyen's
/// `returnUrl`, Checkout.com's `success_url`/`failure_url`/`cancel_url`,
/// Mollie's `redirectUrl`, PayPal's `return_url`/`cancel_url`, Square's
/// `redirect_url`, Xendit's `success_redirect_url`, MercadoPago's `back_urls`,
/// and the callback URLs iyzico, PayFast and Yoco require.
///
/// Every one of those processors documents this field as an absolute URL, so
/// that much is a real, citable format check and a string that is not one is a
/// [`patala_core::DestinationStatus::Malformed`] refusal. A string that *is*
/// one is still only [`patala_core::DestinationStatus::Unknown`]: a redirect
/// URL is not a payout address, and no offline check can say where money on
/// this rail ends up.
pub fn redirect_url(rail_id: &str, dest: &str) -> DestinationVerdict {
    const FIELD: &str = "the URL the buyer's browser returns to after checkout";

    if let Some(refusal) = common_refusals(rail_id, dest, FIELD) {
        return refusal;
    }

    let trimmed = dest.trim();
    let insecure = match parse_absolute_http_url(trimmed) {
        Err(why) => {
            return DestinationVerdict::malformed(
                rail_id,
                format!(
                    "the {rail_id} rail uses `destination` as {FIELD}, and every processor in \
                     this crate documents that field as an absolute URL — but {why}. Pass the \
                     full URL the buyer should land on, scheme and all, e.g. \
                     `https://shop.example.com/orders/1234/thanks`."
                ),
            );
        }
        Ok(scheme_is_http) => scheme_is_http,
    };

    let security_note = if insecure {
        " Note that this is a plain `http://` URL: processors generally accept one only in test \
         mode, and a buyer returning from a payment over http is a real exposure — use https in \
         production."
    } else {
        ""
    };

    DestinationVerdict::unknown(
        rail_id,
        format!(
            "this is a well-formed absolute URL, which is what the {rail_id} rail sends this \
             field as — it is where the BUYER'S BROWSER lands after checkout, not an address \
             money is sent to, so there is no payout destination here for a verdict to be about.\
             {security_note} {}",
            processor_owns_this(rail_id)
        ),
    )
}

/// For rails whose `destination` is the **buyer's email address** — Paystack's
/// and Flutterwave's required `email`/`customer.email`, PayU's `buyer.email`,
/// Midtrans's `customer_details.email`.
///
/// Those processors document the field as an email address, so a string that
/// plainly is not one is a [`patala_core::DestinationStatus::Malformed`]
/// refusal. A string that is one is still only
/// [`patala_core::DestinationStatus::Unknown`]: an email address is not a
/// payout address, and whether it is the right buyer's is not decidable here.
pub fn buyer_email(rail_id: &str, dest: &str) -> DestinationVerdict {
    const FIELD: &str = "the buyer's email address";

    if let Some(refusal) = common_refusals(rail_id, dest, FIELD) {
        return refusal;
    }

    if let Err(why) = check_email_shape(dest.trim()) {
        return DestinationVerdict::malformed(
            rail_id,
            format!(
                "the {rail_id} rail uses `destination` as {FIELD}, which its processor requires \
                 to open a transaction — but {why}. Pass the address the buyer's receipt should \
                 go to, e.g. `buyer@example.com`."
            ),
        );
    }

    DestinationVerdict::unknown(
        rail_id,
        format!(
            "this has the shape of an email address, which is what the {rail_id} rail sends this \
             field as — it identifies the BUYER, it is not an address money is sent to, so there \
             is no payout destination here for a verdict to be about. Whether the mailbox exists \
             or belongs to the right person is not decidable offline either. {}",
            processor_owns_this(rail_id)
        ),
    )
}

/// For rails that **never read `destination` at all** — BTCPay, Coinbase
/// Commerce, LNbits, OpenNode, Razorpay and the offline `manual` rail, each of
/// which documents this in its own module and passes a literal placeholder in
/// its own tests.
///
/// The verdict is always [`patala_core::DestinationStatus::Unknown`] for a
/// non-empty string, and the reason says the one thing a caller needs to know:
/// that setting this field steers nothing. Deliberately **no** format check —
/// there is no format. A wallet address here is harmless rather than wrong, so
/// refusing one would be a guard firing at something that is not a defect,
/// which is its own kind of dishonesty.
pub fn ignored(rail_id: &str, dest: &str) -> DestinationVerdict {
    if dest.trim().is_empty() {
        return DestinationVerdict::malformed(
            rail_id,
            format!(
                "an empty destination is refused by `PayRequest::validate()` on every rail, so \
                 the {rail_id} rail cannot be charged with one — even though it never reads the \
                 field. Pass any non-empty placeholder."
            ),
        );
    }

    DestinationVerdict::unknown(
        rail_id,
        format!(
            "the {rail_id} rail never reads `destination` — see its module docs — so this string \
             cannot be right or wrong, and setting it steers nothing. It exists only because \
             `PayRequest::validate()` requires a non-empty one on every rail. If you expected it \
             to direct where the money goes, it does not: on this rail the processor settles to \
             the merchant account configured out of band, and giving a customer their money back \
             is `refund` (or the processor's own dashboard), never a charge to a destination."
        ),
    )
}

/// Is `s` an absolute `http`/`https` URL with a non-empty host? Returns
/// `Ok(true)` when the scheme is plain `http`.
///
/// Deliberately hand-rolled rather than pulling in the `url` crate: this
/// module compiles in the crate's default feature set, which links no optional
/// dependency at all (that is what keeps `cargo build -p patala-fiat` offline),
/// and the question being asked — "did a person paste a URL here, or something
/// that is plainly not one" — needs a scheme, a host and no whitespace, not
/// RFC 3986.
fn parse_absolute_http_url(s: &str) -> Result<bool, &'static str> {
    let (is_http, rest) = if let Some(r) = s.strip_prefix("https://") {
        (false, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (true, r)
    } else if s.contains("://") {
        return Err("its scheme is neither http nor https; a browser cannot be redirected to it");
    } else {
        return Err("it has no scheme at all, so it is not an absolute URL");
    };

    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("it contains whitespace, so it is not a single URL");
    }

    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@') // strip any userinfo
        .next()
        .unwrap_or("");
    if host.is_empty() {
        return Err("it has a scheme but no host");
    }

    Ok(is_http)
}

/// Does `s` have the shape of an email address? Intentionally minimal — one
/// `@`, something either side, a dot in the domain, no whitespace. Validating
/// an email address properly is not possible offline (and barely possible
/// online), so this refuses only what is *plainly* not one, which is exactly
/// the case this method exists to catch: an address, a URL or a bare word
/// pasted into the field.
fn check_email_shape(s: &str) -> Result<(), &'static str> {
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("it contains whitespace");
    }
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("an email address has exactly one `@` and this does not");
    };
    if local.is_empty() {
        return Err("there is nothing before the `@`");
    }
    if domain.is_empty() {
        return Err("there is nothing after the `@`");
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err("the part after the `@` is not a domain name");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use patala_core::DestinationStatus;

    /// One real address per chain, for the cross-rail refusals.
    const SOLANA: &str = "6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs";
    const STELLAR: &str = "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7";
    const EVM: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    const BITCOIN: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

    #[test]
    fn a_good_redirect_url_is_unknown_and_never_structurally_valid() {
        let v = redirect_url("stripe", "https://shop.example.com/thanks?order=1234");
        // The central honesty claim of this module: a well-formed redirect URL
        // is NOT a vetted payout destination, so the ceiling is Unknown.
        assert_eq!(v.status, DestinationStatus::Unknown);
        assert_ne!(v.status, DestinationStatus::StructurallyValid);
        assert!(!v.is_refusal());
        assert!(v.human_must_confirm);
        assert_eq!(v.rail_id, "stripe");
        assert!(v.reason.contains("BUYER'S BROWSER"), "{}", v.reason);
    }

    #[test]
    fn a_string_that_is_not_a_url_is_refused_by_a_redirect_rail() {
        for (dest, needle) in [
            ("shop.example.com/thanks", "no scheme"),
            ("ftp://example.com/x", "neither http nor https"),
            ("https://", "no host"),
            ("https://exa mple.com", "whitespace"),
            ("just some words", "no scheme"),
        ] {
            let v = redirect_url("mollie", dest);
            assert_eq!(v.status, DestinationStatus::Malformed, "{dest}");
            assert!(v.is_refusal(), "{dest}");
            assert!(v.reason.contains(needle), "{dest}: {}", v.reason);
        }
    }

    #[test]
    fn plain_http_is_accepted_but_flagged() {
        // Processors do accept http in test mode, so refusing it would refuse
        // a payment that would have worked — but it is worth saying.
        let v = redirect_url("adyen", "http://localhost:3000/return");
        assert_eq!(v.status, DestinationStatus::Unknown);
        assert!(v.reason.contains("use https in production"), "{}", v.reason);
    }

    #[test]
    fn a_good_email_is_unknown_and_never_structurally_valid() {
        let v = buyer_email("paystack", "buyer@example.com");
        assert_eq!(v.status, DestinationStatus::Unknown);
        assert_ne!(v.status, DestinationStatus::StructurallyValid);
        assert!(v.human_must_confirm);
        assert!(v.reason.contains("identifies the BUYER"), "{}", v.reason);
    }

    #[test]
    fn a_string_that_is_not_an_email_is_refused_by_an_email_rail() {
        for (dest, needle) in [
            ("buyer.example.com", "exactly one `@`"),
            ("a@b@example.com", "exactly one `@`"),
            ("@example.com", "nothing before"),
            ("buyer@", "nothing after"),
            ("buyer@localhost", "not a domain name"),
            ("buyer @example.com", "whitespace"),
        ] {
            let v = buyer_email("payu", dest);
            assert_eq!(v.status, DestinationStatus::Malformed, "{dest}");
            assert!(v.is_refusal(), "{dest}");
            assert!(v.reason.contains(needle), "{dest}: {}", v.reason);
        }
    }

    #[test]
    fn an_ignoring_rail_says_the_field_steers_nothing() {
        let v = ignored("razorpay", "unused-for-razorpay");
        assert_eq!(v.status, DestinationStatus::Unknown);
        assert!(!v.is_refusal());
        assert!(v.reason.contains("never reads"), "{}", v.reason);
        assert!(v.reason.contains("steers nothing"), "{}", v.reason);
    }

    #[test]
    fn an_ignoring_rail_does_not_refuse_a_wallet_address() {
        // It genuinely is harmless there — the rail never reads the field — so
        // a refusal would be a guard firing at something that is not a defect.
        for dest in [SOLANA, STELLAR, EVM, BITCOIN] {
            let v = ignored("btcpay", dest);
            assert_eq!(v.status, DestinationStatus::Unknown, "{dest}");
            assert!(!v.is_refusal(), "{dest}");
        }
    }

    #[test]
    fn every_rail_shape_refuses_every_other_rails_wallet_format_by_name() {
        // The cross-rail case, which is the one that saves money: the message
        // has to name what was pasted, not just say "invalid".
        for (dest, needle) in [
            (SOLANA, "Solana"),
            (STELLAR, "Stellar"),
            (EVM, "Ethereum"),
            (BITCOIN, "Bitcoin"),
            (
                "0x0000000000000000000000000000000000000000000000000000000000000002",
                "Sui",
            ),
            ("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2", "Bitcoin"),
        ] {
            for v in [redirect_url("stripe", dest), buyer_email("paystack", dest)] {
                assert_eq!(v.status, DestinationStatus::WrongNetwork, "{dest}");
                assert!(v.is_refusal(), "{dest}");
                assert!(v.reason.contains(needle), "{dest}: {}", v.reason);
                // And it must point at the right way to give money back here.
                assert!(v.reason.contains("refund"), "{dest}: {}", v.reason);
            }
        }
    }

    #[test]
    fn a_pasted_secret_seed_is_a_disclosure_on_a_fiat_rail_too() {
        // A leaked key is leaked whatever field it was pasted into.
        let seed = "SBFZCHU5645DOKRWYBXVOXY2ELGJKFRX6VGGPRYUWHQ7PMXNMYPOYKUB";
        for v in [redirect_url("stripe", seed), buyer_email("paystack", seed)] {
            assert!(v.is_refusal());
            assert!(v.reason.contains("PRIVATE KEY"), "{}", v.reason);
            assert!(
                !v.reason.contains(seed),
                "a verdict is shown and logged — it must never repeat the secret"
            );
        }
    }

    #[test]
    fn blank_destinations_fail_closed_on_all_three_shapes() {
        for blank in ["", " ", "\t\n"] {
            for v in [
                redirect_url("stripe", blank),
                buyer_email("paystack", blank),
                ignored("razorpay", blank),
            ] {
                assert_eq!(v.status, DestinationStatus::Malformed, "{blank:?}");
                assert!(v.is_refusal(), "{blank:?}");
            }
        }
    }

    #[test]
    fn no_fiat_verdict_is_ever_structurally_valid() {
        // Swept across every shape and every kind of input: a fiat rail has no
        // payout address to call structurally valid, so it must never claim
        // the status that means one.
        for dest in [
            "https://example.com/ok",
            "buyer@example.com",
            "unused",
            SOLANA,
            STELLAR,
            EVM,
            "",
            "nonsense",
        ] {
            for v in [
                redirect_url("stripe", dest),
                buyer_email("paystack", dest),
                ignored("razorpay", dest),
            ] {
                assert_ne!(
                    v.status,
                    DestinationStatus::StructurallyValid,
                    "{dest:?} must never be called structurally valid on a fiat rail"
                );
                assert!(v.human_must_confirm, "{dest:?}");
                assert!(!v.reason.trim().is_empty(), "{dest:?}");
                assert_eq!(
                    v.exchange_deposit_caveat,
                    patala_core::EXCHANGE_DEPOSIT_CAVEAT,
                    "{dest:?}"
                );
            }
        }
    }

    #[test]
    fn validation_is_pure_and_deterministic() {
        for dest in ["https://example.com/ok", "buyer@example.com", SOLANA, ""] {
            assert_eq!(redirect_url("stripe", dest), redirect_url("stripe", dest));
            assert_eq!(buyer_email("payu", dest), buyer_email("payu", dest));
            assert_eq!(ignored("lnbits", dest), ignored("lnbits", dest));
        }
    }

    #[test]
    fn the_wallet_sniffer_does_not_fire_on_ordinary_urls_and_emails() {
        // A false positive here would refuse a perfectly good redirect URL, so
        // it matters more than a false negative (which only costs a vaguer
        // message).
        for dest in [
            "https://shop.example.com/orders/1234/thanks",
            "http://localhost:3000/return",
            "buyer@example.com",
            "unused-for-razorpay",
            "cs_test_a1B2c3D4e5F6g7H8i9J0",
        ] {
            assert_eq!(looks_like_a_wallet_address(dest), None, "{dest}");
            assert!(!looks_like_a_secret_key(dest), "{dest}");
        }
    }
}
