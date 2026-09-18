//! Per-operation trust policy: what a caller requires of a peer before it
//! sends that operation over A2A. Applied by the discovery client before
//! `create_from_card`, never by the protocol itself (see
//! `docs/trust-layer.md`, "What the manifest proves").
//!
//! Levels are cumulative:
//!
//! | Level | Requires |
//! | --- | --- |
//! | `Integrity` | the entry has a `trustManifest` with a `subject`, `sha256(card bytes) == subject.digest`, and the card's own JWS verifies |
//! | `Guaranteed` | + the catalog signature verifies under the operator key, the manifest is signed by a pinned guarantor, is within its validity window and restates the entry (`subject.type`, `subject.url`) |
//! | `VerifiedAccount` | + the manifest carries an `account-verified` attestation from that guarantor |
//!
//! Defaults map operations to levels; `TRUST_POLICY_MOCK`,
//! `TRUST_POLICY_LIVE` and `TRUST_POLICY_BOOKING` override them.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Policy {
    Integrity,
    Guaranteed,
    VerifiedAccount,
}

impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Self::Integrity => "integrity",
            Self::Guaranteed => "guaranteed",
            Self::VerifiedAccount => "verified-account",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "integrity" => Some(Self::Integrity),
            "guaranteed" => Some(Self::Guaranteed),
            "verified-account" | "verified_account" => Some(Self::VerifiedAccount),
            _ => None,
        }
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The level required for each kind of outgoing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policies {
    /// Mock availability between demo agents (`find_common_slot` on a mock agent).
    pub mock: Policy,
    /// Real availability (live booking pages, connected calendars).
    pub live: Policy,
    /// A booking that writes into someone's calendar (`commit_booking`).
    pub booking: Policy,
}

impl Default for Policies {
    fn default() -> Self {
        Self {
            mock: Policy::Integrity,
            live: Policy::Guaranteed,
            booking: Policy::VerifiedAccount,
        }
    }
}

impl Policies {
    /// Read overrides from the environment; an unknown value is a startup error.
    pub fn from_env() -> Result<Self, lambda_http::Error> {
        let mut policies = Self::default();
        for (variable, slot) in [
            ("TRUST_POLICY_MOCK", &mut policies.mock),
            ("TRUST_POLICY_LIVE", &mut policies.live),
            ("TRUST_POLICY_BOOKING", &mut policies.booking),
        ] {
            if let Ok(value) = std::env::var(variable) {
                *slot = Policy::parse(&value)
                    .ok_or_else(|| format!("{variable}: unknown trust policy `{value}`"))?;
            }
        }
        Ok(policies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_and_named() {
        assert!(Policy::Integrity < Policy::Guaranteed);
        assert!(Policy::Guaranteed < Policy::VerifiedAccount);
        for policy in [
            Policy::Integrity,
            Policy::Guaranteed,
            Policy::VerifiedAccount,
        ] {
            assert_eq!(Policy::parse(policy.name()), Some(policy));
        }
        assert_eq!(Policy::parse("open"), None);
        let defaults = Policies::default();
        assert_eq!(defaults.mock, Policy::Integrity);
        assert_eq!(defaults.live, Policy::Guaranteed);
        assert_eq!(defaults.booking, Policy::VerifiedAccount);
    }
}
