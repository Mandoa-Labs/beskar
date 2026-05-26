//! Configuration: parsing `config.yaml`, resolving secret references, and
//! assembling the runtime [`Config`] used by every subcommand.
//!
//! Two shapes exist:
//! * [`RawConfig`] — the literal YAML on disk (secrets may be literals or
//!   `scheme://` references). Used by `config lint`.
//! * [`Config`] — the resolved runtime config: secrets fetched, endpoints and
//!   TLS settled, and a shared [`HttpClient`] carrying the egress policy.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::identity::{Auth, AuthConfig};
use crate::net::{self, EgressArgs, EgressPolicy, HttpClient};
use crate::policy::{Policy, PolicyConfig};
use crate::redact::{RedactionConfig, Redactor};
use crate::secrets;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
const DEFAULT_EMBED_MODEL: &str = "text-embedding-3-small";
const DEFAULT_OPENAI_GEN_MODEL: &str = "gpt-4o-mini";
const DEFAULT_ANTHROPIC_GEN_MODEL: &str = "claude-sonnet-4-6";

// ---------------------------------------------------------------------------
// On-disk shape
// ---------------------------------------------------------------------------

/// Per-endpoint provider configuration (PRD §6.2 E1.4). Used for both the
/// embedding and the generation endpoints, which are configured independently.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct EndpointConfig {
    /// `openai` | `openai-compatible` | `azure-openai` | `anthropic` | `bedrock`.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// API key; may be a literal or a `scheme://` secret reference.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Azure OpenAI API version (e.g. `2024-02-01`).
    #[serde(default)]
    pub api_version: Option<String>,
    /// Azure OpenAI deployment name.
    #[serde(default)]
    pub deployment: Option<String>,
    /// AWS region (Bedrock).
    #[serde(default)]
    pub region: Option<String>,
}

/// Egress controls (PRD §6.2 E1.6). CLI flags override these.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct EgressConfig {
    #[serde(default)]
    pub offline: Option<bool>,
    #[serde(default)]
    pub ca_bundle: Option<String>,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

/// The literal contents of `config.yaml`. New fields are optional, so configs
/// written before M5 deserialize unchanged.
#[derive(Debug, Deserialize, Default)]
pub struct RawConfig {
    pub pat: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub anthropic_key: Option<String>,
    pub pghost: String,
    pub pguser: String,
    pub pgport: String,
    pub pgdatabase: String,
    pub pgpassword: String,

    // Postgres TLS hardening (E1.7).
    #[serde(default)]
    pub pgsslmode: Option<String>,
    #[serde(default)]
    pub pgsslrootcert: Option<String>,
    #[serde(default)]
    pub pgsslcert: Option<String>,
    #[serde(default)]
    pub pgsslkey: Option<String>,

    // Default secret backend for `secret://` references (E1.1).
    #[serde(default)]
    pub secret_backend: Option<String>,

    // Private model endpoints (E1.4).
    #[serde(default)]
    pub embed: EndpointConfig,
    #[serde(default)]
    pub generate: EndpointConfig,

    // Egress controls (E1.6).
    #[serde(default)]
    pub egress: EgressConfig,

    // Pre-embedding PII/secret redaction hooks (E1.11).
    #[serde(default)]
    pub redaction: RedactionConfig,

    // Central admin policy enforced by `beskar serve` (E2.6).
    #[serde(default)]
    pub policy: PolicyConfig,

    // Identity & access: SSO/principals/RBAC/tenancy, enforced by `serve` (E2.2/E2.3/E2.5).
    #[serde(default)]
    pub auth: AuthConfig,
}

// ---------------------------------------------------------------------------
// Resolved runtime shape
// ---------------------------------------------------------------------------

/// Postgres TLS settings (E1.7).
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// `disable` | `require` | `verify-ca` | `verify-full`.
    pub sslmode: String,
    pub root_cert: Option<String>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
}

/// A fully-resolved model endpoint (embedding or generation).
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub api_version: Option<String>,
    pub deployment: Option<String>,
    pub region: Option<String>,
}

