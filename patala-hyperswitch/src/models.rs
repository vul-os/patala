//! Wire DTOs for the subset of Hyperswitch's HTTP API this crate uses.
//!
//! Every shape here was checked against Hyperswitch's own published OpenAPI
//! spec, fetched directly from
//! `github.com/juspay/hyperswitch/api-reference/v1/openapi_spec_v1.json`
//! (schemas `PaymentsCreateRequest`, `PaymentsCreateResponseOpenApi`,
//! `PaymentsResponse`, `IntentStatus`, `RefundRequest`, `RefundResponse`,
//! `RefundStatus`), not guessed. See this crate's `README.md` "Sources"
//! section for the exact paths/lines relied on and what remains
//! NEEDS-CONFIRMATION.
//!
//! Only the fields this crate actually reads or sets are modelled -- both
//! request and response structs are intentionally partial. `#[serde(default)]`
//! and `Option<_>` are used liberally on the response side so an unmodelled
//! or a version-skewed field never breaks deserialization; a field this
//! crate never asked for is simply not decoded.
//!
//! Several response fields below (`connector`, refund `reason`/`error_message`,
//! the error envelope's `code`) are part of Hyperswitch's documented response
//! shape and kept here so the wire model is honest/complete and so a future
//! caller (or someone debugging a raw payload) can see the whole shape this
//! crate parses -- this thin client's own logic does not currently branch on
//! them. `#![allow(dead_code)]` below says so explicitly rather than silently
//! dropping fields that are genuinely part of what Hyperswitch returns.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// `POST /payments` request body (`PaymentsCreateRequest` in Hyperswitch's
/// OpenAPI spec). `amount` and `currency` are the only fields Hyperswitch
/// marks `required`; everything else here is optional there too.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PaymentsCreateRequest {
    /// Amount in the currency's smallest unit -- confirmed by Hyperswitch's
    /// own example (`"amount": 6540` for a $65.40 charge) and schema
    /// (`type: integer, format: int64`). This is exactly `PayRequest::amount_minor`,
    /// no conversion needed.
    pub amount: u64,
    /// ISO-4217 currency code, e.g. `"USD"`, `"NGN"`.
    pub currency: String,
    /// If `true`, Hyperswitch attempts to authorize immediately using
    /// whatever payment method reference is supplied. If omitted/`false`,
    /// the payment intent is created in `requires_payment_method` and goes
    /// no further -- this crate always sends `true` because a `PayRequest`
    /// carries no separate "confirm later" step in the `patala-core` seam.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
    /// **Design choice, not a Hyperswitch API fact (NEEDS-CONFIRMATION):**
    /// `patala-core`'s `PayRequest::destination` is documented as "an opaque
    /// processor-side destination token" for a fiat rail. This crate maps it
    /// to Hyperswitch's `payment_token` field -- a reference to a payment
    /// method already tokenized out-of-band (e.g. via Hyperswitch's own
    /// client-side SDK), so this crate never sees or transports raw card
    /// data. Confirmed present in Hyperswitch's own request examples; NOT
    /// confirmed against a live instance from here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_token: Option<String>,
    /// Caller-supplied idempotency key, echoed back. Hyperswitch's
    /// `payment_id` field doubles as its idempotency key per its own docs
    /// ("This ensures idempotency for multiple payments that have been done
    /// by a single merchant."). Optional: Hyperswitch generates one if
    /// omitted, but supplying `PayRequest::reference` here makes retries of
    /// the same patala-level request idempotent against Hyperswitch too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_id: Option<String>,
    /// Manually pin the Hyperswitch connector (processor) to route through,
    /// e.g. `["paystack"]`. Hyperswitch's schema: `connector: Connector[]`,
    /// "This allows to manually select a connector with which the payment
    /// can go through." Config-driven, never hardcoded -- see
    /// [`crate::HyperswitchConfig::connector`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector: Option<Vec<String>>,
}

