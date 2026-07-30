//! Offline destination validation for the Stellar rail —
//! [`patala_core::PaymentRail::validate_destination`]'s implementation.
//!
//! # Why this exists
//!
//! Settlement on this rail is final (`RailClass::NonCustodialFinal`), so
//! [`patala_core::PaymentRail::refund`] is, correctly, `Unsupported`. Paying a
//! customer back is a **compensating payment** instead: a second, independent
//! [`patala_core::PaymentRail::charge`] to an address the *customer* supplies, never
//! the address the money came from — that one is very often an exchange
//! withdrawal address where the funds cannot be credited back to them. See
//! [`patala_core::destination`] for the whole two-party flow.
//!
//! # Stellar can decide more offline than most chains
//!
//! A Solana address is 32 raw bytes in base58: there is no checksum, so a
//! single mistyped character usually produces *another perfectly well-formed
//! address*. Stellar's [StrKey](https://developers.stellar.org/docs/learn/encyclopedia/security/signatures-multisig)
//! encoding (SEP-23) is uppercase RFC4648 base32 over three parts —
//!
//! ```text
//! [ 1 version byte ][ payload ][ 2-byte CRC16-XMODEM checksum ]
//! ```
//!
//! — which makes two extra things decidable here that are not decidable there:
//!
//! * **A bad checksum is unambiguous.** A typo is caught rather than silently
//!   redirecting the money to a stranger. [`validate`] says so in those words,
//!   because "checksum failed" tells a person to re-copy the address, where
//!   "invalid address" tells them to squint at it.
//! * **The version byte encodes the TYPE**, so `G…` (an account), `M…` (a
//!   muxed account), `C…` (a Soroban contract), `T…`/`X…`/`P…` (signer types)
//!   and `S…` (a **secret seed**) are all told apart from the string alone.
//!
//! ## `S…` is a private key, and is reported as one
//!
//! A seed pasted into a destination field is not a wrong address — it is a
//! **key disclosure**, and the person needs to be told that, not told
//! "invalid". [`validate`] gives it its own loud refusal naming what happened
//! and what to do about it, and that check runs **before** the checksum is
//! ever consulted: a secret is disclosed whether or not it was typed
//! correctly. The verdict never repeats the value — a
//! [`patala_core::DestinationVerdict::reason`] is meant to be shown to a
//! person and is very likely to be logged on the way there.
//!
//! # Who decides
//!
//! The authority on whether a string is a valid StrKey is
//! [`stellar_strkey`] — the Stellar Development Foundation's own crate, the
//! same one [`crate::keys`] already delegates to. Nothing in this module ever
//! *accepts* an address on its own: it calls that crate and, when that crate
//! says no, works out **why** so the refusal can be explained. That split is
//! deliberate — a second checksum implementation here could disagree with the
//! first, and the one place two validators must never disagree is the one
//! deciding where money goes.
//!
//! # The purity contract
//!
//! [`validate`] is **pure and offline**: no network, no clock, no filesystem,
//! no global state. It is a free function so it needs no configured rail, no
//! Horizon URL and no keypair — it runs in a browser through wasm, on a gate
//! device with no uplink, and in a test with no Horizon.
//!
//! What it therefore cannot decide, and does not attempt:
//!
//! - **Whether the account exists** and is funded above the base reserve. A
//!   `PAYMENT` to an unfunded account fails with `op_no_destination`; finding
//!   out needs `GET /accounts/{id}`, a chain query, a different method.
//! - **Whether it has a USDC trustline.** Stellar requires the recipient to
//!   have explicitly trusted the asset; without one the payment fails with
//!   `op_no_trust`. This is the single most common Stellar payment failure and
//!   it is *not* knowable from the address — only from the account's
//!   `balances`, over the network.
//! - **Who owns it.** patala never tries to detect whether an address belongs
//!   to an exchange — see [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`], which
//!   rides on every verdict this module produces, including the most positive
//!   one.

use patala_core::DestinationVerdict;
use stellar_strkey::Strkey;

/// The rail id every verdict from this module carries — the same string
/// [`patala_core::PaymentRail::id`] returns. A verdict is only ever an opinion about
/// the network *this* rail pays on.
pub const RAIL_ID: &str = "stellar";

