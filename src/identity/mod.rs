//! Identity & access for the platform tier (PRD §6.3 E2.2, E2.3, E2.5 · §9.2).
//!
//! `beskar serve` enforces, **server-side**, who a caller is and what they may
//! do. This module is the policy core; [`crate::serve`] is the front-end that
//! applies it per request. Three concerns:
//!
//! * **Identity (E2.2)** — a request carries a bearer token. It may be the
//!   operator's shared admin token, a configured static principal token (service
//!   accounts), or a short-lived **session token** that `beskar login` obtained
//!   by exchanging an OIDC ID token at `/v1/login`. The CLI never holds DB creds.
//! * **RBAC (E2.3)** — every principal has a [`Role`] (`reader` ⊂ `author` ⊂
//!   `admin`) per corpus; [`Action`]s are checked against it. Enforcement is here,
//!   not client-trusted.
//! * **Tenant isolation (E2.5)** — a principal belongs to a tenant, and the
//!   physical corpus tables are derived from the tenant *server-side*
//!   ([`Principal::physical_table`]). A caller cannot name another tenant's
//!   tables, so cross-tenant access is structurally impossible.
//!
//! All token crypto goes through [`jwt`], which uses the existing `openssl`
//! crate (no new dependencies; FIPS-coherent).

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

mod jwt;

/// Clock skew tolerance when validating `exp` (seconds).
const EXP_LEEWAY_SECS: u64 = 60;
/// Default lifetime of a session token issued by `/v1/login`.
const DEFAULT_SESSION_TTL_SECS: u64 = 3600;
/// Max length of a tenant/corpus identifier.
const MAX_IDENTIFIER_LEN: usize = 40;
/// Human description of a valid identifier, reused in error messages.
const DESCR: &str = "a lowercase letter followed by lowercase letters/digits";

// ---------------------------------------------------------------------------
// Roles & actions
// ---------------------------------------------------------------------------

/// A role, ordered `reader` < `author` < `admin`. A higher role subsumes the
/// capabilities of the lower ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Reader,
    Author,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "reader" => Some(Role::Reader),
            "author" => Some(Role::Author),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Reader => "reader",
            Role::Author => "author",
            Role::Admin => "admin",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Role::Reader => 0,
            Role::Author => 1,
            Role::Admin => 2,
        }
    }

    /// Whether this role is permitted to perform `action`.
    pub fn allows(self, action: Action) -> bool {
        self.rank() >= action.min_role().rank()
    }
}

/// An action a caller can request against a corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Retrieve + generate (`/v1/query`). Available to every role.
    Query,
    /// Ingest documents (`/v1/ingest`). Requires `author` or higher.
    Ingest,
    /// Administer a corpus — create/drop (`/v1/admin/...`). Requires `admin`.
    Administer,
}

impl Action {
    fn min_role(self) -> Role {
        match self {
            Action::Query => Role::Reader,
            Action::Ingest => Role::Author,
            Action::Administer => Role::Admin,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Action::Query => "query",
            Action::Ingest => "ingest",
            Action::Administer => "administer",
        }
    }
}

// ---------------------------------------------------------------------------
// Identifiers & tenant namespacing
// ---------------------------------------------------------------------------

/// A valid tenant/corpus identifier: a lowercase letter followed by lowercase
/// letters/digits, up to [`MAX_IDENTIFIER_LEN`]. Deliberately strict: the name
/// is interpolated into SQL table names, so this both prevents injection and
/// keeps tenant-namespaced names unambiguous (no `_` means `t_<tenant>_<corpus>`
/// has exactly one split).
pub fn valid_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > MAX_IDENTIFIER_LEN {
        return false;
    }
    if !b[0].is_ascii_lowercase() {
        return false;
    }
    b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Role grants & principals
// ---------------------------------------------------------------------------

