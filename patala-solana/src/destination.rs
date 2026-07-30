//! Offline destination validation for the Solana rail —
//! [`patala_core::PaymentRail::validate_destination`]'s implementation.
//!
//! # Why this exists
//!
//! Settlement on this rail is final (`RailClass::NonCustodialFinal`), so
//! [`patala_core::PaymentRail::refund`] is, correctly, `Unsupported`. Paying a
//! customer back is instead a **compensating payment**: a second, independent
//! [`patala_core::PaymentRail::charge`] to an address the *customer* supplies, never
//! the address the money came from — that one is very often an exchange
//! withdrawal address where the funds cannot be credited back to them. See
//! [`patala_core::destination`] for the whole two-party flow.
//!
//! [`validate`] is what a caller runs on that customer-supplied address at the
//! moment a person types it, so "that is not a valid Solana address" is said
//! then rather than at charge time — or never.
//!
//! # The purity contract
//!
//! [`validate`] is **pure and offline**: no network, no clock, no filesystem,
//! no global state. It is a free function precisely so it needs no configured
//! rail, no RPC URL and no keypair — it runs in a browser through wasm, on a
//! gate device with no uplink, and in a test with no validator.
//!
//! # What is decidable offline, and what is not
//!
//! A Solana address is nothing but a 32-byte Ed25519 public key in base58, so
//! four things are fully decidable from the string alone:
//!
//! 1. **The alphabet.** Base58 deliberately omits `0`, `O`, `I` and `l`; a
//!    string containing one is a transcription error, and [`validate`] says
//!    which character and which look-alike was probably meant.
//! 2. **The length.** Exactly 32 bytes decoded. This is what catches an
//!    address from another chain: a Stellar `G…` and an Ethereum `0x…` are
//!    both base58-shaped garbage of the wrong length, and a bare "invalid" is
//!    a far less useful thing to tell a person than "this looks like a Stellar
//!    address".
//! 3. **Whether the bytes are an Ed25519 point at all.** An *off-curve*
//!    32-byte account is a program-derived address — nobody holds a key for
//!    it, and it cannot sign. Every canonical associated token account is a
//!    PDA, so this check catches the specific and expensive mistake of pasting
//!    an ATA where a wallet belongs: [`patala_core::PaymentRail::charge`] derives the
//!    ATA *from* the destination, so an ATA destination would derive the ATA
//!    of an ATA and the money would land somewhere nobody can reach.
//! 4. **Whether it is a well-known program or mint.** Sending USDC to the SPL
//!    Token program, or to the USDC *mint* itself, is a classic unrecoverable
//!    mistake, and both are on-curve — the curve check alone does not catch
//!    them. See [`WELL_KNOWN_ACCOUNTS`].
//!
//! What is **not** decidable offline, and is deliberately not attempted here:
//!
//! - **Whether the account exists**, is rent-exempt, or has an initialized
//!   associated token account for this mint. All three need
//!   `getAccountInfo`/`getTokenAccountsByOwner`, i.e. a chain query, i.e. a
//!   different method than this one.
//! - **Whether an on-curve key is a plain wallet or something else.** A
//!   canonical ATA is off-curve and *is* caught (point 3), but a token account
//!   created from an ordinary keypair rather than derived is on-curve and is
//!   byte-for-byte indistinguishable from a wallet. Only the account's owner
//!   program, read from the chain, separates them.
//! - **Who owns it.** patala never tries to detect whether an address belongs
//!   to an exchange — see [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`], which
//!   rides on every verdict this module produces, including the most positive
//!   one.

use patala_core::DestinationVerdict;

use crate::tx;

/// The rail id every verdict from this module carries — the same string
/// [`patala_core::PaymentRail::id`] returns. A verdict is only ever an opinion about
/// the network *this* rail pays on.
pub const RAIL_ID: &str = "solana";

/// The base58 alphabet Solana (and Bitcoin) uses. `0`, `O`, `I` and `l` are
/// omitted on purpose — they are the four characters humans transcribe wrong.
const BASE58_ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// The look-alike a caller most likely meant when they typed an excluded
/// character. Base58's whole reason for excluding these four is that each is
/// confusable with a character it *does* use.
fn base58_lookalike(c: char) -> Option<&'static str> {
    match c {
        '0' => Some("the digit 0 is not in the base58 alphabet — did you mean the letter O? (base58 omits both, so re-copy the address rather than guessing)"),
        'O' => Some("the letter O is not in the base58 alphabet — did you mean the digit 0? (base58 omits both, so re-copy the address rather than guessing)"),
        'I' => Some("the capital I is not in the base58 alphabet — did you mean a lowercase l or the digit 1? (base58 omits I and l, so re-copy the address rather than guessing)"),
        'l' => Some("the lowercase l is not in the base58 alphabet — did you mean a capital I or the digit 1? (base58 omits I and l, so re-copy the address rather than guessing)"),
        _ => None,
    }
}

