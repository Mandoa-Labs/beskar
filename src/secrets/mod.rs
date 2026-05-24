//! Pluggable secret backends and secret redaction (PRD §6.2 E1.1, E1.3).
//!
//! Config values that hold secrets (`pat`, `pgpassword`, `anthropic_key`, and
//! per-endpoint API keys) may be either a literal or a `scheme://` reference
//! that is resolved at runtime from a backend:
//!
//! ```text
//! pgpassword: azure-keyvault://mykv/beskar-pgpassword
//! pat:        env://OPENAI_API_KEY
//! pgpassword: secret://beskar-pgpassword   # backend from BESKAR_SECRET_BACKEND
//! ```
//!
//! `azure-keyvault` and `env` ship in M5; `vault`, `aws-secrets`, and
//! `gcp-secrets` are recognized and stubbed for follow-up milestones.

use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

use crate::net::HttpClient;

mod azure_keyvault;

/// Backends recognized as `scheme://` prefixes in config values.
const KNOWN_SCHEMES: &[&str] = &[
    "azure-keyvault",
    "env",
    "secret", // resolves via the default backend (config / BESKAR_SECRET_BACKEND)
    "vault",
    "aws-secrets",
    "gcp-secrets",
];

/// A parsed `scheme://location` secret reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    pub scheme: String,
    /// Everything after `scheme://` — interpreted per backend.
    pub location: String,
}

/// Parse a config value into a [`SecretRef`] if it is a `scheme://...` reference
/// for a known backend. A bare literal (e.g. `sk-...`) returns `None`.
pub fn parse_reference(value: &str) -> Option<SecretRef> {
    let (scheme, location) = value.split_once("://")?;
    if KNOWN_SCHEMES.contains(&scheme) {
        Some(SecretRef {
            scheme: scheme.to_string(),
            location: location.to_string(),
        })
    } else {
        None
    }
}

/// `true` if the value is a literal secret on disk (non-empty, not a reference).
pub fn is_literal_secret(value: &str) -> bool {
    !value.is_empty() && parse_reference(value).is_none()
}

/// A backend that turns a [`SecretRef`] into a secret value.
pub trait SecretBackend {
    fn resolve(&self, reference: &SecretRef, http: &HttpClient) -> Result<String>;
}

/// Resolve a config value: pass literals through unchanged, dispatch references
/// to the appropriate backend. `default_backend` (config `secret_backend` or
/// `BESKAR_SECRET_BACKEND`) is used for the generic `secret://` scheme.
pub fn resolve_value(
    value: &str,
    default_backend: Option<&str>,
    http: &HttpClient,
) -> Result<String> {
    let reference = match parse_reference(value) {
        Some(r) => r,
        None => return Ok(value.to_string()),
    };

    let scheme = if reference.scheme == "secret" {
        default_backend.context(
            "config value uses the generic 'secret://' scheme but no default backend is set; \
             set 'secret_backend' in config or BESKAR_SECRET_BACKEND",
        )?
    } else {
        reference.scheme.as_str()
    };

    let resolved = match scheme {
        "env" => resolve_env(&reference)?,
        "azure-keyvault" => azure_keyvault::AzureKeyVault.resolve(&reference, http)?,
        "vault" | "aws-secrets" | "gcp-secrets" => bail!(
            "secret backend '{scheme}' is not yet implemented \
             (M5 ships 'azure-keyvault' and 'env'; '{scheme}' is stubbed for a later milestone)"
        ),
        other => bail!("unknown secret backend scheme '{other}'"),
    };
    Ok(resolved)
}

fn resolve_env(reference: &SecretRef) -> Result<String> {
    let var = reference.location.trim();
    std::env::var(var)
        .with_context(|| format!("env secret backend: environment variable '{var}' is not set"))
}

// ---------------------------------------------------------------------------
// Redaction registry (E1.3)
//
// Resolved secret values are registered here so that error messages, the
// `--verbose` config dump, and API error bodies can be scrubbed before they
// reach stdout/stderr or any log.
// ---------------------------------------------------------------------------

fn registry() -> &'static Mutex<Vec<String>> {
    static REGISTRY: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a live secret so it can be redacted from any user-facing output.
/// Very short values are ignored to avoid scrubbing innocuous substrings.
pub fn register_secret(secret: &str) {
    let secret = secret.trim();
    if secret.len() < 6 {
        return;
    }
    if let Ok(mut reg) = registry().lock() {
        if !reg.iter().any(|s| s == secret) {
            reg.push(secret.to_string());
        }
    }
}

/// Replace every registered secret in `text` with `***REDACTED***`.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    if let Ok(reg) = registry().lock() {
        for secret in reg.iter() {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "***REDACTED***");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_scheme_references() {
        let r = parse_reference("azure-keyvault://mykv/beskar-pgpassword").unwrap();
        assert_eq!(r.scheme, "azure-keyvault");
        assert_eq!(r.location, "mykv/beskar-pgpassword");
    }

    #[test]
    fn literal_secrets_are_not_references() {
        assert!(parse_reference("sk-proj-abc123").is_none());
        assert!(parse_reference("hunter2").is_none());
        assert!(is_literal_secret("sk-proj-abc123"));
        assert!(!is_literal_secret("env://OPENAI_API_KEY"));
        assert!(!is_literal_secret(""));
    }

    #[test]
    fn unknown_scheme_is_treated_as_literal() {
        // A password that happens to contain "://" but isn't a known backend.
        assert!(parse_reference("postgres://localhost/db").is_none());
    }

    #[test]
    fn env_backend_resolves_from_environment() {
        std::env::set_var("BESKAR_TEST_SECRET", "topsecret-value");
        let http = HttpClient::new(crate::net::EgressPolicy::default(), None).unwrap();
        let got = resolve_value("env://BESKAR_TEST_SECRET", None, &http).unwrap();
        assert_eq!(got, "topsecret-value");
    }

    #[test]
    fn literal_passes_through() {
        let http = HttpClient::new(crate::net::EgressPolicy::default(), None).unwrap();
        assert_eq!(resolve_value("plain-literal", None, &http).unwrap(), "plain-literal");
    }

    #[test]
    fn stubbed_backends_error_clearly() {
        let http = HttpClient::new(crate::net::EgressPolicy::default(), None).unwrap();
        let err = resolve_value("vault://secret/data/x", None, &http).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }

    #[test]
    fn redaction_scrubs_registered_secrets() {
        register_secret("supersecretpassword");
        let scrubbed = redact("connecting with password=supersecretpassword now");
        assert!(!scrubbed.contains("supersecretpassword"));
        assert!(scrubbed.contains("***REDACTED***"));
    }

    #[test]
    fn redaction_ignores_tiny_values() {
        register_secret("ab"); // too short to register
        assert_eq!(redact("ab cd"), "ab cd");
    }
}
