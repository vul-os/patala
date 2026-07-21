//! Ed25519 keypair — ported from `magnetite-seams::identity::{PubKey, Sig,
//! RawKeypairAuth}` and trimmed to exactly what a payment rail needs (no
//! challenge/response login, no scoped tokens — that machinery belonged to
//! magnetite's `Identity`/`AuthProvider` seam, not to a `PaymentRail`).
//!
//! **PATALA.md §6:** Solana is Ed25519-native, so this keypair doubles as
//! *both* the signing identity presented to [`crate::SolanaRail`] and, because
//! a Solana wallet address is nothing but an Ed25519 public key, the wallet
//! that holds the funds. There is no separate identity → wallet mapping table
//! to keep in sync — the public key bytes *are* the address.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::SolanaError;

/// Ed25519 public key — 32 raw bytes; base58-encoded, it is a Solana address.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PubKey(pub [u8; 32]);

impl std::fmt::Debug for PubKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PubKey({})", hex::encode(self.0))
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

/// A signing keypair. Ported from magnetite's `RawKeypairAuth`, minus the
/// login/token machinery that crate also carried (out of scope for a payment
/// rail).
pub struct Keypair(SigningKey);

impl Keypair {
    /// Build from an explicit 32-byte seed (deterministic — handy for tests
    /// and for `SOLANA_KEYPAIR`/`SOLANA_KEYPAIR_PATH` loading).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    /// Generate a fresh random keypair from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// This keypair's public key — also its Solana wallet address (§6).
    pub fn pubkey(&self) -> PubKey {
        PubKey(self.0.verifying_key().to_bytes())
    }

    /// Sign a message with the secret key.
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

    /// Load a signing key from `SOLANA_KEYPAIR_PATH` (a solana-CLI JSON byte
    /// array; the file MUST be `chmod 600`) or `SOLANA_KEYPAIR` (a base58
    /// secret key).
    ///
    /// Returns `Ok(None)` when neither is set — a verify-only rail, which is
    /// the right posture for a process that never spends. The key material is
    /// never logged and error messages never quote it.
    pub fn from_env() -> Result<Option<Self>, SolanaError> {
        if let Ok(path) = std::env::var("SOLANA_KEYPAIR_PATH") {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| SolanaError::Config(format!("SOLANA_KEYPAIR_PATH {path}: {e}")))?;
            let bytes: Vec<u8> = serde_json::from_str(&raw).map_err(|_| {
                SolanaError::Config(format!("SOLANA_KEYPAIR_PATH {path}: not a JSON byte array"))
            })?;
            return Ok(Some(Self::from_bytes(&bytes)?));
        }
        if let Ok(b58) = std::env::var("SOLANA_KEYPAIR") {
            let bytes = bs58::decode(b58.trim())
                .into_vec()
                .map_err(|_| SolanaError::Config("SOLANA_KEYPAIR: not base58".into()))?;
            return Ok(Some(Self::from_bytes(&bytes)?));
        }
        Ok(None)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, SolanaError> {
        // Solana keypairs are 64 bytes: 32-byte seed followed by the public key.
        let seed: [u8; 32] = match bytes.len() {
            64 => bytes[..32].try_into().unwrap(),
            32 => bytes.try_into().unwrap(),
            n => {
                return Err(SolanaError::Config(format!(
                    "keypair must be 32 or 64 bytes, got {n}"
                )))
            }
        };
        Ok(Self::from_seed(seed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let k = Keypair::from_seed([1u8; 32]);
        let msg = b"patala-solana test message";
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
    fn keypair_length_is_enforced() {
        assert!(Keypair::from_bytes(&[1u8; 64]).is_ok());
        assert!(Keypair::from_bytes(&[1u8; 32]).is_ok());
        assert!(Keypair::from_bytes(&[1u8; 13]).is_err());
    }
}
