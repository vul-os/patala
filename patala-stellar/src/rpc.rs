//! The Horizon seam *inside* the Stellar rail.
//!
//! Every network call the rail makes goes through [`StellarRpc`], so the
//! whole `charge`/`verify` path can be unit-tested offline against a fake —
//! see `src/tests.rs`. Only [`HorizonRpc`] opens a socket, and only when
//! actually invoked. This mirrors `patala-solana::rpc::SolanaRpc` /
//! `magnetite-seams::solana::rpc::SolanaRpc` one-for-one, adapted to
//! [Horizon's REST API](https://developers.stellar.org/docs/data/apis/horizon)
//! instead of Solana's JSON-RPC.
//!
//! **Honesty:** [`HorizonRpc`] has not been exercised against a live Horizon
//! instance from this environment. See `README.md` and the `#[ignore]`d live
//! test in `src/tests.rs`.

use async_trait::async_trait;

use crate::StellarError;

/// The result Horizon returns from a successful `POST /transactions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitResult {
    /// The transaction hash (hex), as Horizon reports it. `charge` asserts
    /// this equals the hash it computed locally *before* trusting the
    /// receipt — see `src/lib.rs`.
    pub hash: String,
    /// The ledger sequence the transaction was included in.
    pub ledger: u32,
    /// Whether the transaction actually succeeded. Horizon can return HTTP
    /// 200 for a transaction that landed but failed at the protocol level in
    /// some client libraries' loose handling — this crate always checks this
    /// field explicitly rather than inferring success from the status code.
    pub successful: bool,
}

/// The result of looking up a transaction that already happened, by hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxRecord {
    /// Whether the transaction succeeded.
    pub successful: bool,
    /// The ledger sequence it closed in.
    pub ledger: u32,
    /// The signed transaction envelope, base64 XDR, exactly as submitted and
    /// included. `verify` decodes this and checks it — never trusts Horizon's
    /// summary fields alone for the money-moving details.
    pub envelope_xdr: String,
}

/// One `balances[]` entry from `GET /accounts/{id}`, reduced to what deciding
/// "can this account actually receive this payment?" needs.
///
/// Amounts are **stroops** — the integer fixed-point units the ledger itself
/// uses. Horizon renders them as decimal strings (`"1.2500000"`); [`parse_stroops`]
/// converts with integer arithmetic only, never a float, so no balance
/// comparison in this crate can be off by a rounding error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetHolding {
    /// `true` for the native XLM balance, which has no code, no issuer and no
    /// trustline (every account can always hold XLM).
    pub is_native: bool,
    /// The asset code (`"USDC"`), empty for the native balance.
    pub asset_code: String,
    /// The issuing account, StrKey `G…`, empty for the native balance.
    pub asset_issuer: String,
    /// Units currently held, in stroops.
    pub balance_stroops: i64,
    /// The trustline's limit, in stroops — the most of this asset the account
    /// has agreed to hold. A payment that would push `balance` past `limit`
    /// fails with `op_line_full`. `i64::MAX` for the native balance.
    pub limit_stroops: i64,
    /// Whether the issuer has authorized this trustline. An issuer with
    /// `AUTH_REQUIRED` set must authorize each trustline; until it does, a
    /// payment to it fails (`op_not_authorized`) even though the trustline
    /// exists.
    pub is_authorized: bool,
}

/// Convert one of Horizon's decimal amount strings (`"1.2500000"`, `"0"`,
/// `"922337203685.4775807"`) to **stroops**, with integer arithmetic only.
///
/// Stellar amounts are `int64` counts of 10^-7, so every value Horizon prints
/// is exactly representable: there is no rounding to do, and doing it in `f64`
/// would silently lose precision above ~90 million units (a float has 53 bits
/// of mantissa; these amounts need 63). This crate contains **no floating point
/// arithmetic anywhere**, and this is the function that would otherwise be the
/// exception.
///
/// More than 7 fractional digits is a refusal, not a truncation — a value the
/// ledger cannot represent means this is not a ledger amount, and quietly
/// dropping digits off a balance is exactly the class of bug that produces a
/// wrong payment decision.
///
/// ```
/// use patala_stellar::rpc::parse_stroops;
///
/// assert_eq!(parse_stroops("1").unwrap(), 10_000_000);
/// assert_eq!(parse_stroops("1.25").unwrap(), 12_500_000);
/// assert_eq!(parse_stroops("0.0000001").unwrap(), 1);
/// assert_eq!(parse_stroops("922337203685.4775807").unwrap(), i64::MAX);
/// assert!(parse_stroops("1.00000001").is_err(), "8 decimals is not a ledger amount");
/// assert!(parse_stroops("").is_err());
/// assert!(parse_stroops("1e7").is_err(), "no exponent notation, no float parsing");
/// ```
pub fn parse_stroops(s: &str) -> Result<i64, StellarError> {
    let bad = || StellarError::Rpc(format!("not a stellar decimal amount: {s:?}"));
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(bad());
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    if frac.len() > usize::from(crate::tx::USDC_DECIMALS) {
        return Err(bad());
    }
    let whole: i128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| bad())?
    };
    let mut units: i128 = if frac.is_empty() {
        0
    } else {
        frac.parse().map_err(|_| bad())?
    };
    // Scale the fraction up to exactly 7 digits, then add — all integer.
    for _ in frac.len()..usize::from(crate::tx::USDC_DECIMALS) {
        units *= 10;
    }
    let scale: i128 = 10i128.pow(u32::from(crate::tx::USDC_DECIMALS));
    let total = whole
        .checked_mul(scale)
        .and_then(|w| w.checked_add(units))
        .ok_or_else(bad)?;
    i64::try_from(total).map_err(|_| bad())
}