/// The character count of every StrKey whose payload is 32 bytes: 1 version
/// byte + 32 payload + 2 checksum = 35 bytes, and 35 bytes is exactly 56
/// base32 characters. Covers `G`, `S`, `C`, `T` and `X`.
const STRKEY_LEN_32: usize = 56;

/// A muxed account (`M…`) carries 32 key bytes plus a 64-bit subaccount id:
/// 1 + 40 + 2 = 43 bytes, which is 69 base32 characters.
const STRKEY_LEN_MUXED: usize = 69;

/// Is this character in the RFC4648 base32 alphabet StrKey uses? (`A`–`Z` and
/// `2`–`7`; `0`, `1`, `8` and `9` are omitted because each is confusable with
/// a letter the alphabet does use.)
///
/// **Case-insensitive on purpose.** SEP-23 writes StrKeys in uppercase, but
/// the decoder that actually decides — [`stellar_strkey`], and through it the
/// `base32` crate — accepts either case and yields the same bytes, so a
/// lowercased address really does resolve to the same account and
/// [`patala_core::PaymentRail::charge`] really does pay it. This predicate exists
/// only to explain a rejection that decoder already made; if it were stricter
/// than the decoder it would invent a cause that was not the real one.
fn is_base32_char(c: char) -> bool {
    c.is_ascii_alphabetic() || ('2'..='7').contains(&c)
}

/// Is every character in that alphabet?
fn is_strkey_alphabet(s: &str) -> bool {
    s.chars().all(is_base32_char)
}

/// The base58 alphabet — used only to *recognize* an address from a base58
/// chain so a refusal can name it. Never used to accept anything.
const BASE58_ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// A destination that is well-formed for some *other* chain, named. Only ever
/// used to turn a bare "invalid" into the sentence that actually saves the
/// money.
///
/// Recognition here is by shape alone — length plus alphabet, no decoding —
/// which is why every message says "looks like". That is enough: the verdict
/// is a refusal either way, and the only thing the shape has to buy is a
/// message that tells the person which field they pasted into by mistake. No
/// shape below can be a StrKey (every StrKey is 56 or 69 characters of
/// uppercase base32), so this can run before the StrKey decode without ever
/// shadowing a real Stellar address.
fn foreign_format(dest: &str) -> Option<&'static str> {
    let len = dest.len();
    let all_base58 = |s: &str| s.chars().all(|c| BASE58_ALPHABET.contains(c));

    // Solana: 32 bytes of base58 is always 43 or 44 characters.
    if matches!(len, 43 | 44) && all_base58(dest) {
        return Some("a Solana address (32 bytes of base58)");
    }

    // Bitcoin, both eras.
    if matches!(len, 26..=35)
        && all_base58(dest)
        && (dest.starts_with('1') || dest.starts_with('3'))
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

    // Hex-with-0x families.
    if let Some(hex) = dest.strip_prefix("0x").or_else(|| dest.strip_prefix("0X")) {
        if hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return match hex.len() {
                40 => Some("an Ethereum/EVM address (0x + 40 hex characters)"),
                64 => Some("a Sui or Aptos address (0x + 64 hex characters)"),
                _ => None,
            };
        }
    }

    None
}

/// The refusal for a pasted private key. Never quotes the key itself — see the
/// module docs.
fn secret_seed_refusal() -> DestinationVerdict {
    DestinationVerdict::malformed(
        RAIL_ID,
        "STOP — this is not an address, it is a Stellar SECRET SEED (S…). You have just pasted a \
         PRIVATE KEY into a destination field. Treat it as disclosed: create a new Stellar \
         account, move every asset the old key controls to it, remove the old key as a signer \
         anywhere it is one, and never use it again. patala has not logged the value and this \
         message does not repeat it, but wherever you copied it from may have. The address you \
         want is the PUBLIC account address (G…) derived from that seed.",
    )
}

