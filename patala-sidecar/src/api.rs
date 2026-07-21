//! HTTP handlers. Every payload here is a `patala_core` type used directly —
//! `PayRequest`, `Quote`, `Receipt`, `RailCapabilities` already `derive`
//! `Serialize`/`Deserialize` in `patala-core`, so this module does not
//! hand-roll a second set of DTOs that could drift from the trait's actual
//! shape. Amounts stay the `u64` minor-units integers `patala-core` defines —
//! `serde_json` encodes a `u64` as a JSON number, never a float
//! (`PATALA.md` §3, §8).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use patala_core::{Error as CoreError, PayRequest, Receipt};

use crate::registry::RailRegistry;

/// Shared state every handler gets: the rail registry. `Arc`-wrapped so
/// cloning it per-request (axum's `State` extractor requires `Clone`) is
/// just a refcount bump, not a copy of every rail.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RailRegistry>,
}

/// Unauthenticated liveness check. Deliberately reveals nothing about which
/// rails are configured — that's behind the token, everything else isn't.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Every error this module's handlers can produce: either a `patala_core`
/// operation error, or "no rail is registered under that id" (which
/// `patala-core` itself has no opinion about — routing by id is this
/// sidecar's job, not the trait's).
pub enum ApiError {
    UnknownRail(String),
    Core(CoreError),
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        ApiError::Core(e)
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    kind: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind, message) = match &self {
            ApiError::UnknownRail(id) => (
                StatusCode::NOT_FOUND,
                "unknown_rail",
                format!("no rail is registered under id {id:?}"),
            ),
            // Fail-closed mapping: an operational rail failure is a 502 (the
            // rail, not this sidecar, is at fault); a malformed request is a
            // 400; `Unsupported` is a 501 (the operation is understood but
            // this rail genuinely cannot do it, e.g. refund on a
            // NonCustodialFinal rail); the cross-class guard and
            // all-rails-failed cases are 409/502 respectively. None of these
            // ever come back as a bare 200 — `verify` returning `Ok(false)`
            // is handled separately, in `verify_receipt`, precisely so a
            // rail's honest "this doesn't verify" is never confused with a
            // transport-level error here.
            ApiError::Core(CoreError::InvalidRequest(msg)) => {
                (StatusCode::BAD_REQUEST, "invalid_request", msg.clone())
            }
            ApiError::Core(CoreError::Unsupported(op)) => (
                StatusCode::NOT_IMPLEMENTED,
                "unsupported",
                format!("{op} is not supported by this rail"),
            ),
            ApiError::Core(CoreError::Rail(msg)) => {
                (StatusCode::BAD_GATEWAY, "rail_error", msg.clone())
            }
            ApiError::Core(CoreError::CrossClassFailover { from, to }) => (
                StatusCode::CONFLICT,
                "cross_class_failover",
                format!("failover from {from:?} to {to:?} was not opted into"),
            ),
            ApiError::Core(CoreError::AllRailsFailed) => (
                StatusCode::BAD_GATEWAY,
                "all_rails_failed",
                "no rail satisfied the request".to_string(),
            ),
        };
        (
            status,
            Json(ErrorBody {
                error: message,
                kind,
            }),
        )
            .into_response()
    }
}

fn lookup<'a>(
    state: &'a AppState,
    rail_id: &str,
) -> Result<&'a Arc<dyn patala_core::PaymentRail>, ApiError> {
    state
        .registry
        .get(rail_id)
        .ok_or_else(|| ApiError::UnknownRail(rail_id.to_string()))
}

/// `GET /v1/rails/:rail_id` — the capability descriptor. A caller decides its
/// whole UX contract (refundable-pending-plus-card-form vs
/// wallet-address-plus-final-receipt) from this response's `class` field,
/// without knowing which provider answered (`PATALA.md` §3).
pub async fn capabilities(
    State(state): State<AppState>,
    Path(rail_id): Path<String>,
) -> Result<Json<patala_core::RailCapabilities>, ApiError> {
    let rail = lookup(&state, &rail_id)?;
    Ok(Json(rail.capabilities().clone()))
}

/// `POST /v1/rails/:rail_id/quote` — fees/fx/expiry, no money moved.
pub async fn quote(
    State(state): State<AppState>,
    Path(rail_id): Path<String>,
    Json(req): Json<PayRequest>,
) -> Result<Json<patala_core::Quote>, ApiError> {
    let rail = lookup(&state, &rail_id)?;
    let quote = rail.quote(&req).await?;
    Ok(Json(quote))
}

/// `POST /v1/rails/:rail_id/charge` — initiates/settles a payment, returns
/// the `Receipt` entitlement.
pub async fn charge(
    State(state): State<AppState>,
    Path(rail_id): Path<String>,
    Json(req): Json<PayRequest>,
) -> Result<Json<Receipt>, ApiError> {
    let rail = lookup(&state, &rail_id)?;
    let receipt = rail.charge(&req).await?;
    Ok(Json(receipt))
}

#[derive(Serialize)]
pub struct VerifyResponse {
    pub valid: bool,
}

/// `POST /v1/rails/:rail_id/verify` — re-derive whether a receipt still
/// holds. **This is the entitlement check** (`patala_core::Receipt`'s docs):
/// a `200` with `{"valid": false}` is the honest, fail-closed answer for an
/// unverifiable receipt — it is never surfaced as an HTTP error, so a caller
/// cannot mistake "verified false" for "the sidecar broke".
pub async fn verify(
    State(state): State<AppState>,
    Path(rail_id): Path<String>,
    Json(receipt): Json<Receipt>,
) -> Result<Json<VerifyResponse>, ApiError> {
    let rail = lookup(&state, &rail_id)?;
    let valid = rail.verify(&receipt).await?;
    Ok(Json(VerifyResponse { valid }))
}