/// A principal's roles: an optional tenant-wide default (the `*` grant) plus
/// per-corpus overrides. A specific grant overrides the wildcard entirely (so a
/// wildcard `admin` can be narrowed to `reader` on one corpus, and vice versa).
#[derive(Clone, Debug, Default)]
pub struct RoleGrants {
    wildcard: Option<Role>,
    per_corpus: BTreeMap<String, Role>,
}

impl RoleGrants {
    fn insert(&mut self, corpus: &str, role: Role) {
        if corpus == "*" {
            self.wildcard = Some(role);
        } else {
            self.per_corpus.insert(corpus.to_string(), role);
        }
    }

    fn is_empty(&self) -> bool {
        self.wildcard.is_none() && self.per_corpus.is_empty()
    }

    /// The effective role for `corpus`, if any.
    pub fn role_for(&self, corpus: &str) -> Option<Role> {
        self.per_corpus.get(corpus).copied().or(self.wildcard)
    }

    /// Build from a config map of `corpus -> role` (with `*` for the wildcard).
    fn from_config(map: &BTreeMap<String, String>) -> Result<Self> {
        let mut grants = RoleGrants::default();
        for (corpus, role) in map {
            let role = Role::parse(role)
                .with_context(|| format!("invalid role '{role}' for corpus '{corpus}'"))?;
            if corpus != "*" && !valid_identifier(corpus) {
                bail!("invalid corpus name '{corpus}' in role grants (expected {DESCR})");
            }
            grants.insert(corpus, role);
        }
        Ok(grants)
    }

    /// Rebuild from a session token's `roles` claim (`{"*":"admin",...}`).
    fn from_claim(v: &Value) -> Result<Self> {
        let obj = v.as_object().context("`roles` claim must be an object")?;
        let mut map = BTreeMap::new();
        for (corpus, role) in obj {
            let role = role.as_str().context("role value must be a string")?;
            map.insert(corpus.clone(), role.to_string());
        }
        RoleGrants::from_config(&map)
    }

    fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(r) = self.wildcard {
            m.insert("*".to_string(), json!(r.as_str()));
        }
        for (corpus, role) in &self.per_corpus {
            m.insert(corpus.clone(), json!(role.as_str()));
        }
        Value::Object(m)
    }
}

/// An authenticated caller.
#[derive(Clone, Debug)]
pub struct Principal {
    /// Stable identity, recorded in the audit log.
    pub subject: String,
    /// Owning tenant, or `None` for the operator's super-admin (shared token):
    /// the super-admin is not tenant-scoped and addresses corpora by raw name.
    pub tenant: Option<String>,
    grants: RoleGrants,
    superadmin: bool,
}

impl Principal {
    /// The operator's super-admin, authenticated by the shared serve token.
    /// Full access to every corpus; no tenant namespacing.
    pub fn superadmin() -> Principal {
        Principal {
            subject: "admin".to_string(),
            tenant: None,
            grants: RoleGrants::default(),
            superadmin: true,
        }
    }

    /// `true` for the operator's super-admin (the shared serve token). Used to
    /// gate non-tenant-scoped, operator-only surfaces such as SCIM provisioning.
    pub fn is_superadmin(&self) -> bool {
        self.superadmin
    }

    /// Check that this principal may perform `action` on `corpus`. Returns a
    /// caller-safe reason on denial.
    pub fn authorize(&self, corpus: &str, action: Action) -> Result<(), String> {
        if self.superadmin {
            return Ok(());
        }
        match self.grants.role_for(corpus) {
            Some(role) if role.allows(action) => Ok(()),
            Some(role) => Err(format!(
                "role '{}' is not permitted to {} corpus '{}'",
                role.as_str(),
                action.as_str(),
                corpus
            )),
            None => Err(format!(
                "identity '{}' has no role on corpus '{}'",
                self.subject, corpus
            )),
        }
    }

    /// The physical table prefix for a logical `corpus`, namespaced by tenant.
    /// Derived from `self.tenant` (from the token), never from the request, so a
    /// caller can only ever address its own tenant's tables (E2.5).
    pub fn physical_table(&self, corpus: &str) -> String {
        match &self.tenant {
            Some(tenant) => format!("t_{tenant}_{corpus}"),
            None => corpus.to_string(),
        }
    }