/// Runtime configuration handed to each subcommand.
pub struct Config {
    pub pghost: String,
    pub pguser: String,
    pub pgport: String,
    pub pgdatabase: String,
    pub pgpassword: String,
    pub tls: TlsConfig,
    pub embed: Endpoint,
    pub generate: Endpoint,
    /// Shared HTTP client carrying the egress policy + CA bundle.
    pub http: HttpClient,
    /// Pre-embedding redaction hook (E1.11); `None` when disabled in config.
    pub redactor: Option<Redactor>,
    /// Central admin policy enforced by `beskar serve` (E2.6).
    pub policy: Policy,
    /// Resolved identity & access config, enforced by `beskar serve`
    /// (E2.2/E2.3/E2.5). Empty unless an `auth` block is configured.
    pub auth: Auth,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn config_path() -> PathBuf {
    let dir = dirs::config_dir().expect("Could not determine config directory");
    dir.join("beskar").join("config.yaml")
}

pub fn read_raw_config(path: &PathBuf) -> Result<RawConfig> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))
}

/// Read, resolve, and assemble the runtime config. Emits the plaintext-secret
/// warning (E1.2) and, with `--verbose`, an effective-config dump (E1.3).
pub fn load_config(egress: &EgressArgs) -> Result<Config> {
    let path = config_path();
    let raw = read_raw_config(&path)
        .context("failed to read config; run `beskar init` first")?;
    let config = Config::resolve(&raw, egress, &path)?;
    if egress.verbose {
        config.print_effective();
    }
    Ok(config)
}

impl Config {
    fn resolve(raw: &RawConfig, egress: &EgressArgs, path: &PathBuf) -> Result<Config> {
        // 1. Egress policy. Auto-allow the hosts we were explicitly pointed at
        //    (configured endpoints + any Key Vault references), so `--offline`
        //    against a self-hosted stack works while public vendors stay blocked.
        let offline = egress.offline || raw.egress.offline.unwrap_or(false);
        let mut allow_hosts = raw.egress.allow_hosts.clone();
        allow_hosts.extend(egress.allow_host.iter().cloned());
        for url in [&raw.embed.base_url, &raw.generate.base_url].into_iter().flatten() {
            if let Some(h) = net::host_of(url) {
                allow_hosts.push(h);
            }
        }
        for value in secret_fields(raw) {
            if let Some(host) = keyvault_host(value) {
                allow_hosts.push(host);
            }
        }
        let policy = EgressPolicy::new(offline, allow_hosts);

        let ca_bundle = egress
            .ca_bundle
            .clone()
            .or_else(|| raw.egress.ca_bundle.clone());
        let http = HttpClient::new(policy, ca_bundle.as_deref())?;

        // 2. Resolve secrets, warning about literals (E1.2) and registering
        //    resolved values for redaction (E1.3).
        let default_backend = raw
            .secret_backend
            .clone()
            .or_else(|| std::env::var("BESKAR_SECRET_BACKEND").ok());
        let backend = default_backend.as_deref();

        warn_on_plaintext_secrets(raw, path);

        let resolve = |value: &str| -> Result<String> {
            let v = secrets::resolve_value(value, backend, &http)?;
            secrets::register_secret(&v);
            Ok(v)
        };

        let pat = resolve(&raw.pat)?;
        let pgpassword = resolve(&raw.pgpassword)?;
        let anthropic_key = match raw.anthropic_key.as_deref() {
            Some(k) if !k.is_empty() => Some(resolve(k)?),
            _ => None,
        };

        // 3. Endpoints (E1.4).
        let embed = resolve_embed_endpoint(&raw.embed, &pat, &resolve)?;
        let generate = resolve_generate_endpoint(
            &raw.generate,
            raw.provider.as_deref(),
            &pat,
            anthropic_key.as_deref(),
            &resolve,
        )?;

        // 4. TLS (E1.7).
        let tls = TlsConfig {
            sslmode: raw.pgsslmode.clone().unwrap_or_else(|| "require".to_string()),
            root_cert: raw.pgsslrootcert.clone(),
            client_cert: raw.pgsslcert.clone(),
            client_key: raw.pgsslkey.clone(),
        };

        // 5. Pre-embedding redaction hooks (E1.11). Fails closed on a bad
        //    pattern rather than embedding text the operator meant to scrub.
        let redactor = Redactor::from_config(&raw.redaction)
            .context("invalid `redaction` config")?;

        // 6. Central admin policy (E2.6), enforced by `beskar serve`.
        let policy = Policy::from_config(&raw.policy);

        // 7. Identity & access (E2.2/E2.3/E2.5). Resolves principal/SSO secrets
        //    via the same resolver, registering them for redaction.
        let auth = Auth::from_config(&raw.auth, &resolve).context("invalid `auth` config")?;

        Ok(Config {
            pghost: raw.pghost.clone(),
            pguser: raw.pguser.clone(),
            pgport: raw.pgport.clone(),
            pgdatabase: raw.pgdatabase.clone(),
            pgpassword,
            tls,
            embed,
            generate,
            http,
            redactor,
            policy,
            auth,
        })
    }

