//! # patala-sidecar
//!
//! The universal polyglot path for `patala-core` (`PATALA.md` §5): a thin
//! local HTTP server exposing `quote`/`charge`/`verify`/`validate-destination`
//! /`webhook` as JSON, for any language that can make an HTTP call — no FFI, no
//! generated bindings, no `patala-py` required.
//!
//! ## Why a sidecar in addition to `patala-py`
//!
//! Two different problems, not a redundant second answer to the same one:
//! `patala-py` is a same-process binding — lowest latency, richest types,
//! but one dependency per consuming language. The sidecar is a separate
//! *process* — worse latency, JSON instead of native types, but it works
//! from literally any language with an HTTP client, and it buys a real
//! security property neither the Python binding nor a bare Rust dependency
//! gets you for free: **key isolation**. A non-custodial rail's signing key
//! lives inside whichever process calls `charge()`; if every product in a
//! polyglot stack links `patala-core` (or `patala-py`) directly, that key
//! (or the credentials to derive it) is smeared across every one of those
//! processes' memory. Route all of them through one sidecar instead, and the
//! key lives in exactly one hardened, narrowly-scoped process — see
//! `README.md` "Threat model" for what this does and does not buy you.
//!
//! ## Structure
//!
//! - [`registry`] — the `rail_id -> Arc<dyn PaymentRail>` map. `"mock"` is
//!   always present; real rails register here behind feature flags later,
//!   with **no change to any handler** (see that module's docs).
//! - [`auth`] — the local bearer-token gate. Fail-closed: no token in the
//!   environment means the sidecar refuses to start at all.
//! - [`api`] — the HTTP handlers, operating on `patala_core` types directly.
//! - [`app`] — wires the above into one `axum::Router`, used identically by
//!   `main.rs` (real process) and this crate's integration test (in-process).

pub mod api;
pub mod auth;
pub mod registry;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};

use api::AppState;
use auth::SidecarToken;
use registry::RailRegistry;

/// Build the full router: an unauthenticated `/healthz`, and the
/// token-gated `/v1/rails/...` surface. Shared by the real binary
/// (`main.rs`) and the in-process integration test, so a test that boots
/// this router is exercising exactly what production runs — not a stand-in.
pub fn app(registry: RailRegistry, token: SidecarToken) -> Router {
    let state = AppState {
        registry: Arc::new(registry),
    };

    let protected = Router::new()
        .route("/v1/rails/:rail_id", get(api::capabilities))
        .route("/v1/rails/:rail_id/quote", post(api::quote))
        .route("/v1/rails/:rail_id/charge", post(api::charge))
        .route("/v1/rails/:rail_id/verify", post(api::verify))
        .route(
            "/v1/rails/:rail_id/validate-destination",
            post(api::validate_destination),
        )
        .route("/v1/rails/:rail_id/webhook", post(api::webhook))
        .with_state(state)
        .route_layer(middleware::from_fn_with_state(token, auth::require_token));

    Router::new()
        .route("/healthz", get(api::healthz))
        .merge(protected)
}
