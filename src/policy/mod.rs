//! Central admin policy (PRD §6.3 E2.6).
//!
//! An operator sets a `policy` block in the server's `config.yaml` to constrain
//! which model providers/endpoints may be used, require redaction, and declare a
//! data-retention window. `beskar serve` enforces it for **every caller** — the
//! policy is the central governance point of the platform tier, so no API client
//! can escape it (callers never override the server's configured providers).
//!
//! Enforcement lives in [`crate::serve`]: the provider/endpoint rules are checked
//! per request (denied → HTTP 403), and `require_redaction` is checked at startup
//! (the server fails closed if redaction is required but disabled).

use serde::Deserialize;
use serde_json::{json, Value};

/// The `policy` block in `config.yaml`. All fields default to "no restriction",
/// so a config without a `policy` section behaves exactly as before.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct PolicyConfig {
    /// If non-empty, only these providers may be used (allow-list).
    #[serde(default)]
    pub allow_providers: Vec<String>,
    /// Providers that may never be used; takes precedence over the allow-list.
    #[serde(default)]
    pub deny_providers: Vec<String>,
    /// If non-empty, model-endpoint hosts must be on this list.
    #[serde(default)]
    pub allow_endpoints: Vec<String>,
    /// Require that pre-embedding redaction (E1.11) is enabled.
    #[serde(default)]
    pub require_redaction: bool,
    /// Declared data-retention window, in days (surfaced via `GET /v1/policy`).
    #[serde(default)]
    pub retention_days: Option<u32>,
}

/// A compiled central policy. Built once from [`PolicyConfig`]; provider/endpoint
/// names are normalized for case-insensitive matching.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    allow_providers: Vec<String>,
    deny_providers: Vec<String>,
    allow_endpoints: Vec<String>,
    require_redaction: bool,
    retention_days: Option<u32>,
}

fn normalize(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Policy {
    pub fn from_config(cfg: &PolicyConfig) -> Self {
        Policy {
            allow_providers: normalize(&cfg.allow_providers),
            deny_providers: normalize(&cfg.deny_providers),
            allow_endpoints: normalize(&cfg.allow_endpoints),
            require_redaction: cfg.require_redaction,
            retention_days: cfg.retention_days,
        }
    }

    /// `true` if any rule is configured (used for reporting).
    pub fn is_active(&self) -> bool {
        !self.allow_providers.is_empty()
            || !self.deny_providers.is_empty()
            || !self.allow_endpoints.is_empty()
            || self.require_redaction
            || self.retention_days.is_some()
    }

    pub fn require_redaction(&self) -> bool {
        self.require_redaction
    }

    /// Check that a provider + optional endpoint host is permitted for `role`
    /// (`"embed"` / `"generate"`). Returns `Err(reason)` if denied, where the
    /// reason is safe to return to a caller (no secrets).
    pub fn check_endpoint(&self, role: &str, provider: &str, host: Option<&str>) -> Result<(), String> {
        let p = provider.trim().to_lowercase();

        if self.deny_providers.contains(&p) {
            return Err(format!("{role} provider '{provider}' is denied by central policy"));
        }
        if !self.allow_providers.is_empty() && !self.allow_providers.contains(&p) {
            return Err(format!(
                "{role} provider '{provider}' is not in the central policy allow-list"
            ));
        }
        if !self.allow_endpoints.is_empty() {
            match host.map(|h| h.trim().to_lowercase()) {
                Some(h) if self.allow_endpoints.contains(&h) => {}
                Some(h) => {
                    return Err(format!(
                        "{role} endpoint host '{h}' is not in the central policy allow-list"
                    ))
                }
                None => {} // no resolvable host (e.g. a relative base_url): nothing to gate
            }
        }
        Ok(())
    }

    /// The policy as JSON, for the authenticated `GET /v1/policy` endpoint.
    pub fn as_json(&self) -> Value {
        json!({
            "allow_providers": self.allow_providers,
            "deny_providers": self.deny_providers,
            "allow_endpoints": self.allow_endpoints,
            "require_redaction": self.require_redaction,
            "retention_days": self.retention_days,
        })
    }

    /// A one-line human summary for `--verbose` / startup logging.
    pub fn summary(&self) -> String {
        if !self.is_active() {
            return "none".to_string();
        }
        let mut parts = Vec::new();
        if !self.allow_providers.is_empty() {
            parts.push(format!("allow_providers={:?}", self.allow_providers));
        }
        if !self.deny_providers.is_empty() {
            parts.push(format!("deny_providers={:?}", self.deny_providers));
        }
        if !self.allow_endpoints.is_empty() {
            parts.push(format!("allow_endpoints={:?}", self.allow_endpoints));
        }
        if self.require_redaction {
            parts.push("require_redaction=true".to_string());
        }
        if let Some(d) = self.retention_days {
            parts.push(format!("retention_days={d}"));
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(cfg: PolicyConfig) -> Policy {
        Policy::from_config(&cfg)
    }

    #[test]
    fn empty_policy_allows_everything() {
        let p = Policy::default();
        assert!(!p.is_active());
        assert!(p.check_endpoint("embed", "openai", Some("api.openai.com")).is_ok());
    }

    #[test]
    fn deny_list_blocks_provider_case_insensitively() {
        let p = policy(PolicyConfig {
            deny_providers: vec!["OpenAI".into()],
            ..Default::default()
        });
        let err = p.check_endpoint("generate", "openai", None).unwrap_err();
        assert!(err.contains("denied by central policy"));
        // A different provider is still allowed.
        assert!(p.check_endpoint("generate", "anthropic", None).is_ok());
    }

    #[test]
    fn allow_list_blocks_providers_not_listed() {
        let p = policy(PolicyConfig {
            allow_providers: vec!["ollama".into()],
            ..Default::default()
        });
        assert!(p.check_endpoint("embed", "ollama", None).is_ok());
        let err = p.check_endpoint("embed", "openai", None).unwrap_err();
        assert!(err.contains("not in the central policy allow-list"));
    }

    #[test]
    fn deny_takes_precedence_over_allow() {
        let p = policy(PolicyConfig {
            allow_providers: vec!["openai".into()],
            deny_providers: vec!["openai".into()],
            ..Default::default()
        });
        assert!(p.check_endpoint("embed", "openai", None).is_err());
    }

    #[test]
    fn endpoint_host_allowlist_is_enforced() {
        let p = policy(PolicyConfig {
            allow_endpoints: vec!["llm.internal".into()],
            ..Default::default()
        });
        assert!(p.check_endpoint("generate", "openai-compatible", Some("llm.internal")).is_ok());
        let err = p
            .check_endpoint("generate", "openai-compatible", Some("api.openai.com"))
            .unwrap_err();
        assert!(err.contains("not in the central policy allow-list"));
    }

    #[test]
    fn require_redaction_and_retention_are_reported() {
        let p = policy(PolicyConfig {
            require_redaction: true,
            retention_days: Some(90),
            ..Default::default()
        });
        assert!(p.is_active());
        assert!(p.require_redaction());
        assert_eq!(p.as_json()["retention_days"], 90);
    }
}
