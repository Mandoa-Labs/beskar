# Identity & access — SSO, RBAC, tenant isolation (E2.2 / E2.3 / E2.5, §9.2)

`beskar serve` enforces **who a caller is** and **what they may do**, server-side.
This is the M9 layer on top of the M8 server core ([server.md](server.md)):

- **SSO (E2.2)** — users authenticate via an OIDC identity provider and receive a
  **short-lived token** with `beskar login`. The CLI never holds database
  credentials; it only talks to the server.
- **RBAC (E2.3)** — `reader` ⊂ `author` ⊂ `admin`, enforced per corpus.
- **Tenant isolation (E2.5)** — a corpus is namespaced to its owning tenant
  *server-side*, so cross-tenant access is structurally impossible.

All token crypto uses the existing OpenSSL dependency (HS256/RS256 JWT), so the
FIPS posture ([fips.md](fips.md)) is unaffected and no new crates are added.

## Roles & actions

| Role | query | ingest | administer (create/drop corpus) |
| --- | :---: | :---: | :---: |
| `reader` | ✅ | — | — |
| `author` | ✅ | ✅ | — |
| `admin`  | ✅ | ✅ | ✅ |

A principal's grants are a map of `corpus -> role`, with `*` as a tenant-wide
default. A specific corpus grant overrides the wildcard (so a wildcard `admin`
can be narrowed to `reader` on one sensitive corpus, or vice versa).

## How a caller is authenticated

A request carries `Authorization: Bearer <token>`. The server resolves it, in
order, to a **principal**:

1. **Shared super-admin token** (`--token` / `BESKAR_SERVE_TOKEN`) — the operator.
   Full access, *not* tenant-scoped (addresses corpora by raw name). This is the
   M8 single-token behavior, preserved.
2. **Static principal token** — a service account configured in `auth.principals`.
3. **Session token** — a short-lived beskar-issued JWT obtained from `/v1/login`.

Unknown or expired credentials get `401`.

## Tenant isolation (E2.5)

A tenant-scoped principal's logical corpus name (e.g. `runbooks`) is mapped to a
physical table prefix **derived from the token's tenant**:

```
tenant "acme" + corpus "runbooks"  ->  physical tables  t_acme_runbooks_{documents,chunks,meta}
```

Because the tenant comes from the authenticated token — never from the request
body — there is no corpus name a caller can send to reach another tenant's
tables. Corpus and tenant identifiers are validated as `^[a-z][a-z0-9]*$`
(≤ 40 chars): no underscores, so `t_<tenant>_<corpus>` is unambiguous, and the
only untrusted value reaching a SQL table name is constrained to a safe charset.

The super-admin (shared token) is not tenant-scoped and uses raw corpus names.

## Configuration (`auth:` block in the server's `config.yaml`)

Everything is optional; with no `auth` block the server behaves exactly as M8
(shared token only).

```yaml
auth:
  # Signs/verifies the short-lived session tokens issued by /v1/login.
  # May be a literal or a scheme:// secret reference (E1.1). Required for SSO.
  session_secret: secret://beskar-session-secret
  session_ttl_secs: 3600                 # default 3600

  # Trusted OIDC identity provider (SSO).
  oidc:
    issuer: "https://idp.corp.example/"  # must match the token's `iss`
    audience: "beskar"                   # must be in the token's `aud` (if set)
    # Exactly one signing key:
    hs256_secret: secret://idp-shared-secret
    # rs256_public_key: |                # ...or an RSA public key (PEM)
    #   -----BEGIN PUBLIC KEY-----
    #   ...
    tenant_claim: "tenant"               # claim that carries the tenant (default)
    default_tenant: "acme"               # used if the token has no tenant claim
    roles_claim: "groups"                # claim with the caller's groups (default)
    role_map:                            # IdP group -> role grant
      beskar-admins:   { role: admin }              # corpus defaults to "*"
      runbook-authors: { role: author, corpus: runbooks }
    default_role: reader                 # granted on "*" if no group matched

  # Static principals (service accounts), authenticated by a bearer token.
  principals:
    - token: secret://acme-ci-token
      subject: acme-ci
      tenant: acme
      roles:
        "*": reader
        runbooks: author
```

## `beskar login`

```bash
# 1. Obtain an OIDC ID token from your IdP (corporate SSO, az/gcloud/oidc helper…).
export BESKAR_ID_TOKEN="$(your-idp-helper ...)"

# 2. Exchange it for a short-lived beskar token (stored at ~/.config/beskar/session.yaml, 0600).
beskar login --server https://beskar.corp.internal

# 3. Query a corpus through the server — no DB credentials on this machine.
beskar generate --corpus runbooks --query "deploy runbook for service X?"
```

`beskar login` POSTs the ID token to `POST /v1/login`; the server validates it
(signature, `iss`, `aud`, `exp`), maps its claims to a tenant + roles, and returns
a short-lived session token. Service accounts that already hold a static token can
skip SSO with `beskar login --server <url> --token <token>`.

`beskar generate --corpus <name>` (client mode) talks to the server with the
stored token; `beskar generate --table-name <name>` remains the direct-to-Postgres
mode for the operator.

## Endpoints added in M9

| Method & path | Auth | Role | Purpose |
| --- | --- | --- | --- |
| `POST /v1/login` | IdP ID token in body | — | Exchange an OIDC ID token for a session token |
| `GET /v1/whoami` | bearer | any | The authenticated principal (subject, tenant, roles) |
| `POST /v1/admin/corpus/create` | bearer | admin | Create a (tenant-namespaced) corpus |
| `POST /v1/admin/corpus/drop` | bearer | admin | Drop a (tenant-namespaced) corpus |

`POST /v1/ingest` now requires `author`; `POST /v1/query` requires `reader`. Both
accept `corpus` (preferred) or the legacy `table_name` alias. A role denial
returns `403` with a clear reason — enforced server-side, never client-trusted.

Every authenticated request is recorded in the audit log (E1.8) **attributed to
the authenticated subject** (PRD §5.6), including denials.

## SAML

The acceptance target names OIDC **and** SAML. SAML assertions are consumed via an
**OIDC bridge**: point `auth.oidc` at an IdP/broker (Keycloak, Dex, Microsoft
Entra ID, Okta, …) that performs SAML with your enterprise IdP and issues OIDC ID
tokens to beskar. This keeps beskar's trust surface to signed JWTs (verified with
the FIPS-validated OpenSSL module) rather than embedding an XML-DSig stack. A
native SAML acceptor can be added later behind the same `Principal` model.
