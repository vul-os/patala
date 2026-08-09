//! Ed25519 keypair, StrKey-encoded — `PATALA.md` §6.
//!
//! Stellar is Ed25519-native and its account addresses (`G...`) and secret
//! seeds (`S...`) are just [StrKey](https://developers.stellar.org/docs/encyclopedia/strkeys)
//! encodings of a raw 32-byte Ed25519 public key / seed (version byte +
//! payload + CRC16-XMODEM checksum, base32). That means the same identity
//! keypair an app already holds can double as its Stellar wallet key with
//! **no separate mapping table** — exactly the property `PATALA.md` §6 calls
//! out for Solana and preserves here.
//!
//! StrKey encode/decode itself is delegated to `stellar-strkey`, the Stellar
//! Development Foundation's own crate for this (Apache-2.0) — re-implementing
//! a checksum-and-version-byte format by hand is exactly the kind of "another
//! wire format" `PATALA.md` §1 says not to invent.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use stellar_strkey::ed25519::{PrivateKey as StrkeySeed, PublicKey as StrkeyPub};
use zeroize::Zeroize;

use crate::StellarError;

/// Ed25519 public key — 32 raw bytes. StrKey-encoded (`G...`) it is a Stellar
/// account address.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PubKey(pub [u8; 32]);

impl std::fmt::Debug for PubKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PubKey({})", self.to_strkey())
    }
}

impl PubKey {
    /// Encode as a Stellar account address (`G...`).
    pub fn to_strkey(&self) -> String {
        StrkeyPub(self.0).to_string()
    }

    /// Decode a Stellar account address (`G...`). Anything that is not a
    /// valid StrKey-encoded Ed25519 public key — bad checksum, wrong version
    /// byte, wrong length — is rejected, never coerced.
    pub fn from_strkey(s: &str) -> Result<Self, StellarError> {
        StrkeyPub::from_string(s)
            .map(|k| PubKey(k.0))
            .map_err(|_| StellarError::BadAddress(s.to_string()))
    }
}

/// Ed25519 signature — 64 raw bytes.
#[derive(Clone, Copy)]
pub struct Sig(pub [u8; 64]);

impl std::fmt::Debug for Sig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sig({}…)", hex::encode(&self.0[..8]))
    }
}

/// A signing keypair. The seed StrKey-encodes as `S...` (never logged, never
/// serialized, never written anywhere by this crate).
pub struct Keypair(SigningKey);

impl Keypair {
    /// Build from an explicit 32-byte seed (deterministic — handy for tests
    /// and for `STELLAR_SECRET_KEY` loading).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    /// Generate a fresh random keypair from the OS CSPRNG.
    pub fn generate() -> Self {
        // Straight into `SigningKey`, which is `ZeroizeOnDrop`: no
        // intermediate seed copy is made here to have to wipe.
        Self(SigningKey::generate(&mut rand_core::OsRng))
    }

    /// This keypair's public key — also its Stellar wallet address (§6).
    pub fn pubkey(&self) -> PubKey {
        PubKey(self.0.verifying_key().to_bytes())
    }

    /// Sign a message (here: always a 32-byte transaction hash) with the
    /// secret key.
    pub fn sign(&self, msg: &[u8]) -> Sig {
        Sig(self.0.sign(msg).to_bytes())
    }

    /// Verify a detached signature against a public key and message.
    pub fn verify(pk: &PubKey, msg: &[u8], sig: &Sig) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&pk.0) else {
            return false;
        };
        vk.verify(msg, &Signature::from_bytes(&sig.0)).is_ok()
    }

    /// Load a signing key from `STELLAR_SECRET_KEY` (a StrKey secret seed,
    /// `S...`) — the native Stellar convention, analogous to how
    /// `patala-solana` reads a base58 secret from `SOLANA_KEYPAIR`.
    ///
    /// Returns `Ok(None)` when unset — a verify-only rail, the right posture
    /// for a process that never spends. The key material is never logged and
    /// error messages never quote it.
    ///
    /// Every intermediate copy of the secret made here — the `S...` text out
    /// of the environment, the decoded `StrkeySeed`, the raw seed array — is
    /// wiped before this returns, on the error path as well as the success
    /// one. Only the copy inside `SigningKey` survives, and `ed25519-dalek`
    /// wipes that on drop. The process environment block itself holds a copy
    /// nothing in this crate can reach.
    pub fn from_env() -> Result<Option<Self>, StellarError> {
        let Ok(mut s) = std::env::var("STELLAR_SECRET_KEY") else {
            return Ok(None);
        };
        let parsed = StrkeySeed::from_string(s.trim());
        s.zeroize();
        let mut decoded = parsed.map_err(|_| {
            StellarError::Config("STELLAR_SECRET_KEY: not a valid S... seed".into())
        })?;
        let keypair = Self::from_seed(decoded.0);
        decoded.0.zeroize();
        Ok(Some(keypair))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let k = Keypair::from_seed([1u8; 32]);
        let msg = b"patala-stellar test message";
        let sig = k.sign(msg);
        assert!(Keypair::verify(&k.pubkey(), msg, &sig));
        assert!(!Keypair::verify(&k.pubkey(), b"different message", &sig));
    }

    #[test]
    fn seed_round_trips_so_a_wallet_can_be_persisted() {
        let a = Keypair::from_seed([9u8; 32]);
        let b = Keypair::from_seed([9u8; 32]);
        assert_eq!(a.pubkey().0, b.pubkey().0);
    }

    #[test]
    fn strkey_address_round_trips() {
        let k = Keypair::from_seed([3u8; 32]);
        let addr = k.pubkey().to_strkey();
        assert!(
            addr.starts_with('G'),
            "Stellar account addresses start with G"
        );
        let back = PubKey::from_strkey(&addr).unwrap();
        assert_eq!(back.0, k.pubkey().0);
    }

    #[test]
    fn strkey_rejects_garbage() {
        assert!(PubKey::from_strkey("not-a-strkey").is_err());
        assert!(PubKey::from_strkey("").is_err());
        // A well-formed seed (S...) is not a valid account address (G...).
        let seed_strkey = StrkeySeed([7u8; 32]).to_string();
        assert!(PubKey::from_strkey(&seed_strkey).is_err());
    }
}