    fn print_effective(&self) {
        eprintln!("[verbose] effective config (secrets redacted):");
        eprintln!("  pghost={} pgport={} pguser={} pgdatabase={}",
            self.pghost, self.pgport, self.pguser, self.pgdatabase);
        eprintln!("  pgpassword={}", secrets::redact(&self.pgpassword));
        eprintln!("  tls.sslmode={} root_cert={:?} client_cert={:?}",
            self.tls.sslmode, self.tls.root_cert, self.tls.client_cert);
        eprintln!("  embed: provider={} base_url={} model={} api_key={}",
            self.embed.provider, self.embed.base_url, self.embed.model,
            secrets::redact(&self.embed.api_key));
        eprintln!("  generate: provider={} base_url={} model={} api_key={}",
            self.generate.provider, self.generate.base_url, self.generate.model,
            secrets::redact(&self.generate.api_key));
        let policy = self.http.policy();
        eprintln!("  egress: offline={} allow_hosts={:?}", policy.offline(), policy.allow_hosts());
        match &self.redactor {
            Some(r) => eprintln!("  redaction: enabled rules={:?}", r.rule_names()),
            None => eprintln!("  redaction: disabled"),
        }
        eprintln!("  policy: {}", self.policy.summary());
        eprintln!("  auth: {}", self.auth.summary());
    }
}

/// The set of config values that may carry a secret (literal or reference).
fn secret_fields(raw: &RawConfig) -> Vec<&str> {
    let mut v = vec![raw.pat.as_str(), raw.pgpassword.as_str()];
    if let Some(k) = raw.anthropic_key.as_deref() {
        v.push(k);
    }
    if let Some(k) = raw.embed.api_key.as_deref() {
        v.push(k);
    }
    if let Some(k) = raw.generate.api_key.as_deref() {
        v.push(k);
    }
    v
}

/// If a value is an `azure-keyvault://<vault>/...` reference, return the vault
/// host so it can be added to the egress allowlist.
fn keyvault_host(value: &str) -> Option<String> {
    let reference = secrets::parse_reference(value)?;
    if reference.scheme != "azure-keyvault" {
        return None;
    }
    let vault = reference.location.split('/').next()?;
    if vault.contains('.') {
        Some(vault.to_string())
    } else {
        Some(format!("{vault}.vault.azure.net"))
    }
}

fn warn_on_plaintext_secrets(raw: &RawConfig, path: &PathBuf) {
    let mut literal = Vec::new();
    if secrets::is_literal_secret(&raw.pat) {
        literal.push("pat");
    }
    if secrets::is_literal_secret(&raw.pgpassword) {
        literal.push("pgpassword");
    }
    if raw.anthropic_key.as_deref().is_some_and(secrets::is_literal_secret) {
        literal.push("anthropic_key");
    }
    if !literal.is_empty() {
        eprintln!(
            "warning: plaintext secret(s) [{}] read from {}; consider a secret backend \
             (e.g. azure-keyvault://...) — run `beskar config lint` for details.",
            literal.join(", "),
            path.display()
        );
    }
}

