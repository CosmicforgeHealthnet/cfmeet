//! The typed, bounded Context Policy.
//!
//! Authorization authority is never an arbitrary document: the policy is exactly these
//! four booleans (unknown members are refused, and the database CHECK refuses any other
//! shape). Defaults are fail-safe.

use serde::{Deserialize, Serialize};

/// Version of the policy semantics recorded with every context and session.
pub const CONTEXT_POLICY_VERSION: &str = "CONTEXT_POLICY_V1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    /// On REVOKE, actively terminate protected calls (new authority is denied regardless).
    #[serde(default = "yes")]
    pub terminate_active_calls_on_revoke: bool,
    /// On participant removal, actively terminate protected calls involving the removed
    /// participant. When false, existing calls may continue but no new protected call or
    /// transfer involving that participant is authorized.
    #[serde(default = "yes")]
    pub terminate_active_calls_on_participant_removal: bool,
    /// On SUSPEND, actively terminate protected calls (new authority is denied regardless).
    #[serde(default)]
    pub terminate_active_calls_on_suspend: bool,
    /// During the completion grace period, whether NEW calls may be authorized.
    #[serde(default)]
    pub grace_allows_new_calls: bool,
}

fn yes() -> bool {
    true
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            terminate_active_calls_on_revoke: true,
            terminate_active_calls_on_participant_removal: true,
            terminate_active_calls_on_suspend: false,
            grace_allows_new_calls: false,
        }
    }
}

/// Public (camelCase) shape of the policy in request and response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextPolicyBody {
    #[serde(default = "yes")]
    pub terminate_active_calls_on_revoke: bool,
    #[serde(default = "yes")]
    pub terminate_active_calls_on_participant_removal: bool,
    #[serde(default)]
    pub terminate_active_calls_on_suspend: bool,
    #[serde(default)]
    pub grace_allows_new_calls: bool,
}

impl From<ContextPolicyBody> for ContextPolicy {
    fn from(b: ContextPolicyBody) -> Self {
        Self {
            terminate_active_calls_on_revoke: b.terminate_active_calls_on_revoke,
            terminate_active_calls_on_participant_removal: b
                .terminate_active_calls_on_participant_removal,
            terminate_active_calls_on_suspend: b.terminate_active_calls_on_suspend,
            grace_allows_new_calls: b.grace_allows_new_calls,
        }
    }
}

impl From<ContextPolicy> for ContextPolicyBody {
    fn from(p: ContextPolicy) -> Self {
        Self {
            terminate_active_calls_on_revoke: p.terminate_active_calls_on_revoke,
            terminate_active_calls_on_participant_removal: p
                .terminate_active_calls_on_participant_removal,
            terminate_active_calls_on_suspend: p.terminate_active_calls_on_suspend,
            grace_allows_new_calls: p.grace_allows_new_calls,
        }
    }
}

impl ContextPolicy {
    /// Canonical persisted snapshot (snake_case, all four members).
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("policy serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_is_bounded_and_fail_safe() {
        let p: ContextPolicyBody = serde_json::from_str("{}").unwrap();
        assert_eq!(ContextPolicy::from(p), ContextPolicy::default());
        assert!(serde_json::from_str::<ContextPolicyBody>(r#"{"anything":{"nested":1}}"#).is_err());
        assert!(
            serde_json::from_str::<ContextPolicyBody>(r#"{"graceAllowsNewCalls":"yes"}"#).is_err()
        );
        let snapshot = ContextPolicy::default().snapshot();
        assert_eq!(snapshot.as_object().unwrap().len(), 4);
        assert!(snapshot
            .as_object()
            .unwrap()
            .values()
            .all(|v| v.is_boolean()));
    }
}