/// Explain a StrKey that [`stellar_strkey`] rejected.
///
/// Only ever reached after the authoritative decode has already said no, so
/// this cannot accept anything — it only chooses the sentence.
///
/// The checksum conclusion is a deduction, and it is sound. `stellar-strkey`'s
/// own `DecodeError` is a single opaque `Invalid` variant, so the reason has
/// to be reconstructed: its decoder can reject a string for exactly four
/// things — a non-base32 character, a non-canonical length, an unrecognized
/// version byte, or a CRC16 mismatch. If the alphabet is clean, the length is
/// the canonical one for the leading character's type, and the second
/// character is in `A`–`D` (every StrKey version byte is `n << 3`, so its low
/// three bits are zero, and those three bits are the top of the second base32
/// character — which pins it to the first four), then the first three are
/// excluded and the checksum is all that is left.
fn explain_rejection(dest: &str, expected_len: usize, what: &str) -> String {
    if let Some(bad) = dest.chars().find(|c| !is_base32_char(*c)) {
        let hint = match bad {
            '0' => " (base32 has no 0 — the letter O is what it uses)",
            '1' => " (base32 has no 1 — the letter I is what it uses)",
            '8' => " (base32 has no 8 — the letter B is what it uses)",
            '9' => " (base32 has no 9 — the letter G is what it uses)",
            _ => "",
        };
        return format!(
            "this is not a valid Stellar address: the character {bad:?} is not in the base32 \
             alphabet StrKey uses{hint}."
        );
    }

    if dest.len() != expected_len {
        return format!(
            "this starts like {what}, but {what} is exactly {expected_len} characters and this is \
             {}. Characters were lost or added when it was copied — take the whole address again.",
            dest.len()
        );
    }

    // Second character pins the low three bits of the version byte; see this
    // function's docs.
    match dest.chars().nth(1) {
        Some('A'..='D') => format!(
            "this is {what} of exactly the right length and alphabet, but its CRC16 checksum does \
             not match — so at least one character is wrong. This is what a typo, a truncated \
             copy, or an address mangled in transit looks like, and it is exactly the case \
             Stellar's checksum exists to catch. Do NOT try to repair it by hand: copy the whole \
             address again from the wallet that owns it."
        ),
        _ => format!(
            "this starts like {what} and has the right length, but its second character does not \
             encode a StrKey version byte, so it is not a StrKey at all. Re-copy the address."
        ),
    }
}