/// The subset of `PaymentsCreateResponseOpenApi` / `PaymentsResponse` this
/// crate reads. Hyperswitch's create and retrieve endpoints return
/// differently-named schemas (`PaymentsCreateResponseOpenApi` vs
/// `PaymentsResponse`) but both carry every field modelled here with the
/// same names and types, so one struct serves both `charge()` and `verify()`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PaymentsResponse {
    pub payment_id: String,
    pub status: IntentStatus,
    /// Requested amount, minor units.
    pub amount: u64,
    /// Amount actually captured so far, minor units. `None`/`0` until
    /// capture happens.
    #[serde(default)]
    pub amount_received: Option<u64>,
    pub currency: String,
    /// Name of the Hyperswitch connector that processed/is processing this
    /// payment, e.g. `"paystack"`. `None` before Hyperswitch has picked one.
    #[serde(default)]
    pub connector: Option<String>,
    /// Present when the payment needs a customer-facing next step (e.g. 3DS
    /// redirect). Only the redirect-URL shape is modelled; other
    /// `NextActionData` variants (bank-transfer instructions, popup, SDK
    /// session token) are intentionally not decoded since this crate does
    /// not drive a checkout UI -- callers needing those should keep the raw
    /// Hyperswitch response themselves via the sidecar/HTTP layer, not through
    /// this opaque `Receipt`.
    #[serde(default)]
    pub next_action: Option<NextActionData>,
    /// Refunds already recorded against this payment. Used by `verify()` to
    /// fail closed on a payment that has since been (fully) refunded -- see
    /// Hyperswitch's own field description: "An array of refund objects
    /// associated with this payment."
    #[serde(default)]
    pub refunds: Option<Vec<RefundResponse>>,
}

/// Only the `redirect_to_url` variant of Hyperswitch's `NextActionData`
/// (a `oneOf` in its schema) is modelled -- it is the common card-3DS case.
/// Any other variant simply fails to deserialize into this shape and is
/// treated as "no actionable next step known to this crate", never as an
/// error -- see the `Option`-returning helper on [`PaymentsResponse`].
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NextActionData {
    #[serde(default)]
    pub redirect_to_url: Option<String>,
}

/// Hyperswitch `IntentStatus` -- the full enum, confirmed byte-for-byte
/// against the OpenAPI spec's `enum` list (17 variants). Only `Succeeded` is
/// a final-success state; the rest are either final-failure or still-pending,
/// and this crate's `verify()` treats anything but `Succeeded` as "not (yet)
/// settled", never as success.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntentStatus {
    Succeeded,
    Failed,
    Cancelled,
    CancelledPostCapture,
    Processing,
    RequiresCustomerAction,
    RequiresMerchantAction,
    RequiresPaymentMethod,
    RequiresConfirmation,
    RequiresCapture,
    PartiallyCaptured,
    PartiallyCapturedAndCapturable,
    PartiallyAuthorizedAndRequiresCapture,
    PartiallyCapturedAndProcessing,
    Conflicted,
    Expired,
    Review,
}

impl IntentStatus {
    /// The only status this crate will ever call "settled". Every other
    /// variant -- including all the `requires_*`/`processing`/`review`
    /// pending states -- must be reported as not-yet-settled, per
    /// `PATALA.md` §8 ("never fabricate ... a 'success' a rail didn't
    /// return") and this task's brief (do not report a pending payment as
    /// settled).
    pub(crate) fn is_settled_success(self) -> bool {
        matches!(self, IntentStatus::Succeeded)
    }
}

/// `POST /refunds` request body (`RefundRequest`). Hyperswitch marks only
/// `payment_id` as required; omitting `amount` means "refund the full
/// payment amount" per Hyperswitch's own examples.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RefundRequest {
    pub payment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refund_id: Option<String>,
}

/// `RefundResponse`, as returned by `POST /refunds` and `GET
/// /refunds/{refund_id}`, and embedded in `PaymentsResponse::refunds`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RefundResponse {
    pub refund_id: String,
    pub payment_id: String,
    pub amount: u64,
    pub currency: String,
    pub status: RefundStatus,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Hyperswitch `RefundStatus` -- confirmed against the OpenAPI spec's `enum`
/// list (4 variants; there is no separate "cancelled" state for a refund).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefundStatus {
    Succeeded,
    Failed,
    Pending,
    Review,
}

/// Hyperswitch's error envelope (`GenericErrorResponseOpenApi`: flat
/// `{error_type, message, code}`, all three `required` in the spec). Modelled
/// with every field `Option` anyway -- a body that doesn't even match this
/// shape must still produce an honest `Error::Rail`, not a second parse
/// panic on top of the original non-2xx.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ErrorResponse {
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}
