//! The provider-registry pattern — ported from cackle's
//! `internal/payments/registry.go`.
//!
//! `manual` is always registered and can never be disabled — mirroring
//! cackle's own framing verbatim: *"Cackle must always have a way to run a
//! real event with zero API keys, zero compliance surface, and zero network
//! calls, in any country."* `PATALA.md` makes the identical point about
//! `patala-core::MockRail` for the crypto side; `manual` (see [`crate::manual`])
//! is that same always-available default for the fiat side.
//!
//! **Not ported:** cackle's `Register` also refuses to register a provider
//! named `ProviderNameStub` if a real Paystack secret is configured
//! (`registry.go`'s `realPaystackSecretConfigured` defense-in-depth check).
//! This crate does not port cackle's `stub.go` (an auto-settling demo
//! provider) at all — it is out of scope for this foundation-plus-two-pilots
//! task (see `PORTING.md`) — so there is nothing for that guard to defend
//! against here.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use patala_core::{PaymentRail, RailCapabilities, RailClass};

/// Mirrors cackle's `EnvPaymentProviders` (`CACKLE_PAYMENT_PROVIDERS`).
pub const ENV_PATALA_FIAT_RAILS: &str = "PATALA_FIAT_RAILS";

/// Mirrors cackle's `ProviderNameManual`. Re-exported from [`crate::manual`]
/// as the single source of truth; kept here too since this is the module
/// that most directly cares about the "always enabled" contract.
pub use crate::manual::RAIL_ID_MANUAL;

/// Mirrors cackle's `ErrProviderNotFound` / `ErrProviderDisabled` /
/// `ErrCurrencyNotSupported`, plus two registration-time errors
/// (`Register`'s own nil-name/duplicate-name checks in cackle, which are
/// plain `errors.New`/`fmt.Errorf` there rather than named sentinels — given
/// names here since Rust callers match on enum variants, not string
/// prefixes).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Mirrors `ErrProviderNotFound`.
    #[error("patala-fiat: rail {0:?} is not registered")]
    RailNotFound(String),
    /// Mirrors `ErrProviderDisabled`.
    #[error("patala-fiat: rail {0:?} is registered but not enabled (see PATALA_FIAT_RAILS)")]
    RailDisabled(String),
    /// Mirrors `ErrCurrencyNotSupported`.
    #[error("patala-fiat: rail {0:?} does not support currency {1:?}")]
    CurrencyNotSupported(String, String),
    /// Mirrors `Register`'s `"provider Name() must not be empty"` check.
    #[error("patala-fiat: cannot register a rail with an empty id")]
    EmptyRailId,
    /// Mirrors `Register`'s `"provider %q already registered"` check.
    #[error("patala-fiat: rail {0:?} is already registered")]
    AlreadyRegistered(String),
}

/// Narrows [`Registry::list`] to rails matching every non-empty field.
///
/// **Gap vs cackle** (see `PORTING.md`): cackle's `CapabilityFilter` also has
/// `Country` (ISO-3166-1 alpha-2 merchant country) and `Flow`
/// (redirect/inline/manual/invoice) fields. `patala_core::RailCapabilities`
/// has neither a country nor a flow concept at all, so there is nothing for
/// those two fields to filter on here — only `currency` and `class` (the one
/// piece of information `RailCapabilities` has that cackle's `Capabilities`
/// does not: the settlement class, `PATALA.md` §3).
#[derive(Default, Clone)]
pub struct CapabilityFilter {
    pub currency: Option<String>,
    pub class: Option<RailClass>,
}

/// Mirrors cackle's `Capabilities.SupportsCurrency`: an empty
/// `currencies` list means unrestricted.
fn supports_currency(caps: &RailCapabilities, code: &str) -> bool {
    if caps.currencies.is_empty() {
        return true;
    }
    caps.currencies.iter().any(|c| c.eq_ignore_ascii_case(code))
}

/// Mirrors cackle's `Registry`. Looks up rails by name so callers never
/// hardcode one, and filters by capability and by which rails this
/// deployment has chosen to enable.
pub struct Registry {
    rails: RwLock<HashMap<String, Arc<dyn PaymentRail>>>,
    /// `None` mirrors `NewRegistry`: every REGISTERED rail is enabled (the
    /// permissive default for tests / single-rail setups). `Some(set)`
    /// mirrors `NewRegistryFromEnv` once `PATALA_FIAT_RAILS` is non-empty:
    /// only the listed names (plus `manual`, always) are enabled.
    enabled: Option<HashSet<String>>,
}

impl Registry {
    /// Mirrors cackle's `NewRegistry`.
    pub fn new() -> Self {
        Self {
            rails: RwLock::new(HashMap::new()),
            enabled: None,
        }
    }

    /// Mirrors cackle's `NewRegistryFromEnv`: `env` is a comma-separated
    /// list of rail ids (case-insensitive, whitespace trimmed). An
    /// empty/unset `env` enables every rail that ends up registered
    /// (matching [`Registry::new`]'s permissive default). Once `env` is
    /// non-empty, ONLY the listed ids (plus `manual`, unconditionally) are
    /// enabled.
    pub fn from_env(env: &str) -> Self {
        let mut set = HashSet::new();
        let mut any = false;
        for part in env.split(',') {
            let name = part.trim().to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            set.insert(name);
            any = true;
        }
        let enabled = if any {
            set.insert(RAIL_ID_MANUAL.to_string());
            Some(set)
        } else {
            None
        };
        Self {
            rails: RwLock::new(HashMap::new()),
            enabled,
        }
    }