    /// The principal as JSON, for `GET /v1/whoami`.
    pub fn to_json(&self) -> Value {
        json!({
            "subject": self.subject,
            "tenant": self.tenant,
            "superadmin": self.superadmin,
            "roles": self.grants.to_json(),
        })
    }
}

// ---------------------------------------------------------------------------
// On-disk config (`auth:` block in config.yaml)
// ---------------------------------------------------------------------------

/// The `auth` block. Absent (the default) means only the shared serve token
/// authenticates — i.e. M8 behavior, a single super-admin.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct AuthConfig {
    /// Secret used to sign/verify short-lived session tokens issued by
    /// `/v1/login`. May be a literal or a `scheme://` secret reference. Required
    /// for SSO login to be available.
    #[serde(default)]
    pub session_secret: Option<String>,
    /// Session-token lifetime in seconds (default 3600).
    #[serde(default)]
    pub session_ttl_secs: Option<u64>,
    /// Trusted OIDC identity provider for SSO.
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
    /// Static principals (service accounts) authenticated by a bearer token.
    #[serde(default)]
    pub principals: Vec<PrincipalConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OidcConfig {
    /// Required `iss` claim of accepted ID tokens.
    pub issuer: String,
    /// Required `aud` claim, if set.
    #[serde(default)]
    pub audience: Option<String>,
    /// Shared secret for HS256-signed ID tokens (literal or secret reference).
    #[serde(default)]
    pub hs256_secret: Option<String>,
    /// RSA public key (PEM) for RS256-signed ID tokens.
    #[serde(default)]
    pub rs256_public_key: Option<String>,
    /// Claim that carries the tenant (default `tenant`).
    #[serde(default = "default_tenant_claim")]
    pub tenant_claim: String,
    /// Fallback tenant when the token has no tenant claim.
    #[serde(default)]
    pub default_tenant: Option<String>,
    /// Claim carrying the caller's groups/roles (default `groups`).
    #[serde(default = "default_roles_claim")]
    pub roles_claim: String,
    /// Map of IdP group name -> role grant.
    #[serde(default)]
    pub role_map: BTreeMap<String, RoleMapEntry>,
    /// Role granted (on `*`) when no group matched.
    #[serde(default)]
    pub default_role: Option<String>,
}

