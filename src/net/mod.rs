//! Outbound HTTP with enterprise egress controls (PRD §6.2 E1.6).
//!
//! Every outbound request beskar makes — embeddings, generation, and secret
//! backends — goes through [`HttpClient`], which wraps a `reqwest` client with:
//!
//! * proxy support (honored from `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` by
//!   reqwest's defaults — we simply don't disable it),
//! * a custom CA bundle (`--ca-bundle` / `SSL_CERT_FILE`),
//! * a host allowlist, and
//! * `--offline`, which fails closed on any non-allowlisted host.

use anyhow::{bail, Context, Result};

/// Global egress flags, parsed by clap and shared by every subcommand.
///
/// These live here (rather than in `main`) so `utils` and `secrets` can accept
/// them without depending on the binary's CLI types.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct EgressArgs {
    /// Fail closed: refuse any outbound connection to a non-allowlisted host.
    #[arg(long, global = true)]
    pub offline: bool,

    /// PEM CA bundle for outbound TLS (overrides the system store / `SSL_CERT_FILE`).
    #[arg(long, global = true, value_name = "PATH")]
    pub ca_bundle: Option<String>,

    /// Permit an outbound host (repeatable). Adds to the egress allowlist.
    #[arg(long = "allow-host", global = true, value_name = "HOST")]
    pub allow_host: Vec<String>,

    /// Print the effective config (secrets redacted) to stderr before running.
    #[arg(long, global = true)]
    pub verbose: bool,
}

/// Decides whether a given outbound host may be contacted.
///
/// Enforcement is active when `offline` is set **or** the allowlist is
/// non-empty. When active, only hosts that match an allowlist entry are
/// permitted; everything else fails closed. When inactive (the default,
/// non-offline, empty allowlist) all hosts are permitted.
#[derive(Clone, Debug, Default)]
pub struct EgressPolicy {
    offline: bool,
    allow_hosts: Vec<String>,
}

impl EgressPolicy {
    pub fn new(offline: bool, allow_hosts: Vec<String>) -> Self {
        let allow_hosts = allow_hosts
            .into_iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        Self { offline, allow_hosts }
    }

    pub fn offline(&self) -> bool {
        self.offline
    }

    pub fn allow_hosts(&self) -> &[String] {
        &self.allow_hosts
    }

    /// Enforcement is on when offline or an explicit allowlist is configured.
    fn enforced(&self) -> bool {
        self.offline || !self.allow_hosts.is_empty()
    }

    /// A host matches an entry if it is equal to it or a subdomain of it, so
    /// `vault.azure.net` permits `mykv.vault.azure.net`.
    fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.allow_hosts.iter().any(|entry| {
            host == *entry || host.ends_with(&format!(".{entry}"))
        })
    }

    /// Returns an error if `url`'s host is not permitted under this policy.
    pub fn check_url(&self, url: &str) -> Result<()> {
        if !self.enforced() {
            return Ok(());
        }
        let host = host_of(url)
            .with_context(|| format!("could not parse host from URL: {url}"))?;
        if self.host_allowed(&host) {
            return Ok(());
        }
        if self.offline {
            bail!(
                "offline mode: refusing connection to '{host}'. \
                 Add it with --allow-host {host} (or egress.allow_hosts in config) if it is trusted."
            );
        }
        bail!(
            "egress allowlist: '{host}' is not permitted. \
             Add it with --allow-host {host} (or egress.allow_hosts in config)."
        )
    }
}

/// Extract the host portion of an `http(s)://` URL without pulling in the `url`
/// crate: drop the scheme, any `userinfo@`, then the path and `:port`.
pub fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    // Strip the port. Guard against IPv6 literals like `[::1]:5432`.
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// A `reqwest` blocking client paired with an [`EgressPolicy`]. Obtain request
/// builders via [`HttpClient::post`] / [`HttpClient::get`], which enforce the
/// policy before the request leaves the process.
#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::blocking::Client,
    policy: EgressPolicy,
}

impl HttpClient {
    /// Build a client honoring the policy and an optional PEM CA bundle.
    /// `ca_bundle` takes precedence over the `SSL_CERT_FILE` environment variable.
    pub fn new(policy: EgressPolicy, ca_bundle: Option<&str>) -> Result<Self> {
        let mut builder = reqwest::blocking::Client::builder();

        if let Some(path) = ca_bundle {
            // Explicit --ca-bundle: a failure here is fatal.
            for cert in load_ca_bundle(path)? {
                builder = builder.add_root_certificate(cert);
            }
        } else if let Ok(path) = std::env::var("SSL_CERT_FILE") {
            // SSL_CERT_FILE is also consumed by OpenSSL itself; honor it here too,
            // but best-effort so a malformed value never breaks an unrelated run.
            if !path.trim().is_empty() {
                match load_ca_bundle(&path) {
                    Ok(certs) => {
                        for cert in certs {
                            builder = builder.add_root_certificate(cert);
                        }
                    }
                    Err(e) => eprintln!("warning: ignoring SSL_CERT_FILE ({path}): {e}"),
                }
            }
        }

        let client = builder
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { client, policy })
    }

    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    pub fn post(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder> {
        self.policy.check_url(url)?;
        Ok(self.client.post(url))
    }

    pub fn get(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder> {
        self.policy.check_url(url)?;
        Ok(self.client.get(url))
    }
}

/// Parse a PEM file that may contain one or more concatenated certificates.
fn load_ca_bundle(path: &str) -> Result<Vec<reqwest::Certificate>> {
    let pem = std::fs::read(path)
        .with_context(|| format!("failed to read CA bundle: {path}"))?;
    let certs = reqwest::Certificate::from_pem_bundle(&pem)
        .with_context(|| format!("failed to parse PEM certificates from {path}"))?;
    if certs.is_empty() {
        bail!("CA bundle {path} contained no certificates");
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_handles_scheme_userinfo_port_and_path() {
        assert_eq!(host_of("https://api.openai.com/v1/x").as_deref(), Some("api.openai.com"));
        assert_eq!(host_of("https://user:pw@h.example:8443/p").as_deref(), Some("h.example"));
        assert_eq!(host_of("http://kv.vault.azure.net").as_deref(), Some("kv.vault.azure.net"));
        assert_eq!(host_of("https://[::1]:5432/db").as_deref(), Some("::1"));
    }

    #[test]
    fn disabled_policy_allows_everything() {
        let p = EgressPolicy::new(false, vec![]);
        assert!(p.check_url("https://anything.example/x").is_ok());
    }

    #[test]
    fn allowlist_restricts_even_when_online() {
        let p = EgressPolicy::new(false, vec!["llm.internal".into()]);
        assert!(p.check_url("https://llm.internal/v1").is_ok());
        assert!(p.check_url("https://api.openai.com/v1").is_err());
    }

    #[test]
    fn subdomains_of_an_entry_are_allowed() {
        let p = EgressPolicy::new(true, vec!["vault.azure.net".into()]);
        assert!(p.check_url("https://mykv.vault.azure.net/secrets/x").is_ok());
        assert!(p.check_url("https://evil.example/x").is_err());
    }

    #[test]
    fn offline_with_empty_allowlist_blocks_all() {
        let p = EgressPolicy::new(true, vec![]);
        assert!(p.check_url("https://llm.internal/v1").is_err());
    }
}