fn resolve_embed_endpoint(
    cfg: &EndpointConfig,
    pat: &str,
    resolve: &impl Fn(&str) -> Result<String>,
) -> Result<Endpoint> {
    let provider = cfg.provider.clone().unwrap_or_else(|| "openai".to_string());
    let api_key = match cfg.api_key.as_deref() {
        Some(k) if !k.is_empty() => resolve(k)?,
        _ => pat.to_string(),
    };
    let base_url = cfg
        .base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_string());
    let model = cfg
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string());
    Ok(Endpoint {
        provider,
        base_url: trim_base(&base_url),
        model,
        api_key,
        api_version: cfg.api_version.clone(),
        deployment: cfg.deployment.clone(),
        region: cfg.region.clone(),
    })
}

fn resolve_generate_endpoint(
    cfg: &EndpointConfig,
    top_provider: Option<&str>,
    pat: &str,
    anthropic_key: Option<&str>,
    resolve: &impl Fn(&str) -> Result<String>,
) -> Result<Endpoint> {
    let provider = cfg
        .provider
        .clone()
        .or_else(|| top_provider.map(str::to_string))
        .unwrap_or_else(|| "openai".to_string());

    let api_key = match cfg.api_key.as_deref() {
        Some(k) if !k.is_empty() => resolve(k)?,
        _ if provider == "anthropic" => anthropic_key.unwrap_or("").to_string(),
        _ => pat.to_string(),
    };

    let base_url = cfg.base_url.clone().unwrap_or_else(|| match provider.as_str() {
        "anthropic" => DEFAULT_ANTHROPIC_BASE.to_string(),
        _ => DEFAULT_OPENAI_BASE.to_string(),
    });
    let model = cfg.model.clone().unwrap_or_else(|| match provider.as_str() {
        "anthropic" => DEFAULT_ANTHROPIC_GEN_MODEL.to_string(),
        _ => DEFAULT_OPENAI_GEN_MODEL.to_string(),
    });

    Ok(Endpoint {
        provider,
        base_url: trim_base(&base_url),
        model,
        api_key,
        api_version: cfg.api_version.clone(),
        deployment: cfg.deployment.clone(),
        region: cfg.region.clone(),
    })
}

fn trim_base(base: &str) -> String {
    base.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// `beskar config lint` (E1.2)
// ---------------------------------------------------------------------------

/// Lint the on-disk config for plaintext secrets and lax file permissions.
/// Returns `true` if any issue was found (caller should exit non-zero).
pub fn lint() -> Result<bool> {
    let path = config_path();
    let raw = read_raw_config(&path)?;
    let mut issues = 0u32;

    println!("Linting {}", path.display());

    for (name, value) in [
        ("pat", Some(raw.pat.as_str())),
        ("pgpassword", Some(raw.pgpassword.as_str())),
        ("anthropic_key", raw.anthropic_key.as_deref()),
        ("embed.api_key", raw.embed.api_key.as_deref()),
        ("generate.api_key", raw.generate.api_key.as_deref()),
    ] {
        if let Some(v) = value {
            if secrets::is_literal_secret(v) {
                println!(
                    "  [plaintext secret] '{name}' is a literal value. Use a secret backend, \
                     e.g. {name}: azure-keyvault://<vault>/<secret-name>"
                );
                issues += 1;
            }
        }
    }

    if let Some(mode_issue) = lax_mode_issue(&path) {
        println!("  [permissions] {mode_issue}");
        issues += 1;
    }

    // Validate the redaction hooks (E1.11) compile, so a bad preset/regex is
    // caught here rather than at ingestion time.
    match Redactor::from_config(&raw.redaction) {
        Ok(Some(r)) => println!("  [redaction] enabled with rules: {:?}", r.rule_names()),
        Ok(None) => {}
        Err(e) => {
            println!("  [redaction] {e:#}");
            issues += 1;
        }
    }

    // Central policy (E2.6): report it, and flag a require_redaction policy that
    // its own config would violate — `beskar serve` would fail closed on it.
    let policy = Policy::from_config(&raw.policy);
    if policy.is_active() {
        println!("  [policy] {}", policy.summary());
        if policy.require_redaction() && !raw.redaction.enabled {
            println!(
                "  [policy] require_redaction is set but `redaction.enabled` is false; \
                 `beskar serve` will refuse to start"
            );
            issues += 1;
        }
    }

    // Identity & access (E2.2/E2.3/E2.5): validate the `auth` block compiles
    // (roles, identifiers, exactly-one OIDC key) so a bad grant is caught here
    // rather than at server startup. Secrets are not resolved during lint, so a
    // no-op resolver is used (token validity is checked only at `serve` time).
    if raw.auth.oidc.is_some() || !raw.auth.principals.is_empty() {
        match Auth::from_config(&raw.auth, &|v| Ok(v.to_string())) {
            Ok(auth) => println!("  [auth] {}", auth.summary()),
            Err(e) => {
                println!("  [auth] {e:#}");
                issues += 1;
            }
        }
    }

    if issues == 0 {
        println!("OK: no plaintext secrets or lax permissions found.");
    } else {
        println!("{issues} issue(s) found.");
    }
    Ok(issues > 0)
}

/// On unix, flag config files readable/writable by group or other (mode is not
/// `0600`-style). Returns `None` on non-unix or when the mode is acceptable.
#[cfg(unix)]
fn lax_mode_issue(path: &PathBuf) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        Some(format!(
            "config is mode {:#o}; restrict to 0600 (chmod 600 {})",
            mode,
            path.display()
        ))
    } else {
        None
    }
}