fn default_tenant_claim() -> String {
    "tenant".to_string()
}
fn default_roles_claim() -> String {
    "groups".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct RoleMapEntry {
    pub role: String,
    /// Corpus the role applies to; `*` (default) for tenant-wide.
    #[serde(default = "wildcard")]
    pub corpus: String,
}

fn wildcard() -> String {
    "*".to_string()
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct PrincipalConfig {
    /// Bearer token (literal or secret reference).
    pub token: String,
    pub subject: String,
    pub tenant: String,
    /// `corpus -> role` (use `*` for tenant-wide).
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Resolved runtime auth
// ---------------------------------------------------------------------------

struct StaticPrincipal {
    token: String,
    principal: Principal,
}

enum OidcKey {
    Hs256(Vec<u8>),
    Rs256(String),
}

struct Oidc {
    issuer: String,
    audience: Option<String>,
    key: OidcKey,
    tenant_claim: String,
    default_tenant: Option<String>,
    roles_claim: String,
    role_map: BTreeMap<String, (Role, String)>,
    default_role: Option<Role>,
}

/// Fully-resolved authentication config. Secrets are fetched; the shared
/// super-admin serve token is *not* held here (it is checked by `serve`).
#[derive(Default)]
pub struct Auth {
    session_secret: Option<Vec<u8>>,
    session_ttl_secs: u64,
    oidc: Option<Oidc>,
    principals: Vec<StaticPrincipal>,
}

impl Auth {
    /// Resolve an [`AuthConfig`], using `resolve` to fetch any `scheme://` secret
    /// references (and register them for redaction).
    pub fn from_config(cfg: &AuthConfig, resolve: &dyn Fn(&str) -> Result<String>) -> Result<Auth> {
        let session_secret = match cfg.session_secret.as_deref() {
            Some(s) if !s.is_empty() => Some(resolve(s)?.into_bytes()),
            _ => None,
        };
        let session_ttl_secs = cfg.session_ttl_secs.unwrap_or(DEFAULT_SESSION_TTL_SECS);

        let oidc = match &cfg.oidc {
            Some(o) => Some(Oidc::from_config(o, resolve)?),
            None => None,
        };

        let mut principals = Vec::with_capacity(cfg.principals.len());
        for (i, p) in cfg.principals.iter().enumerate() {
            if p.subject.trim().is_empty() {
                bail!("auth.principals[{i}]: `subject` is required");
            }
            if !valid_identifier(&p.tenant) {
                bail!(
                    "auth.principals[{i}] (subject '{}'): tenant '{}' must be {DESCR}",
                    p.subject,
                    p.tenant
                );
            }
            let token = resolve(&p.token)
                .with_context(|| format!("auth.principals[{i}] (subject '{}'): token", p.subject))?;
            if token.is_empty() {
                bail!("auth.principals[{i}] (subject '{}'): token resolved to empty", p.subject);
            }
            let grants = RoleGrants::from_config(&p.roles)
                .with_context(|| format!("auth.principals[{i}] (subject '{}')", p.subject))?;
            if grants.is_empty() {
                bail!("auth.principals[{i}] (subject '{}'): no roles granted", p.subject);
            }
            principals.push(StaticPrincipal {
                token,
                principal: Principal {
                    subject: p.subject.clone(),
                    tenant: Some(p.tenant.clone()),
                    grants,
                    superadmin: false,
                },
            });
        }

        Ok(Auth {
            session_secret,
            session_ttl_secs,
            oidc,
            principals,
        })
    }

    /// `true` if any non-shared-token authentication is configured.
    pub fn is_configured(&self) -> bool {
        !self.principals.is_empty() || self.oidc.is_some()
    }

    /// `true` if SSO login can issue session tokens (OIDC trust + signer set).
    pub fn can_login(&self) -> bool {
        self.oidc.is_some() && self.session_secret.is_some()
    }

    /// A one-line human summary for startup logging.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.principals.is_empty() {
            parts.push(format!("{} static principal(s)", self.principals.len()));
        }
        if self.oidc.is_some() {
            parts.push("OIDC SSO".to_string());
        }
        if self.can_login() {
            parts.push("login enabled".to_string());
        }
        if parts.is_empty() {
            "shared admin token only".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// Authenticate a bearer token to a [`Principal`], or `None` if it matches
    /// nothing. (The shared super-admin token is handled by `serve` itself.)
    pub fn authenticate(&self, bearer: &str) -> Option<Principal> {
        // 1. Static principal tokens (constant-time compare).
        for sp in &self.principals {
            if ct_eq(bearer, &sp.token) {
                return Some(sp.principal.clone());
            }
        }
        // 2. A beskar session token (HS256, signed by us at /v1/login).
        if let Some(secret) = &self.session_secret {
            if let Ok(claims) = jwt::verify_hs256(bearer, secret) {
                if let Ok(p) = principal_from_session(&claims) {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Exchange an OIDC ID token for a short-lived beskar session token. Returns
    /// the signed token, the resolved principal, and the token's `exp` (epoch s).
    pub fn login_with_oidc(&self, id_token: &str) -> Result<(String, Principal, u64)> {
        let oidc = self
            .oidc
            .as_ref()
            .context("SSO is not configured on this server (no auth.oidc)")?;
        let secret = self
            .session_secret
            .as_ref()
            .context("session signing is not configured (no auth.session_secret)")?;

        let claims = oidc.verify(id_token)?;
        let principal = oidc.principal_from_claims(&claims)?;

        let now = now_secs();
        let exp = now + self.session_ttl_secs;
        let session_claims = json!({
            "iss": "beskar",
            "sub": principal.subject,
            "tenant": principal.tenant,
            "roles": principal.grants.to_json(),
            "iat": now,
            "exp": exp,
        });
        let token = jwt::encode_hs256(&session_claims, secret)?;
        Ok((token, principal, exp))
    }
}

impl Oidc {
    fn from_config(cfg: &OidcConfig, resolve: &dyn Fn(&str) -> Result<String>) -> Result<Oidc> {
        if cfg.issuer.trim().is_empty() {
            bail!("auth.oidc.issuer is required");
        }
        let key = match (cfg.hs256_secret.as_deref(), cfg.rs256_public_key.as_deref()) {
            (Some(s), None) if !s.is_empty() => OidcKey::Hs256(resolve(s)?.into_bytes()),
            (None, Some(pem)) if !pem.is_empty() => OidcKey::Rs256(pem.to_string()),
            (None, None) => bail!("auth.oidc requires either `hs256_secret` or `rs256_public_key`"),
            _ => bail!("auth.oidc: set exactly one of `hs256_secret` or `rs256_public_key`"),
        };

        let mut role_map = BTreeMap::new();
        for (group, entry) in &cfg.role_map {
            let role = Role::parse(&entry.role).with_context(|| {
                format!("auth.oidc.role_map['{group}']: invalid role '{}'", entry.role)
            })?;
            if entry.corpus != "*" && !valid_identifier(&entry.corpus) {
                bail!("auth.oidc.role_map['{group}']: corpus '{}' must be {DESCR}", entry.corpus);
            }
            role_map.insert(group.clone(), (role, entry.corpus.clone()));
        }
        let default_role = match cfg.default_role.as_deref() {
            Some(r) => Some(
                Role::parse(r).with_context(|| format!("auth.oidc.default_role: invalid role '{r}'"))?,
            ),
            None => None,
        };
        if let Some(t) = &cfg.default_tenant {
            if !valid_identifier(t) {
                bail!("auth.oidc.default_tenant '{t}' must be {DESCR}");
            }
        }

        Ok(Oidc {
            issuer: cfg.issuer.clone(),
            audience: cfg.audience.clone(),
            key,
            tenant_claim: cfg.tenant_claim.clone(),
            default_tenant: cfg.default_tenant.clone(),
            roles_claim: cfg.roles_claim.clone(),
            role_map,
            default_role,
        })
    }

    /// Verify an ID token's signature and standard claims (`iss`, `aud`, `exp`).
    fn verify(&self, token: &str) -> Result<Value> {
        let claims = match &self.key {
            OidcKey::Hs256(secret) => jwt::verify_hs256(token, secret)?,
            OidcKey::Rs256(pem) => jwt::verify_rs256(token, pem)?,
        };
        let iss = claims.get("iss").and_then(Value::as_str).unwrap_or("");
        if iss != self.issuer {
            bail!("ID token issuer '{iss}' does not match the configured issuer");
        }
        if let Some(aud) = &self.audience {
            let ok = match claims.get("aud") {
                Some(Value::String(s)) => s == aud,
                Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(aud.as_str())),
                _ => false,
            };
            if !ok {
                bail!("ID token audience does not include '{aud}'");
            }
        }
        match claims.get("exp").and_then(Value::as_u64) {
            Some(exp) if now_secs() < exp + EXP_LEEWAY_SECS => {}
            Some(_) => bail!("ID token has expired"),
            None => bail!("ID token has no `exp` claim"),
        }
        Ok(claims)
    }

    /// Map verified ID-token claims to a tenant-scoped [`Principal`].
    fn principal_from_claims(&self, claims: &Value) -> Result<Principal> {
        let subject = claims
            .get("sub")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .context("ID token has no `sub` claim")?
            .to_string();

        let tenant = claims
            .get(self.tenant_claim.as_str())
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| self.default_tenant.clone())
            .with_context(|| {
                format!(
                    "ID token has no '{}' claim and no default_tenant is configured",
                    self.tenant_claim
                )
            })?;
        if !valid_identifier(&tenant) {
            bail!("tenant '{tenant}' from the ID token must be {DESCR}");
        }

        let mut grants = RoleGrants::default();
        let groups: Vec<&str> = match claims.get(self.roles_claim.as_str()) {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
            Some(Value::String(s)) => vec![s.as_str()],
            _ => vec![],
        };
        for g in groups {
            if let Some((role, corpus)) = self.role_map.get(g) {
                grants.insert(corpus, *role);
            }
        }
        if grants.is_empty() {
            if let Some(role) = self.default_role {
                grants.insert("*", role);
            }
        }
        if grants.is_empty() {
            bail!("identity '{subject}' was granted no roles (no group in the ID token matched auth.oidc.role_map)");
        }

        Ok(Principal {
            subject,
            tenant: Some(tenant),
            grants,
            superadmin: false,
        })
    }
}

/// Rebuild a principal from the claims of a beskar-issued session token.
fn principal_from_session(claims: &Value) -> Result<Principal> {
    if claims.get("iss").and_then(Value::as_str) != Some("beskar") {
        bail!("not a beskar session token");
    }
    match claims.get("exp").and_then(Value::as_u64) {
        Some(exp) if now_secs() < exp + EXP_LEEWAY_SECS => {}
        _ => bail!("session token has expired"),
    }
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .context("session token has no `sub`")?
        .to_string();
    let tenant = claims
        .get("tenant")
        .and_then(Value::as_str)
        .context("session token has no `tenant`")?
        .to_string();
    if !valid_identifier(&tenant) {
        bail!("session token tenant '{tenant}' is invalid");
    }
    let grants = RoleGrants::from_claim(claims.get("roles").unwrap_or(&Value::Null))?;
    Ok(Principal {
        subject,
        tenant: Some(tenant),
        grants,
        superadmin: false,
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Length-checked constant-time string compare.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && openssl::memcmp::eq(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(pairs: &[(&str, &str)]) -> RoleGrants {
        let map = pairs.iter().map(|(c, r)| (c.to_string(), r.to_string())).collect();
        RoleGrants::from_config(&map).unwrap()
    }

    fn tenant_principal(subject: &str, tenant: &str, pairs: &[(&str, &str)]) -> Principal {
        Principal {
            subject: subject.to_string(),
            tenant: Some(tenant.to_string()),
            grants: grants(pairs),
            superadmin: false,
        }
    }

    #[test]
    fn role_capability_ladder() {
        assert!(Role::Reader.allows(Action::Query));
        assert!(!Role::Reader.allows(Action::Ingest));
        assert!(!Role::Reader.allows(Action::Administer));

        assert!(Role::Author.allows(Action::Query));
        assert!(Role::Author.allows(Action::Ingest));
        assert!(!Role::Author.allows(Action::Administer));

        assert!(Role::Admin.allows(Action::Administer));
    }

    #[test]
    fn reader_cannot_ingest_author_cannot_administer() {
        let reader = tenant_principal("r", "acme", &[("*", "reader")]);
        assert!(reader.authorize("kb", Action::Query).is_ok());
        assert!(reader.authorize("kb", Action::Ingest).is_err());

        let author = tenant_principal("a", "acme", &[("*", "author")]);
        assert!(author.authorize("kb", Action::Ingest).is_ok());
        assert!(author.authorize("kb", Action::Administer).is_err());
    }

    #[test]
    fn no_grant_means_no_access() {
        let p = tenant_principal("u", "acme", &[("runbooks", "author")]);
        assert!(p.authorize("runbooks", Action::Ingest).is_ok());
        // No wildcard, no grant for "secrets": denied even to read.
        assert!(p.authorize("secrets", Action::Query).is_err());
    }

    #[test]
    fn specific_grant_overrides_wildcard() {
        let p = tenant_principal("u", "acme", &[("*", "admin"), ("locked", "reader")]);
        assert!(p.authorize("anything", Action::Administer).is_ok());
        assert!(p.authorize("locked", Action::Query).is_ok());
        assert!(p.authorize("locked", Action::Ingest).is_err());
    }

    #[test]
    fn superadmin_can_do_anything_without_a_tenant() {
        let s = Principal::superadmin();
        assert!(s.authorize("any", Action::Administer).is_ok());
        assert_eq!(s.physical_table("kb"), "kb");
    }

    #[test]
    fn tenant_namespacing_prevents_cross_tenant_access() {
        let acme = tenant_principal("a", "acme", &[("*", "admin")]);
        let beta = tenant_principal("b", "beta", &[("*", "admin")]);
        // Same logical corpus name resolves to disjoint physical tables...
        assert_eq!(acme.physical_table("shared"), "t_acme_shared");
        assert_eq!(beta.physical_table("shared"), "t_beta_shared");
        assert_ne!(acme.physical_table("shared"), beta.physical_table("shared"));
        // ...and the tenant comes from the token, not the request, so there is
        // no corpus name acme can pass to reach beta's tables.
        assert!(!acme.physical_table("anything").starts_with("t_beta_"));
    }

    #[test]
    fn identifier_validation_rejects_injection_and_underscores() {
        assert!(valid_identifier("kb"));
        assert!(valid_identifier("runbooks2"));
        assert!(!valid_identifier(""));
        assert!(!valid_identifier("2kb")); // must start with a letter
        assert!(!valid_identifier("my_corpus")); // no underscores
        assert!(!valid_identifier("a b"));
        assert!(!valid_identifier("kb;drop table x")); // injection attempt
        assert!(!valid_identifier(&"a".repeat(MAX_IDENTIFIER_LEN + 1)));
    }

    #[test]
    fn session_token_roundtrip_rebuilds_principal() {
        let auth = Auth {
            session_secret: Some(b"session-signing-secret".to_vec()),
            session_ttl_secs: 3600,
            oidc: None,
            principals: vec![],
        };
        // Forge a session by signing the same claim shape login produces.
        let claims = json!({
            "iss": "beskar", "sub": "alice", "tenant": "acme",
            "roles": {"*": "reader", "runbooks": "author"},
            "iat": now_secs(), "exp": now_secs() + 3600,
        });
        let token = jwt::encode_hs256(&claims, auth.session_secret.as_ref().unwrap()).unwrap();

        let p = auth.authenticate(&token).expect("valid session authenticates");
        assert_eq!(p.subject, "alice");
        assert_eq!(p.tenant.as_deref(), Some("acme"));
        assert!(p.authorize("runbooks", Action::Ingest).is_ok());
        assert!(p.authorize("other", Action::Ingest).is_err()); // wildcard reader
        assert!(p.authorize("other", Action::Query).is_ok());
    }

    #[test]
    fn expired_session_token_is_rejected() {
        let secret = b"session-signing-secret".to_vec();
        let auth = Auth {
            session_secret: Some(secret.clone()),
            session_ttl_secs: 3600,
            oidc: None,
            principals: vec![],
        };
        let claims = json!({
            "iss": "beskar", "sub": "alice", "tenant": "acme",
            "roles": {"*": "reader"}, "iat": 1, "exp": 100,
        });
        let token = jwt::encode_hs256(&claims, &secret).unwrap();
        assert!(auth.authenticate(&token).is_none());
    }

    #[test]
    fn oidc_login_issues_session_for_mapped_groups() {
        let idp_secret = "idp-shared-secret";
        let cfg = OidcConfig {
            issuer: "https://idp.test/".to_string(),
            audience: Some("beskar".to_string()),
            hs256_secret: Some(idp_secret.to_string()),
            rs256_public_key: None,
            tenant_claim: "tenant".to_string(),
            default_tenant: None,
            roles_claim: "groups".to_string(),
            role_map: BTreeMap::from([(
                "beskar-admins".to_string(),
                RoleMapEntry { role: "admin".to_string(), corpus: "*".to_string() },
            )]),
            default_role: None,
        };
        let auth_cfg = AuthConfig {
            session_secret: Some("session-secret".to_string()),
            session_ttl_secs: Some(900),
            oidc: Some(cfg),
            principals: vec![],
        };
        let auth = Auth::from_config(&auth_cfg, &|v| Ok(v.to_string())).unwrap();
        assert!(auth.can_login());

        // Mint an IdP token signed with the shared secret.
        let id_claims = json!({
            "iss": "https://idp.test/", "aud": "beskar", "sub": "carol",
            "tenant": "acme", "groups": ["beskar-admins"], "exp": now_secs() + 600,
        });
        let id_token = jwt::encode_hs256(&id_claims, idp_secret.as_bytes()).unwrap();

        let (session, principal, exp) = auth.login_with_oidc(&id_token).unwrap();
        assert_eq!(principal.subject, "carol");
        assert_eq!(principal.tenant.as_deref(), Some("acme"));
        assert!(exp > now_secs());

        // The issued session token authenticates and carries the admin grant.
        let p = auth.authenticate(&session).expect("session authenticates");
        assert!(p.authorize("anything", Action::Administer).is_ok());
        assert_eq!(p.physical_table("kb"), "t_acme_kb");
    }

    #[test]
    fn oidc_rejects_wrong_issuer_audience_and_signature() {
        let cfg = OidcConfig {
            issuer: "https://idp.test/".to_string(),
            audience: Some("beskar".to_string()),
            hs256_secret: Some("idp-secret".to_string()),
            rs256_public_key: None,
            tenant_claim: "tenant".to_string(),
            default_tenant: None,
            roles_claim: "groups".to_string(),
            role_map: BTreeMap::from([(
                "g".to_string(),
                RoleMapEntry { role: "reader".to_string(), corpus: "*".to_string() },
            )]),
            default_role: None,
        };
        let auth = Auth::from_config(
            &AuthConfig {
                session_secret: Some("s".to_string()),
                session_ttl_secs: None,
                oidc: Some(cfg),
                principals: vec![],
            },
            &|v| Ok(v.to_string()),
        )
        .unwrap();

        // Wrong signature.
        let bad_sig = jwt::encode_hs256(
            &json!({"iss":"https://idp.test/","aud":"beskar","sub":"x","tenant":"acme","groups":["g"],"exp": now_secs()+60}),
            b"wrong-secret",
        )
        .unwrap();
        assert!(auth.login_with_oidc(&bad_sig).is_err());

        // Wrong issuer.
        let bad_iss = jwt::encode_hs256(
            &json!({"iss":"https://evil/","aud":"beskar","sub":"x","tenant":"acme","groups":["g"],"exp": now_secs()+60}),
            b"idp-secret",
        )
        .unwrap();
        assert!(auth.login_with_oidc(&bad_iss).is_err());

        // Wrong audience.
        let bad_aud = jwt::encode_hs256(
            &json!({"iss":"https://idp.test/","aud":"other","sub":"x","tenant":"acme","groups":["g"],"exp": now_secs()+60}),
            b"idp-secret",
        )
        .unwrap();
        assert!(auth.login_with_oidc(&bad_aud).is_err());
    }

    #[test]
    fn static_principal_authenticates_by_token() {
        let auth = Auth::from_config(
            &AuthConfig {
                session_secret: None,
                session_ttl_secs: None,
                oidc: None,
                principals: vec![PrincipalConfig {
                    token: "svc-token-123".to_string(),
                    subject: "ci-bot".to_string(),
                    tenant: "acme".to_string(),
                    roles: BTreeMap::from([("runbooks".to_string(), "author".to_string())]),
                }],
            },
            &|v| Ok(v.to_string()),
        )
        .unwrap();

        assert!(auth.is_configured());
        let p = auth.authenticate("svc-token-123").expect("token authenticates");
        assert_eq!(p.subject, "ci-bot");
        assert!(p.authorize("runbooks", Action::Ingest).is_ok());
        assert!(p.authorize("runbooks", Action::Administer).is_err());
        assert!(auth.authenticate("wrong-token").is_none());
    }
}