/// Addresses that decode perfectly but that no customer can ever be paid at,
/// paired with what each one actually is.
///
/// Every entry is on-curve *or* all-zeroes, so [`tx::is_on_curve`] does not
/// catch them — they need naming explicitly. Each is a documented, published
/// Solana address; `well_known_accounts_are_all_real_addresses` pins that
/// every one of them really does decode to 32 bytes, so a typo in this table
/// fails the build's test run rather than silently disabling a guard.
///
/// This list is **not** exhaustive and is not trying to be: it is not an
/// attribution database (see [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`]), it is
/// the short list of accounts that a person pasting from a block explorer, a
/// token list or a docs page actually ends up with by mistake.
pub const WELL_KNOWN_ACCOUNTS: &[(&str, &str)] = &[
    (
        "11111111111111111111111111111111",
        "the System Program (the all-zeroes account id)",
    ),
    (
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        "the SPL Token program",
    ),
    (
        "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
        "the SPL Token-2022 program",
    ),
    (
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
        "the Associated Token Account program",
    ),
    (
        "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
        "the SPL Memo v3 program",
    ),
    (
        "ComputeBudget111111111111111111111111111111",
        "the Compute Budget program",
    ),
    (
        "Stake11111111111111111111111111111111111111",
        "the Stake program",
    ),
    (
        "Vote111111111111111111111111111111111111111",
        "the Vote program",
    ),
    (
        "SysvarRent111111111111111111111111111111111",
        "the Rent sysvar",
    ),
    (
        "SysvarC1ock11111111111111111111111111111111",
        "the Clock sysvar",
    ),
    (
        "1nc1nerator11111111111111111111111111111111",
        "the incinerator — anything sent here is burned, permanently and by design",
    ),
    (
        tx::USDC_MAINNET_MINT,
        "the mainnet-beta USDC mint account itself, not a wallet holding USDC",
    ),
    (
        tx::USDC_DEVNET_MINT,
        "the devnet USDC mint account itself, not a wallet holding USDC",
    ),
    (
        "So11111111111111111111111111111111111111112",
        "the Wrapped SOL mint account itself, not a wallet",
    ),
];

/// A destination that is well-formed for some *other* chain, named. Only ever
/// used to turn a bare "invalid" into the sentence that actually saves the
/// money — never to accept anything.
///
/// Every shape here is one no valid Solana address can have (a Solana address
/// is 32 base58 bytes, so always 43 or 44 characters), which is why this can
/// run before the base58 checks without ever shadowing a real address.
fn foreign_format(dest: &str) -> Option<ForeignFormat> {
    let len = dest.len();
    let is_upper_base32 = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
    };

    // Stellar strkey: uppercase RFC4648 base32, one leading character per
    // type (SEP-23), 56 characters for the 32-byte payload types and 69 for a
    // muxed account.
    if is_upper_base32(dest) {
        let kind = match (dest.as_bytes()[0], len) {
            (b'S', 56) => return Some(ForeignFormat::StellarSecretSeed),
            (b'G', 56) => Some("a Stellar account address (G…)"),
            (b'C', 56) => Some("a Stellar/Soroban contract address (C…)"),
            (b'T', 56) => Some("a Stellar pre-authorized-transaction signer (T…)"),
            (b'X', 56) => Some("a Stellar hash-x signer (X…)"),
            (b'M', 69) => Some("a Stellar muxed account address (M…)"),
            (b'P', n) if n >= 56 => Some("a Stellar signed-payload signer (P…)"),
            _ => None,
        };
        if let Some(what) = kind {
            return Some(ForeignFormat::OtherChain(what));
        }
    }

    // Bitcoin, both eras. Gated on the length NOT being a Solana address
    // length, which is what makes this collision-proof by construction rather
    // than by argument: legacy addresses are 26-35 characters and bech32 ones
    // 42 (P2WPKH) or 62 (P2TR), so none of these can shadow a real 43- or
    // 44-character Solana address.
    if !matches!(len, 43 | 44) {
        // A legacy address is 25 bytes — 1 version + 20 hash160 + 4 checksum —
        // so this decodes rather than guesses. It has to: Solana's own System
        // Program id is `11111111111111111111111111111111`, which is 32
        // characters of base58 beginning with `1` and would otherwise be
        // mistaken for one. (It is also caught earlier, by exact identity;
        // this is the second of the two guards.)
        if matches!(len, 26..=35)
            && (dest.starts_with('1') || dest.starts_with('3'))
            && bs58::decode(dest).into_vec().map(|v| v.len()) == Ok(25)
        {
            return Some(ForeignFormat::OtherChain("a legacy Bitcoin address"));
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
                return Some(ForeignFormat::OtherChain("a bech32 Bitcoin address (bc1…)"));
            }
        }
    }

    // Hex-with-0x families. Base58 has no `0`, so no Solana address can begin
    // "0x" and these can never collide.
    if let Some(hex) = dest.strip_prefix("0x").or_else(|| dest.strip_prefix("0X")) {
        if hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return match hex.len() {
                40 => Some(ForeignFormat::OtherChain(
                    "an Ethereum/EVM address (0x + 40 hex characters)",
                )),
                64 => Some(ForeignFormat::OtherChain(
                    "a Sui or Aptos address (0x + 64 hex characters)",
                )),
                _ => None,
            };
        }
    }

    None
}