#[cfg(not(unix))]
fn lax_mode_issue(_path: &PathBuf) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint_cfg() -> EndpointConfig {
        EndpointConfig::default()
    }

    fn noop_resolve(v: &str) -> Result<String> {
        Ok(v.to_string())
    }

    #[test]
    fn embed_endpoint_defaults_to_openai_with_pat() {
        let ep = resolve_embed_endpoint(&endpoint_cfg(), "sk-pat", &noop_resolve).unwrap();
        assert_eq!(ep.provider, "openai");
        assert_eq!(ep.base_url, "https://api.openai.com/v1");
        assert_eq!(ep.model, "text-embedding-3-small");
        assert_eq!(ep.api_key, "sk-pat");
    }

    #[test]
    fn embed_endpoint_honors_overrides_and_trims_base() {
        let cfg = EndpointConfig {
            provider: Some("openai-compatible".into()),
            base_url: Some("https://llm.internal/v1/".into()),
            model: Some("bge-small".into()),
            api_key: Some("local-key".into()),
            ..Default::default()
        };
        let ep = resolve_embed_endpoint(&cfg, "sk-pat", &noop_resolve).unwrap();
        assert_eq!(ep.provider, "openai-compatible");
        assert_eq!(ep.base_url, "https://llm.internal/v1"); // trailing slash trimmed
        assert_eq!(ep.model, "bge-small");
        assert_eq!(ep.api_key, "local-key");
    }

    #[test]
    fn generate_endpoint_anthropic_uses_anthropic_key() {
        let ep = resolve_generate_endpoint(
            &endpoint_cfg(),
            Some("anthropic"),
            "sk-pat",
            Some("sk-ant"),
            &noop_resolve,
        )
        .unwrap();
        assert_eq!(ep.provider, "anthropic");
        assert_eq!(ep.api_key, "sk-ant");
        assert_eq!(ep.model, "claude-sonnet-4-6");
        assert_eq!(ep.base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn generate_endpoint_defaults_to_openai() {
        let ep =
            resolve_generate_endpoint(&endpoint_cfg(), None, "sk-pat", None, &noop_resolve).unwrap();
        assert_eq!(ep.provider, "openai");
        assert_eq!(ep.api_key, "sk-pat");
        assert_eq!(ep.model, "gpt-4o-mini");
    }

    #[test]
    fn keyvault_host_appends_default_suffix() {
        assert_eq!(
            keyvault_host("azure-keyvault://mykv/secret-name").as_deref(),
            Some("mykv.vault.azure.net")
        );
        assert_eq!(
            keyvault_host("azure-keyvault://mykv.vault.azure.net/secret").as_deref(),
            Some("mykv.vault.azure.net")
        );
        assert_eq!(keyvault_host("sk-literal"), None);
    }
}