/// Check a Stellar destination address, offline. See the module docs for
/// exactly what StrKey makes decidable and what still needs Horizon.
///
/// Pure: the same `dest` always gives the same verdict, with no network, no
/// clock and no filesystem. No verdict — not even
/// [`patala_core::DestinationStatus::StructurallyValid`] — means "safe to send
/// to"; see
/// [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`], which every verdict carries.
///
/// ```
/// use patala_core::DestinationStatus;
/// use patala_stellar::destination::validate;
/// use patala_stellar::keys::Keypair;
///
/// let account = Keypair::from_seed([3u8; 32]).pubkey().to_strkey();
/// assert_eq!(validate(&account).status, DestinationStatus::StructurallyValid);
/// assert!(validate(&account).human_must_confirm);
///
/// // One character wrong, and the checksum says so — unambiguously.
/// let mut typo = account.clone();
/// typo.replace_range(10..11, if &typo[10..11] == "A" { "B" } else { "A" });
/// let v = validate(&typo);
/// assert_eq!(v.status, DestinationStatus::Malformed);
/// assert!(v.reason.contains("checksum"));
///
/// // A Solana address is refused by name, not with a bare "invalid".
/// let v = validate("6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs");
/// assert_eq!(v.status, DestinationStatus::WrongNetwork);
/// assert!(v.reason.contains("Solana"));
/// ```
pub fn validate(dest: &str) -> DestinationVerdict {
    if dest.trim().is_empty() {
        return DestinationVerdict::malformed(
            RAIL_ID,
            "an empty destination is not an address on any rail",
        );
    }

    // Fail closed on surrounding whitespace rather than silently trimming:
    // `StellarRail::charge` does not trim either, so accepting here what
    // `charge` would reject would make this check a lie at the one moment it
    // matters.
    if dest.trim() != dest {
        return DestinationVerdict::malformed(
            RAIL_ID,
            "this address has leading or trailing whitespace, which came from the copy rather \
             than from the address — remove it and check nothing else was picked up with it",
        );
    }

    // FIRST, before the checksum is consulted at all: a secret seed is
    // disclosed whether or not it was typed correctly, so a mistyped seed must
    // still be reported as a leaked key and not as "bad checksum".
    if dest.starts_with(['S', 's']) && dest.len() == STRKEY_LEN_32 && is_strkey_alphabet(dest) {
        return secret_seed_refusal();
    }

    // A well-formed address from another chain, named rather than dismissed.
    if let Some(what) = foreign_format(dest) {
        return DestinationVerdict::wrong_network(
            RAIL_ID,
            format!(
                "this looks like {what}, not a Stellar address. The stellar rail pays USDC on \
                 Stellar only; funds sent using an address from another chain do not arrive on \
                 that chain, they simply do not arrive. Ask for the recipient's Stellar account \
                 address — it starts with G and is 56 characters."
            ),
        );
    }

    // The authority. Nothing below this line ever accepts an address the SDF's
    // own decoder rejected.
    let Ok(parsed) = Strkey::from_string(dest) else {
        let (expected_len, what) = match dest.chars().next() {
            Some('G') => (STRKEY_LEN_32, "a Stellar account address (G…)"),
            Some('M') => (STRKEY_LEN_MUXED, "a Stellar muxed account (M…)"),
            Some('C') => (STRKEY_LEN_32, "a Stellar contract address (C…)"),
            Some('S') => (STRKEY_LEN_32, "a Stellar secret seed (S…)"),
            Some('T') => (STRKEY_LEN_32, "a Stellar pre-auth-tx signer (T…)"),
            Some('X') => (STRKEY_LEN_32, "a Stellar hash-x signer (X…)"),
            _ => {
                return DestinationVerdict::malformed(
                    RAIL_ID,
                    "this is not a Stellar address. A Stellar account address is StrKey: 56 \
                     characters of uppercase base32 beginning with G. Ask the recipient for the \
                     address their wallet shows, not a username, an email, a federation address, \
                     or an address from another chain.",
                )
            }
        };
        return DestinationVerdict::malformed(RAIL_ID, explain_rejection(dest, expected_len, what));
    };

    match parsed {
        Strkey::PublicKeyEd25519(pk) => {
            // Defense in depth. A StrKey payload is 32 arbitrary bytes and the
            // checksum does not care whether they are a real Ed25519 point;
            // an off-curve one can only arrive by fabrication (a random typo
            // would have to also produce a matching CRC), but if it does, no
            // keypair for it can exist and nobody can ever spend from it.
            if ed25519_dalek::VerifyingKey::from_bytes(&pk.0).is_err() {
                return DestinationVerdict::not_a_wallet(
                    RAIL_ID,
                    "this is a checksum-valid G… address whose 32-byte payload is not a point on \
                     the ed25519 curve, so no private key can correspond to it and no one can \
                     ever move funds out of it. A real Stellar account address is always a real \
                     public key; this one was constructed rather than generated.",
                );
            }
            DestinationVerdict::structurally_valid(
                RAIL_ID,
                "this is a well-formed Stellar account address: the StrKey version byte says \
                 account, the CRC16 checksum verifies, and the payload is a real ed25519 public \
                 key. That is every check possible without asking Horizon a question. In \
                 particular this rail cannot yet know whether the account EXISTS and is funded \
                 above the base reserve, or whether it holds a TRUSTLINE for the USDC asset this \
                 rail pays in — without one the payment is rejected with op_no_trust, which is \
                 the most common way a Stellar payment fails. Confirm both with the recipient \
                 before paying.",
            )
        }
        Strkey::MuxedAccountEd25519(_) => DestinationVerdict::not_a_wallet(
            RAIL_ID,
            "this is a muxed account (M…): a Stellar account plus a 64-bit subaccount id, which \
             is what an exchange or custodian hands out so it can tell its own customers apart \
             inside one shared omnibus account. It is a valid Stellar destination in general, \
             but NOT one this rail can pay: patala-stellar builds only plain Ed25519 payment \
             destinations, so it would drop the subaccount id and the money would land in the \
             omnibus account with nothing to say it was yours — which is precisely the \
             unrecoverable case. Ask the recipient for an account they control directly (G…), \
             not a deposit address at a custodian.",
        ),
        Strkey::Contract(_) => DestinationVerdict::not_a_wallet(
            RAIL_ID,
            "this is a Soroban contract address (C…), not an account. A contract has no keypair \
             and no one can sign for it; this rail builds classic PAYMENT operations, which a \
             contract address cannot be the destination of at all. Ask for the recipient's \
             account address (G…).",
        ),
        Strkey::PreAuthTx(_) | Strkey::HashX(_) | Strkey::SignedPayloadEd25519(_) => {
            DestinationVerdict::not_a_wallet(
                RAIL_ID,
                "this is a valid StrKey, but it encodes a SIGNER (a pre-authorized transaction, a \
                 hash-x signer, or a signed payload), not an account. Signers are things that may \
                 authorize a transaction on an account; they are not places money can be sent, \
                 and nothing sent using one is recoverable. Ask for the recipient's account \
                 address (G…).",
            )
        }
        // Unreachable: caught above, before the checksum, so that a mistyped
        // seed is still reported as a disclosure. Kept so the match stays
        // exhaustive without a wildcard — a new StrKey type must be classified
        // here deliberately rather than falling through to something.
        Strkey::PrivateKeyEd25519(_) => secret_seed_refusal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Keypair;
    use patala_core::DestinationStatus;

    /// The Stellar Asset Contract for native XLM on the public network — a
    /// real, widely published `C…` address. `a_published_contract_address_is_
    /// a_real_strkey` asserts it really parses, so a typo here fails the test
    /// run rather than quietly weakening the test below it.
    const XLM_SAC_MAINNET: &str = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";

    /// A real Solana address: 32 bytes of base58 (see `patala-solana`).
    const SOLANA_WALLET: &str = "6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs";

    fn account() -> String {
        Keypair::from_seed([3u8; 32]).pubkey().to_strkey()
    }

    /// Break exactly one character without changing the length or leaving the
    /// alphabet, so what the test exercises is the CRC and nothing else.
    fn corrupt_one_character(s: &str, at: usize) -> String {
        let mut out = s.to_string();
        let orig = &s[at..at + 1];
        out.replace_range(at..at + 1, if orig == "A" { "B" } else { "A" });
        assert_eq!(out.len(), s.len());
        assert_ne!(out, s);
        out
    }

    #[test]
    fn a_published_contract_address_is_a_real_strkey() {
        // The test below reads this as "a real contract address"; that has to
        // be a fact, not a hope.
        assert!(matches!(
            Strkey::from_string(XLM_SAC_MAINNET),
            Ok(Strkey::Contract(_))
        ));
        assert_eq!(XLM_SAC_MAINNET.len(), STRKEY_LEN_32);
    }

    #[test]
    fn a_real_account_address_is_structurally_valid_but_never_waives_the_human() {
        let v = validate(&account());
        assert_eq!(v.status, DestinationStatus::StructurallyValid);
        assert!(!v.is_refusal());
        assert_eq!(v.rail_id, "stellar");
        assert!(v.human_must_confirm);
        assert!(v.requires_human_confirmation());
        assert_eq!(
            v.exchange_deposit_caveat,
            patala_core::EXCHANGE_DEPOSIT_CAVEAT
        );
        // The honest caveat that is Stellar-specific and matters most.
        assert!(v.reason.contains("op_no_trust"), "{}", v.reason);
    }

    #[test]
    fn a_deliberately_corrupted_checksum_is_reported_as_a_checksum_failure() {
        let good = account();
        // Corrupt a payload character (not the version byte, not the length),
        // which is exactly the single-character typo StrKey's CRC exists for.
        let bad = corrupt_one_character(&good, 10);
        assert!(
            Strkey::from_string(&bad).is_err(),
            "the SDF decoder must agree this is broken"
        );

        let v = validate(&bad);
        assert_eq!(v.status, DestinationStatus::Malformed);
        assert!(v.is_refusal());
        assert!(
            v.reason.contains("checksum"),
            "a typo must be named as a typo, not as 'invalid': {}",
            v.reason
        );
        assert!(
            v.reason.contains("copy the whole address again"),
            "and the message must say what to do: {}",
            v.reason
        );
        // Corrupting the checksum bytes themselves reaches the same verdict.
        let tail = corrupt_one_character(&good, STRKEY_LEN_32 - 1);
        assert_eq!(validate(&tail).status, DestinationStatus::Malformed);
        assert!(validate(&tail).reason.contains("checksum"));
    }

    #[test]
    fn a_pasted_secret_seed_is_a_disclosure_and_never_echoed() {
        let seed = stellar_strkey::ed25519::PrivateKey([7u8; 32]).to_string();
        assert!(seed.starts_with('S'));

        let v = validate(&seed);
        assert!(v.is_refusal(), "a leaked key must fail closed");
        assert_eq!(v.status, DestinationStatus::Malformed);
        assert!(
            v.reason.contains("PRIVATE KEY"),
            "the person needs to know what they just did: {}",
            v.reason
        );
        assert!(
            v.reason.contains("never use it again"),
            "and what to do about it: {}",
            v.reason
        );
        assert!(
            !v.reason.contains(&seed),
            "a verdict is shown and logged — it must never repeat the secret"
        );
    }

    #[test]
    fn a_mistyped_seed_is_still_reported_as_a_leak_not_as_a_bad_checksum() {
        // The ordering that matters: a secret is disclosed whether or not it
        // survived the copy intact. Telling someone "bad checksum" here would
        // send them back to re-copy the *secret*.
        let seed = stellar_strkey::ed25519::PrivateKey([7u8; 32]).to_string();
        let mistyped = corrupt_one_character(&seed, 20);
        assert!(Strkey::from_string(&mistyped).is_err());

        let v = validate(&mistyped);
        assert!(v.reason.contains("PRIVATE KEY"), "{}", v.reason);
        assert!(!v.reason.contains("checksum"), "{}", v.reason);
        assert!(!v.reason.contains(&mistyped));
    }

    #[test]
    fn a_muxed_account_is_refused_with_the_reason_that_saves_the_money() {
        let muxed = stellar_strkey::ed25519::MuxedAccount {
            ed25519: Keypair::from_seed([5u8; 32]).pubkey().0,
            id: 1234,
        }
        .to_string();
        assert!(muxed.starts_with('M'));
        assert_eq!(muxed.len(), STRKEY_LEN_MUXED);

        let v = validate(&muxed);
        assert_eq!(v.status, DestinationStatus::NotAWallet);
        assert!(v.is_refusal());
        // Not "invalid" — it is valid. The point is that THIS RAIL would drop
        // the subaccount id, which is the unrecoverable case.
        assert!(v.reason.contains("subaccount id"), "{}", v.reason);
        assert!(v.reason.contains("omnibus"), "{}", v.reason);
    }

    #[test]
    fn a_real_contract_address_is_refused_as_not_an_account() {
        let v = validate(XLM_SAC_MAINNET);
        assert_eq!(v.status, DestinationStatus::NotAWallet);
        assert!(v.is_refusal());
        assert!(v.reason.contains("contract"), "{}", v.reason);
    }

    #[test]
    fn signer_strkeys_are_refused_as_signers_rather_than_accounts() {
        for (label, s) in [
            (
                "pre-auth-tx",
                Strkey::PreAuthTx(stellar_strkey::PreAuthTx([9u8; 32])).to_string(),
            ),
            (
                "hash-x",
                Strkey::HashX(stellar_strkey::HashX([9u8; 32])).to_string(),
            ),
        ] {
            let v = validate(&s);
            assert_eq!(v.status, DestinationStatus::NotAWallet, "{label}");
            assert!(v.reason.contains("SIGNER"), "{label}: {}", v.reason);
        }
    }

    #[test]
    fn a_solana_address_is_named_rather_than_called_invalid() {
        let v = validate(SOLANA_WALLET);
        assert_eq!(v.status, DestinationStatus::WrongNetwork);
        assert!(v.is_refusal());
        assert!(
            v.reason.contains("Solana"),
            "naming the format is the message that saves the money: {}",
            v.reason
        );
        // And the message says what to ask for instead.
        assert!(v.reason.contains("starts with G"));
    }

    #[test]
    fn other_chains_are_named_too() {
        for (dest, needle) in [
            ("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "Ethereum"),
            (
                "0x0000000000000000000000000000000000000000000000000000000000000002",
                "Sui",
            ),
            ("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", "Bitcoin"),
            ("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2", "Bitcoin"),
        ] {
            let v = validate(dest);
            assert_eq!(v.status, DestinationStatus::WrongNetwork, "{dest}");
            assert!(v.reason.contains(needle), "{dest}: {}", v.reason);
        }
    }

    #[test]
    fn a_lowercased_address_is_accepted_because_the_sdf_decoder_accepts_it() {
        // Pinning third-party behaviour that is genuinely surprising. SEP-23
        // writes StrKeys uppercase, and an earlier draft of this module
        // refused a lowercased one as "mangled". That was WRONG: the decoder
        // that decides — stellar-strkey, via the `base32` crate — is
        // case-insensitive, so a lowercased address decodes to the very same
        // account, `PubKey::from_strkey` accepts it, and `charge` really does
        // pay it. Refusing here would have meant refusing money that would
        // have arrived, which is a guard failing in the expensive direction.
        let upper = account();
        let lower = upper.to_lowercase();
        assert_ne!(upper, lower);
        assert_eq!(
            crate::keys::PubKey::from_strkey(&lower).unwrap().0,
            crate::keys::PubKey::from_strkey(&upper).unwrap().0,
            "same account, either case — this is the fact the verdict follows"
        );
        assert_eq!(
            validate(&lower).status,
            DestinationStatus::StructurallyValid
        );
    }

    #[test]
    fn a_lowercased_secret_seed_is_still_reported_as_a_leak() {
        // Follows from the case-insensitivity above: if a lowercased address
        // is a real address, a lowercased seed is a real seed.
        let seed = stellar_strkey::ed25519::PrivateKey([7u8; 32])
            .to_string()
            .to_lowercase();
        let v = validate(&seed);
        assert!(v.reason.contains("PRIVATE KEY"), "{}", v.reason);
        assert!(!v.reason.contains(&seed));
    }

    #[test]
    fn base32_lookalike_digits_are_named() {
        // base32 omits 0, 1, 8 and 9 because each is confusable with a letter
        // it does use.
        for (digit, letter) in [('0', 'O'), ('1', 'I'), ('8', 'B'), ('9', 'G')] {
            let mut broken = account();
            broken.replace_range(10..11, &digit.to_string());
            let v = validate(&broken);
            assert_eq!(v.status, DestinationStatus::Malformed, "{digit}");
            assert!(
                v.reason.contains(&format!("the letter {letter}")),
                "{digit} should point at {letter}: {}",
                v.reason
            );
        }
    }

    #[test]
    fn a_truncated_address_is_told_its_length() {
        let good = account();
        let short = &good[..good.len() - 3];
        let v = validate(short);
        assert_eq!(v.status, DestinationStatus::Malformed);
        assert!(v.reason.contains("56 characters"), "{}", v.reason);
        assert!(v.reason.contains("53"), "{}", v.reason);
    }

    #[test]
    fn blank_and_whitespace_wrapped_destinations_fail_closed() {
        for blank in ["", " ", "\t\n"] {
            let v = validate(blank);
            assert_eq!(v.status, DestinationStatus::Malformed, "{blank:?}");
            assert!(v.is_refusal());
        }
        let padded = format!(" {}\n", account());
        let v = validate(&padded);
        assert_eq!(v.status, DestinationStatus::Malformed);
        assert!(v.reason.contains("whitespace"));
        assert_eq!(
            validate(&account()).status,
            DestinationStatus::StructurallyValid
        );
    }

    #[test]
    fn things_that_are_not_addresses_at_all_are_refused_by_name() {
        for dest in [
            "alice*stellar.org", // a federation address
            "alice@example.com", // an email
            "my stellar wallet", // prose
        ] {
            let v = validate(dest);
            assert_eq!(v.status, DestinationStatus::Malformed, "{dest}");
            assert!(v.is_refusal(), "{dest}");
        }
    }

    #[test]
    fn validation_is_pure_and_deterministic() {
        for dest in [
            account().as_str(),
            XLM_SAC_MAINNET,
            SOLANA_WALLET,
            "",
            "not an address",
        ] {
            assert_eq!(validate(dest), validate(dest), "{dest:?}");
        }
    }

    #[test]
    fn no_verdict_this_module_can_produce_is_a_green_light() {
        let seed = stellar_strkey::ed25519::PrivateKey([7u8; 32]).to_string();
        for dest in [
            account().as_str(),
            XLM_SAC_MAINNET,
            SOLANA_WALLET,
            seed.as_str(),
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
            "not an address",
            "",
        ] {
            let v = validate(dest);
            assert!(v.human_must_confirm, "{dest:?}");
            assert_eq!(v.rail_id, RAIL_ID, "{dest:?}");
            assert!(!v.reason.trim().is_empty(), "{dest:?}");
            assert_eq!(
                v.exchange_deposit_caveat,
                patala_core::EXCHANGE_DEPOSIT_CAVEAT,
                "{dest:?}"
            );
        }
    }
}