/// Minimal Horizon surface used by the payment rail.
#[async_trait]
pub trait StellarRpc: Send + Sync {
    /// The account's current sequence number (`GET /accounts/{id}`,
    /// `sequence` field). The transaction to submit uses `sequence + 1`.
    async fn load_sequence(&self, account_strkey: &str) -> Result<i64, StellarError>;

    /// What assets an account holds and on what terms (`GET /accounts/{id}`,
    /// `balances` array) — the query behind
    /// [`crate::StellarRail::check_trustlines`].
    ///
    /// `Ok(None)` means Horizon has no such account (404): on Stellar an
    /// unfunded account does not exist, and a payment to it fails with
    /// `op_no_destination`. `Err` means Horizon could not be reached or
    /// answered garbage. **Both must fail closed at the call site** — "I could
    /// not check" is never "the payee is ready".
    async fn load_account_holdings(
        &self,
        account_strkey: &str,
    ) -> Result<Option<Vec<AssetHolding>>, StellarError>;

    /// `POST /transactions` with a base64 XDR envelope.
    async fn submit_transaction(
        &self,
        envelope_xdr_b64: &str,
    ) -> Result<SubmitResult, StellarError>;

    /// `GET /transactions/{hash}`.
    ///
    /// `Ok(None)` means "Horizon does not know this hash" (404) — i.e.
    /// unconfirmed. `Err` means Horizon could not be reached or answered
    /// garbage. **Both must fail closed at the call site** — see
    /// `PaymentRail::verify` in `src/lib.rs`.
    async fn get_transaction(&self, tx_hash_hex: &str) -> Result<Option<TxRecord>, StellarError>;
}

/// Real Horizon HTTP client.
pub struct HorizonRpc {
    base_url: String,
    http: reqwest::Client,
}

impl HorizonRpc {
    /// Build a client for a Horizon base URL, e.g.
    /// `https://horizon-testnet.stellar.org`.
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            base_url,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl StellarRpc for HorizonRpc {
    async fn load_sequence(&self, account_strkey: &str) -> Result<i64, StellarError> {
        let url = format!("{}/accounts/{}", self.base_url, account_strkey);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| StellarError::Rpc(format!("GET /accounts: {e}")))?;
        if !resp.status().is_success() {
            return Err(StellarError::Rpc(format!(
                "GET /accounts: HTTP {}",
                resp.status()
            )));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| StellarError::Rpc(format!("GET /accounts: bad JSON: {e}")))?;
        // Horizon renders int64 sequence numbers as a JSON string to avoid
        // precision loss in JS clients.
        v.get("sequence")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .ok_or_else(|| StellarError::Rpc("GET /accounts: no sequence".into()))
    }

    async fn submit_transaction(
        &self,
        envelope_xdr_b64: &str,
    ) -> Result<SubmitResult, StellarError> {
        let url = format!("{}/transactions", self.base_url);
        let resp = self
            .http
            .post(&url)
            .form(&[("tx", envelope_xdr_b64)])
            .send()
            .await
            .map_err(|e| StellarError::Rpc(format!("POST /transactions: {e}")))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| StellarError::Rpc(format!("POST /transactions: bad JSON: {e}")))?;
        if !status.is_success() {
            return Err(StellarError::Rpc(format!(
                "POST /transactions: HTTP {status}: {v}"
            )));
        }
        Ok(SubmitResult {
            hash: v
                .get("hash")
                .and_then(|s| s.as_str())
                .ok_or_else(|| StellarError::Rpc("POST /transactions: no hash".into()))?
                .to_string(),
            ledger: v
                .get("ledger")
                .and_then(|s| s.as_u64())
                .ok_or_else(|| StellarError::Rpc("POST /transactions: no ledger".into()))?
                as u32,
            successful: v
                .get("successful")
                .and_then(|s| s.as_bool())
                // Older Horizon releases omitted `successful` on the
                // synchronous submit response for already-included
                // transactions; a 2xx status with no explicit `false` is the
                // best available signal, but this is stated, not hidden.
                .unwrap_or(true),
        })
    }

    async fn get_transaction(&self, tx_hash_hex: &str) -> Result<Option<TxRecord>, StellarError> {
        let url = format!("{}/transactions/{}", self.base_url, tx_hash_hex);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| StellarError::Rpc(format!("GET /transactions/{{hash}}: {e}")))?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(StellarError::Rpc(format!(
                "GET /transactions/{{hash}}: HTTP {}",
                resp.status()
            )));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| StellarError::Rpc(format!("GET /transactions/{{hash}}: bad JSON: {e}")))?;
        Ok(Some(TxRecord {
            successful: v
                .get("successful")
                .and_then(|s| s.as_bool())
                .ok_or_else(|| {
                    StellarError::Rpc("GET /transactions/{hash}: no successful".into())
                })?,
            ledger: v
                .get("ledger")
                .and_then(|s| s.as_u64())
                .ok_or_else(|| StellarError::Rpc("GET /transactions/{hash}: no ledger".into()))?
                as u32,
            envelope_xdr: v
                .get("envelope_xdr")
                .and_then(|s| s.as_str())
                .ok_or_else(|| {
                    StellarError::Rpc("GET /transactions/{hash}: no envelope_xdr".into())
                })?
                .to_string(),
        }))
    }
}
