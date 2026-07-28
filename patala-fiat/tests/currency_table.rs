//! The ISO-4217 minor-unit table is money-critical: a wrong exponent is a
//! 100× over- or undercharge. This file pins it so that any change is
//! deliberate.
//!
//! Three layers, in increasing order of how much a human can audit them:
//!
//! 1. `currency_table_matches_its_pinned_checksum` — a digest over every
//!    `(code, exponent, name)` triple in table order. Catches *any* edit,
//!    including a renamed currency or a reordered table, with one constant to
//!    update.
//! 2. `currency_table_has_the_expected_number_of_entries` — the count, so a
//!    dropped row is named as a dropped row rather than an opaque digest
//!    mismatch.
//! 3. `zero_and_three_decimal_currencies_are_exactly_these` — the two
//!    exponent classes that are not the default, written out in full. This is
//!    the layer a reviewer can actually check against ISO 4217 by eye, and
//!    the one that matters: an exponent-2 currency wrongly listed as
//!    exponent-0 is the 100× bug.
//!
//! ## When one of these fails
//!
//! A failure is not automatically a bug — currencies genuinely change
//! (redenominations, new codes). It means *prove the change is right*:
//!
//! - Check the new/changed row against ISO 4217 itself, not against another
//!   copy of somebody's table.
//! - Update the exponent-class lists below if an exponent moved, and say in
//!   the commit message which authority you checked.
//! - Only then update `TABLE_CHECKSUM` and `ENTRY_COUNT` to the values the
//!   failure printed.
//!
//! Updating the constants first, to make the suite green, defeats the entire
//! point of the file.
//!
//! These tests run in the crate's **default** feature set — no processor
//! adapter, no network, no optional dependency — so they cannot be skipped by
//! a feature flag and there is nothing to skip loudly about.

use patala_fiat::currency;

/// Digest of the whole table. See the module docs before changing it.
const TABLE_CHECKSUM: u64 = 0xc23a_fdbe_c2e8_34e0;

/// Number of currencies in the table. See the module docs before changing it.
const ENTRY_COUNT: usize = 147;

/// FNV-1a over the canonical rendering of the table.
///
/// Deliberately hand-rolled rather than SHA-256 from a crate: `patala-fiat`'s
/// default feature set pulls in **zero** optional dependencies (that is the
/// property the workspace root `Cargo.toml` keeps this crate in
/// `default-members` for), and `sha2` is behind an adapter feature. This is a
/// drift detector, not a security primitive — no adversary is choosing the
/// contents of an ISO-4217 table — so a dependency-free 64-bit hash is the
/// right tool, exactly as `patala_core::MockRail` reasons about its own
/// digest.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// `CODE:exponent:Name` per line, in table order — the exact bytes the
/// checksum covers.
fn canonical_table() -> String {
    let mut out = String::new();
    for code in currency::supported_currencies() {
        let info = currency::lookup(code).expect("every listed code must look up");
        out.push_str(&format!("{}:{}:{}\n", info.code, info.exponent, info.name));
    }
    out
}

#[test]
fn currency_table_matches_its_pinned_checksum() {
    let actual = fnv1a64(canonical_table().as_bytes());
    assert_eq!(
        actual, TABLE_CHECKSUM,
        "the ISO-4217 table changed. This is money-critical — read this file's \
         module docs before touching TABLE_CHECKSUM. If the change is correct \
         and verified against ISO 4217, the new value is {actual:#018x}."
    );
}

#[test]
fn currency_table_has_the_expected_number_of_entries() {
    let actual = currency::supported_currencies().len();
    assert_eq!(
        actual, ENTRY_COUNT,
        "the table gained or lost currencies ({ENTRY_COUNT} -> {actual}). \
         Confirm each added/removed code against ISO 4217 before updating \
         ENTRY_COUNT."
    );
}

#[test]
fn currency_table_has_no_duplicate_codes() {
    let codes = currency::supported_currencies();
    let unique: std::collections::BTreeSet<_> = codes.iter().collect();
    assert_eq!(
        unique.len(),
        codes.len(),
        "a duplicate code would let one row silently shadow another in the \
         lookup map, which is exactly how a wrong exponent hides"
    );
}

#[test]
fn zero_and_three_decimal_currencies_are_exactly_these() {
    // The auditable layer. Everything not listed here must be exponent 2.
    const ZERO_DECIMAL: &[&str] = &[
        "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VND", "VUV",
        "XAF", "XOF", "XPF",
    ];
    const THREE_DECIMAL: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

    let mut zero: Vec<&str> = Vec::new();
    let mut three: Vec<&str> = Vec::new();
    for code in currency::supported_currencies() {
        match currency::exponent(code).expect("every listed code has an exponent") {
            0 => zero.push(code),
            2 => {}
            3 => three.push(code),
            other => panic!(
                "{code} has exponent {other}: ISO 4217 uses only 0, 2 and 3 in \
                 this table, so a new class needs a deliberate decision (and \
                 this assertion updated), not a silent pass"
            ),
        }
    }

    assert_eq!(
        zero, ZERO_DECIMAL,
        "the set of zero-decimal currencies changed. A currency wrongly moved \
         INTO this list is charged 100× too little; wrongly moved OUT, 100× too \
         much. Verify against ISO 4217 before updating this list."
    );
    assert_eq!(
        three, THREE_DECIMAL,
        "the set of three-decimal currencies changed. Verify against ISO 4217 \
         before updating this list."
    );
}

#[test]
fn the_conversion_helpers_agree_with_the_table_for_every_currency() {
    // Not a spot check: every code in the table round-trips at each exponent
    // boundary. A row whose exponent disagrees with the formatting/parsing
    // code shows up here rather than in production.
    for code in currency::supported_currencies() {
        let exp = currency::exponent(code).unwrap() as u32;
        let one_major = 10u64.pow(exp);

        let rendered = currency::minor_to_major_string(one_major, code).unwrap();
        assert_eq!(
            currency::major_string_to_minor(&rendered, code).unwrap(),
            one_major,
            "{code}: {one_major} minor units rendered as {rendered:?} did not \
             parse back"
        );

        match rendered.split_once('.') {
            Some((_, frac)) => assert_eq!(
                frac.len(),
                exp as usize,
                "{code}: {rendered:?} does not carry the table's {exp} decimal places"
            ),
            None => assert_eq!(
                exp, 0,
                "{code}: {rendered:?} has no decimal point but the table says \
                 it has {exp} decimal places"
            ),
        }

        // One minor unit is never rendered as a whole major unit.
        if exp > 0 {
            let smallest = currency::minor_to_major_string(1, code).unwrap();
            assert!(
                smallest.starts_with("0."),
                "{code}: one minor unit rendered as {smallest:?}"
            );
        }
    }
}