    /// Mirrors cackle's `Register`. Refuses an empty id or a duplicate id.
    /// Does NOT consult the enablement allowlist — registering a rail and
    /// enabling it for selection are separate concerns, exactly as in
    /// cackle.
    pub fn register(&self, rail: Arc<dyn PaymentRail>) -> Result<(), RegistryError> {
        let name = rail.id().trim().to_string();
        if name.is_empty() {
            return Err(RegistryError::EmptyRailId);
        }
        let mut rails = self.rails.write().expect("registry lock poisoned");
        if rails.contains_key(&name) {
            return Err(RegistryError::AlreadyRegistered(name));
        }
        rails.insert(name, rail);
        Ok(())
    }

    /// Mirrors cackle's `Get`: looks up a rail by id, ignoring enablement.
    pub fn get(&self, name: &str) -> Option<Arc<dyn PaymentRail>> {
        self.rails
            .read()
            .expect("registry lock poisoned")
            .get(name)
            .cloned()
    }

    /// Mirrors cackle's `Names`: every REGISTERED rail id, sorted,
    /// regardless of enablement.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .rails
            .read()
            .expect("registry lock poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Mirrors cackle's `IsEnabled`. `manual` is always enabled. Does NOT
    /// require `name` to be registered.
    pub fn is_enabled(&self, name: &str) -> bool {
        if name == RAIL_ID_MANUAL {
            return true;
        }
        match &self.enabled {
            None => true,
            Some(set) => set.contains(&name.to_ascii_lowercase()),
        }
    }

    /// Mirrors cackle's `List`: every ENABLED, registered rail matching
    /// `filter`, sorted by id. Disabled rails are never returned even if
    /// they'd otherwise match.
    pub fn list(&self, filter: CapabilityFilter) -> Vec<Arc<dyn PaymentRail>> {
        let mut all: Vec<Arc<dyn PaymentRail>> = self
            .rails
            .read()
            .expect("registry lock poisoned")
            .values()
            .cloned()
            .collect();
        all.sort_by(|a, b| a.id().cmp(b.id()));

        all.into_iter()
            .filter(|r| self.is_enabled(r.id()))
            .filter(|r| match &filter.currency {
                Some(cur) => supports_currency(r.capabilities(), cur),
                None => true,
            })
            .filter(|r| match filter.class {
                Some(class) => r.capabilities().class == class,
                None => true,
            })
            .collect()
    }

    /// Mirrors cackle's `Select`: returns `name` only if it is both
    /// registered and enabled, and (when `currency` is given) its
    /// capabilities support that currency. This turns "picked a rail that
    /// can't actually handle this currency" into a clear, early, typed
    /// error instead of a confusing failure deep inside `charge()`.
    pub fn select(
        &self,
        name: &str,
        currency: Option<&str>,
    ) -> Result<Arc<dyn PaymentRail>, RegistryError> {
        let rail = self
            .get(name)
            .ok_or_else(|| RegistryError::RailNotFound(name.to_string()))?;
        if !self.is_enabled(name) {
            return Err(RegistryError::RailDisabled(name.to_string()));
        }
        if let Some(cur) = currency {
            if !supports_currency(rail.capabilities(), cur) {
                return Err(RegistryError::CurrencyNotSupported(
                    name.to_string(),
                    cur.to_string(),
                ));
            }
        }
        Ok(rail)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manual::ManualRail;

    fn manual_rail() -> Arc<dyn PaymentRail> {
        Arc::new(ManualRail::default())
    }

    #[test]
    fn manual_is_always_enabled_even_when_unregistered() {
        let r = Registry::new();
        assert!(r.is_enabled(RAIL_ID_MANUAL));
        let r = Registry::from_env("some-other-rail");
        assert!(r.is_enabled(RAIL_ID_MANUAL));
    }

    #[test]
    fn register_then_get_and_names() {
        let r = Registry::new();
        r.register(manual_rail()).unwrap();
        assert!(r.get(RAIL_ID_MANUAL).is_some());
        assert_eq!(r.names(), vec![RAIL_ID_MANUAL.to_string()]);
    }

    #[test]
    fn register_rejects_duplicate() {
        let r = Registry::new();
        r.register(manual_rail()).unwrap();
        let err = r.register(manual_rail()).unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyRegistered(_)));
    }

    #[test]
    fn empty_env_enables_every_registered_rail() {
        let r = Registry::from_env("");
        r.register(manual_rail()).unwrap();
        assert!(r.is_enabled(RAIL_ID_MANUAL));
    }

    #[test]
    fn non_empty_env_restricts_to_listed_rails_plus_manual() {
        let r = Registry::from_env("stripe, PAYSTACK");
        assert!(r.is_enabled("stripe"));
        assert!(r.is_enabled("paystack"));
        assert!(r.is_enabled(RAIL_ID_MANUAL));
        assert!(!r.is_enabled("hyperswitch"));
    }

    #[test]
    fn select_fails_closed_on_not_found_disabled_and_currency() {
        let r = Registry::from_env("manual"); // only manual explicitly enabled
        match r.select("nonexistent", None) {
            Err(RegistryError::RailNotFound(_)) => {}
            Err(other) => panic!("expected RailNotFound, got {other:?}"),
            Ok(_) => panic!("expected RailNotFound, got Ok"),
        }

        r.register(manual_rail()).unwrap();
        // manual is always enabled and unrestricted on currency.
        assert!(r.select(RAIL_ID_MANUAL, Some("ZAR")).is_ok());
    }

    #[test]
    fn list_filters_by_enablement_and_currency() {
        let r = Registry::from_env("manual");
        r.register(manual_rail()).unwrap();
        let all = r.list(CapabilityFilter::default());
        assert_eq!(all.len(), 1);
        let filtered = r.list(CapabilityFilter {
            currency: Some("JPY".into()),
            class: None,
        });
        assert_eq!(filtered.len(), 1, "manual is unrestricted on currency");
    }
}