/// What [`foreign_format`] recognized. A secret seed is split out because it
/// is not a wrong-address problem at all — it is a disclosure.
enum ForeignFormat {
    /// A well-formed address belonging to a network this rail does not pay on.
    OtherChain(&'static str),
    /// A Stellar **private key**. Pasted into any destination field, on any
    /// rail, this is a key compromise.
    StellarSecretSeed,
}

/// The refusal for a pasted private key. Never quotes the key itself: a
/// [`DestinationVerdict::reason`] is meant to be shown to a person and is very
/// likely to be logged on the way there, and copying a secret into a log is
/// how a one-place disclosure becomes a many-place one.
fn secret_seed_refusal(what: &str) -> DestinationVerdict {
    DestinationVerdict::malformed(
        RAIL_ID,
        format!(
            "STOP — this is not an address, it is {what}. You have just pasted a PRIVATE KEY into \
             a destination field. Treat it as disclosed: create a new account, move everything \
             the old key controls to it, and never use the old key again. patala has not logged \
             the value and this message does not repeat it, but wherever you copied it from may \
             have. The address you want is the PUBLIC one derived from that key."
        ),
    )
}

/// Check a Solana destination address, offline. See the module docs for
/// exactly what this can and cannot decide.
///
/// Pure: the same `dest` always gives the same verdict, with no network, no
/// clock and no filesystem. No verdict — not even
/// [`patala_core::DestinationStatus::StructurallyValid`] — means "safe to send
/// to"; see
/// [`patala_core::EXCHANGE_DEPOSIT_CAVEAT`], which every verdict carries.
///
/// ```
/// use patala_core::DestinationStatus;
/// use patala_solana::destination::validate;
///
/// // A wallet: 32 base58 bytes, on the ed25519 curve.
/// let wallet = "6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs";
/// assert_eq!(validate(wallet).status, DestinationStatus::StructurallyValid);
///
/// // ...and even then a human confirms. There is no verdict that skips this.
/// assert!(validate(wallet).human_must_confirm);
///
/// // The SPL Token program is a perfectly valid address nobody can be paid at.
/// let v = validate("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// assert_eq!(v.status, DestinationStatus::NotAWallet);
/// assert!(v.is_refusal());
///
/// // A Stellar address is refused by name, not with a bare "invalid".
/// let v = validate("GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7");
/// assert_eq!(v.status, DestinationStatus::WrongNetwork);
/// assert!(v.reason.contains("Stellar"));
/// ```
pub fn validate(dest: &str) -> DestinationVerdict {
    if dest.trim().is_empty() {
        return DestinationVerdict::malformed(
            RAIL_ID,
            "an empty destination is not an address on any rail",
        );
    }

    // Fail closed on surrounding whitespace rather than silently trimming.
    // `SolanaRail::charge` does not trim either, so accepting here what
    // `charge` would reject would make this check a lie at the one moment it
    // matters.
    if dest.trim() != dest {
        return DestinationVerdict::malformed(
            RAIL_ID,
            "this address has leading or trailing whitespace, which came from the copy rather \
             than from the address — remove it and check nothing else was picked up with it",
        );
    }

    // An exact match against a real Solana account beats every shape
    // heuristic below, and is checked first so it can never lose to one. This
    // is not hypothetical: the System Program id
    // `11111111111111111111111111111111` is 32 characters of base58 starting
    // with `1`, which is also what a legacy Bitcoin address looks like.
    // Identity is knowledge; shape is a guess; knowledge wins.
    if let Some((_, what)) = WELL_KNOWN_ACCOUNTS.iter().find(|(a, _)| *a == dest) {
        return DestinationVerdict::not_a_wallet(
            RAIL_ID,
            format!(
                "this is {what}. It is a real Solana account, but no one holds a key for it and \
                 nothing sent to it can be recovered — by the recipient, by you, or by anyone. \
                 Ask for the recipient's own wallet address."
            ),
        );
    }

    // A private key in a destination field is a disclosure, and it is one
    // whether or not the rest of the string is well-formed. It has to be
    // reported as what it is, not as "invalid".
    match foreign_format(dest) {
        Some(ForeignFormat::StellarSecretSeed) => {
            return secret_seed_refusal("a Stellar secret seed (S…)")
        }
        Some(ForeignFormat::OtherChain(what)) => {
            return DestinationVerdict::wrong_network(
                RAIL_ID,
                format!(
                    "this looks like {what}, not a Solana address. The solana rail pays SPL USDC \
                     on Solana only; funds sent using an address from another chain do not arrive \
                     on that chain, they simply do not arrive. Ask for the recipient's Solana \
                     wallet address — 32 bytes of base58, 43 or 44 characters."
                ),
            )
        }
        None => {}
    }

    // The alphabet, with the specific character named. `bs58` would report
    // this as an opaque decode failure; which character is wrong is the part
    // a person can act on.
    if let Some(bad) = dest.chars().find(|c| !BASE58_ALPHABET.contains(*c)) {
        let detail = base58_lookalike(bad)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("the character {bad:?} is not in the base58 alphabet Solana addresses use")
            });
        return DestinationVerdict::malformed(
            RAIL_ID,
            format!("this is not a Solana address: {detail}."),
        );
    }

    // The length. `pubkey_from_base58` folds "not base58" and "not 32 bytes"
    // into one error; here they are already separated, so a failure at this
    // point can only be the length, and the byte count can be reported.
    let key = match tx::pubkey_from_base58(dest) {
        Ok(k) => k,
        Err(_) => {
            let decoded = bs58::decode(dest).into_vec().map(|v| v.len());
            let got = match decoded {
                Ok(n) => format!("{n} bytes"),
                // Unreachable in practice: every character passed the alphabet
                // check above. Fail closed rather than unwrap.
                Err(_) => "something base58 could not decode at all".to_string(),
            };
            return DestinationVerdict::malformed(
                RAIL_ID,
                format!(
                    "this is base58 but it decodes to {got}; a Solana address is exactly 32 \
                     bytes, which is always 43 or 44 characters. A string of the wrong length is \
                     usually an address for a different chain, or one that lost characters when \
                     it was copied."
                ),
            );
        }
    };

    // Off the curve means unsignable: a program-derived address. This is the
    // check that catches an associated token account, which is what a person
    // copying from a block explorer's token-holdings view most often has.
    if !tx::is_on_curve(&key.0) {
        return DestinationVerdict::not_a_wallet(
            RAIL_ID,
            "this is a valid 32-byte Solana account id, but it is not on the ed25519 curve, so \
             no private key can exist for it — it is a program-derived address (a program's own \
             account, or an associated token account). If it is an associated token account, \
             note that this rail derives the recipient's token account from their WALLET address \
             itself: give it the wallet, not the token account, or the transfer is built against \
             the token account of a token account and the funds are unreachable."
                .to_string(),
        );
    }

    DestinationVerdict::structurally_valid(
        RAIL_ID,
        "this is a well-formed Solana address: 32 bytes of base58, on the ed25519 curve, and not \
         a program or mint this rail knows of. That is every check possible without asking the \
         chain a question — whether the account exists, is rent-exempt, or already holds a USDC \
         token account are all chain queries, and whether the person on the other end can \
         actually be credited on it is not knowable at all. Confirm with them before paying.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use patala_core::DestinationStatus;

    /// A plain wallet: 32 bytes of base58 that decode to a point on the
    /// ed25519 curve. Asserted, not assumed — see
    /// `the_test_wallet_really_is_an_on_curve_32_byte_key`.
    const WALLET: &str = "6dNVeXf5rQrTVAvpjTv2oyeHiWMCGSCUuUkxYCK6bZTs";

    #[test]
    fn the_test_wallet_really_is_an_on_curve_32_byte_key() {
        // Every other test in this module reads WALLET as "the good case", so
        // that has to be a fact rather than a hope.
        let k = tx::pubkey_from_base58(WALLET).expect("32 bytes of base58");
        assert!(tx::is_on_curve(&k.0), "a wallet is a real ed25519 key");
        assert!(matches!(WALLET.len(), 43 | 44));
    }

    #[test]
    fn well_known_accounts_are_all_real_addresses() {
        // A typo in the table would silently switch a guard off, so the table
        // checks itself: every entry must decode to a genuine 32-byte account
        // id, and no entry may be listed twice.
        for (addr, what) in WELL_KNOWN_ACCOUNTS {
            let key = tx::pubkey_from_base58(addr)
                .unwrap_or_else(|e| panic!("{addr} ({what}) is not a 32-byte address: {e}"));
            assert_eq!(key.0.len(), 32);
            assert_eq!(
                WELL_KNOWN_ACCOUNTS
                    .iter()
                    .filter(|(a, _)| a == addr)
                    .count(),
                1,
                "{addr} is listed twice"
            );
        }
    }

    #[test]
    fn every_well_known_account_is_refused_as_not_a_wallet() {
        for (addr, what) in WELL_KNOWN_ACCOUNTS {
            let v = validate(addr);
            assert_eq!(
                v.status,
                DestinationStatus::NotAWallet,
                "{addr} ({what}) must be refused"
            );
            assert!(v.is_refusal(), "{addr} must fail closed");
        }
    }

    #[test]
    fn a_plain_wallet_is_structurally_valid_but_never_waives_the_human() {
        let v = validate(WALLET);
        assert_eq!(v.status, DestinationStatus::StructurallyValid);
        assert!(!v.is_refusal());
        assert_eq!(v.rail_id, "solana");
        // The whole point: the best verdict available is still not a green
        // light.
        assert!(v.human_must_confirm);
        assert!(v.requires_human_confirmation());
        assert_eq!(
            v.exchange_deposit_caveat,
            patala_core::EXCHANGE_DEPOSIT_CAVEAT
        );
    }

    #[test]
    fn an_associated_token_account_is_refused_because_nobody_can_sign_for_it() {
        // Derived, not hardcoded, so this stays true if the derivation
        // changes: every canonical ATA is off-curve by construction.
        let owner = tx::pubkey_from_base58(WALLET).unwrap();
        let mint = tx::pubkey_from_base58(tx::USDC_MAINNET_MINT).unwrap();
        let ata = tx::associated_token_address(&owner, &mint).unwrap();
        assert!(!tx::is_on_curve(&ata.0), "an ATA is a PDA");

        let v = validate(&tx::pubkey_to_base58(&ata));
        assert_eq!(v.status, DestinationStatus::NotAWallet);
        assert!(v.reason.contains("ed25519 curve"));
        // The message has to say what to give instead, not just what is wrong.
        assert!(v.reason.contains("wallet"));
    }

    #[test]
    fn a_stellar_address_is_named_rather_than_called_invalid() {
        // A real Stellar mainnet account address (56-character strkey).
        let v = validate("GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7");
        assert_eq!(v.status, DestinationStatus::WrongNetwork);
        assert!(v.is_refusal());
        assert!(
            v.reason.contains("Stellar"),
            "naming the format is the message that saves the money: {}",
            v.reason
        );
    }

    #[test]
    fn a_stellar_contract_and_a_muxed_account_are_also_named() {
        // Stellar Asset Contract for native XLM on mainnet (C…, 56 chars).
        let contract = validate("CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA");
        assert_eq!(contract.status, DestinationStatus::WrongNetwork);
        assert!(contract.reason.contains("contract"));

        // A muxed account (M…, 69 chars) from SEP-23's own examples.
        let muxed =
            validate("MA7QYNF7SOWQ3GLR2BGMZEHXAVIRZA4KVWLTJJFC7MGXUA74P7UJVAAAAAAAAAAAAAJLK");
        assert_eq!(muxed.status, DestinationStatus::WrongNetwork);
        assert!(muxed.reason.contains("muxed"));
    }

    #[test]
    fn a_pasted_stellar_secret_seed_is_a_disclosure_and_never_echoed() {
        // Deliberately a throwaway value, and the point of the test is that
        // it does NOT come back out.
        let seed = "SBFZCHU5645DOKRWYBXVOXY2ELGJKFRX6VGGPRYUWHQ7PMXNMYPOYKUB";
        let v = validate(seed);

        assert!(v.is_refusal(), "a leaked key must fail closed");
        assert!(
            v.reason.contains("PRIVATE KEY"),
            "the person needs to know what they just did: {}",
            v.reason
        );
        assert!(
            v.reason.contains("never use the old key again"),
            "and what to do about it: {}",
            v.reason
        );
        assert!(
            !v.reason.contains(seed),
            "a verdict is shown and logged — it must never repeat the secret"
        );
    }

    #[test]
    fn evm_and_sui_addresses_are_named_by_their_hex_length() {
        // Ethereum's own documentation example address (0x + 40 hex).
        let evm = validate("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert_eq!(evm.status, DestinationStatus::WrongNetwork);
        assert!(evm.reason.contains("Ethereum"));

        // A Sui/Aptos-shaped address (0x + 64 hex).
        let sui = validate("0x0000000000000000000000000000000000000000000000000000000000000002");
        assert_eq!(sui.status, DestinationStatus::WrongNetwork);
        assert!(sui.reason.contains("Sui"));
    }

    #[test]
    fn base58_lookalikes_are_named_character_by_character() {
        // Each of the four characters base58 omits, substituted into a real
        // address, must be reported as itself rather than as "invalid".
        for (bad, needle) in [
            ('0', "digit 0"),
            ('O', "letter O"),
            ('I', "capital I"),
            ('l', "lowercase l"),
        ] {
            let mut broken: String = WALLET.to_string();
            broken.replace_range(0..1, &bad.to_string());
            let v = validate(&broken);
            assert_eq!(v.status, DestinationStatus::Malformed, "{bad}");
            assert!(
                v.reason.contains(needle),
                "{bad} should be named: {}",
                v.reason
            );
        }
    }

    #[test]
    fn bitcoin_addresses_are_named_in_both_eras() {
        for dest in [
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2",         // legacy P2PKH
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", // bech32 P2WPKH
        ] {
            let v = validate(dest);
            assert_eq!(v.status, DestinationStatus::WrongNetwork, "{dest}");
            assert!(v.reason.contains("Bitcoin"), "{dest}: {}", v.reason);
        }
    }

    #[test]
    fn wrong_length_reports_the_byte_count_it_actually_decoded() {
        // Valid base58 of a length no chain in the table above claims: the
        // fallback must still say how many bytes it got, not just "invalid".
        let v = validate("3mJr7AoUXx2Wqd");
        assert_eq!(v.status, DestinationStatus::Malformed);
        assert!(v.reason.contains("bytes"), "{}", v.reason);
        assert!(v.reason.contains("32 bytes"), "{}", v.reason);
    }

    #[test]
    fn blank_and_whitespace_wrapped_destinations_fail_closed() {
        for blank in ["", " ", "\t\n"] {
            let v = validate(blank);
            assert_eq!(v.status, DestinationStatus::Malformed, "{blank:?}");
            assert!(v.is_refusal());
        }
        // A trimmable address is refused rather than silently accepted,
        // because `charge` does not trim either.
        let padded = format!(" {WALLET}\n");
        let v = validate(&padded);
        assert_eq!(v.status, DestinationStatus::Malformed);
        assert!(v.reason.contains("whitespace"));
        // ...and the same string without the padding is fine, so the refusal
        // really is about the whitespace.
        assert_eq!(
            validate(WALLET).status,
            DestinationStatus::StructurallyValid
        );
    }

    #[test]
    fn validation_is_pure_and_deterministic() {
        // Same input, same verdict — the property that lets this run in a
        // browser, on an offline gate device, and in a test with no RPC.
        for dest in [
            WALLET,
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
            "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7",
            "",
            "not an address",
        ] {
            assert_eq!(validate(dest), validate(dest), "{dest:?}");
        }
    }

    #[test]
    fn no_verdict_this_module_can_produce_is_a_green_light() {
        // Swept across every shape of input this module distinguishes.
        for dest in [
            WALLET,
            "11111111111111111111111111111111",
            tx::USDC_MAINNET_MINT,
            "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7",
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
