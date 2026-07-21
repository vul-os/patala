//! Local bearer-token auth. See `patala-sidecar/README.md` "Threat model" for
//! what this does and does not protect against.
//!
//! Fail-closed by construction: [`SidecarToken::from_env`] is the only way to
//! get a token, and it returns `Err` — not a default, not a randomly
//! generated fallback — if the environment variable is unset or empty. A
//! sidecar that cannot prove a caller was authorized refuses to start at all
//! rather than start unauthenticated (`PATALA.md` §8).

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// The env var the sidecar reads its auth token from. Never has a built-in
/// default value.
pub const TOKEN_ENV_VAR: &str = "PATALA_SIDECAR_TOKEN";

/// A loaded, non-empty local auth token.
#[derive(Clone)]
pub struct SidecarToken(String);

impl SidecarToken {
    /// Read [`TOKEN_ENV_VAR`] from the environment. `Err` if it is unset or
    /// empty — callers must fail closed on this, never fall back to running
    /// unauthenticated.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var(TOKEN_ENV_VAR) {
            Ok(v) if !v.trim().is_empty() => Ok(Self(v)),
            Ok(_) => Err(format!(
                "{TOKEN_ENV_VAR} is set but empty; refusing to start unauthenticated"
            )),
            Err(_) => Err(format!(
                "{TOKEN_ENV_VAR} is not set; refusing to start unauthenticated. \
                 Set it to a random local secret, e.g.: \
                 export {TOKEN_ENV_VAR}=$(openssl rand -hex 32)"
            )),
        }
    }

    /// Build directly from a string. Used by tests and by
    /// [`SidecarToken::from_env`]; production code should prefer `from_env`.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Constant-time comparison against a presented token, so a timing side
    /// channel can't be used to recover the token byte-by-byte. Length is not
    /// hidden (an early return on length mismatch is fine: the token space is
    /// large and random, so leaking its length is not a practical attack).
    fn matches(&self, presented: &str) -> bool {
        let expected = self.0.as_bytes();
        let presented = presented.as_bytes();
        if expected.len() != presented.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for (a, b) in expected.iter().zip(presented.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

/// Axum middleware: every request must carry `Authorization: Bearer <token>`
/// matching the sidecar's [`SidecarToken`], or the request is rejected with
/// `401` before it reaches any handler — including handlers that would only
/// read a quote, not move money. Fail-closed: a missing header, a malformed
/// header, or a non-matching token are all the same `401`, with no detail
/// about which.
pub async fn require_token(
    State(token): State<SidecarToken>,
    req: Request,
    next: Next,
) -> Response {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match presented {
        Some(p) if token.matches(p) => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_exact_token() {
        let t = SidecarToken::new("s3cr3t-token");
        assert!(t.matches("s3cr3t-token"));
        assert!(!t.matches("s3cr3t-tokeN"));
        assert!(!t.matches("s3cr3t-toke"));
        assert!(!t.matches(""));
    }

    // These three assertions share one test function (rather than three)
    // because they all mutate the process-wide `TOKEN_ENV_VAR` env var;
    // `cargo test` runs tests within a crate in parallel by default, and
    // separate `#[test]` fns here would race on that shared global state.
    #[test]
    fn from_env_fails_closed_unless_set_to_a_real_value() {
        std::env::remove_var(TOKEN_ENV_VAR);
        assert!(SidecarToken::from_env().is_err(), "unset must fail closed");

        std::env::set_var(TOKEN_ENV_VAR, "   ");
        assert!(
            SidecarToken::from_env().is_err(),
            "whitespace-only must fail closed"
        );

        std::env::set_var(TOKEN_ENV_VAR, "a-real-token");
        assert!(
            SidecarToken::from_env().is_ok(),
            "a real value must succeed"
        );

        std::env::remove_var(TOKEN_ENV_VAR);
    }
}
