# Control-mapping appendix — SOC 2 / NIST 800-53 (§11)

This appendix maps each Beskar enterprise-hardening control (PRD §6.2 Phase E1)
and supply-chain control (PRD §12) to the **SOC 2 Trust Services Criteria** and
the **NIST SP 800-53 Rev. 5** control families they support, with a pointer to
the implementation/evidence in this repository. It exists to answer SIG / CAIQ
vendor-security questionnaires directly.

## Scope and shared responsibility

Beskar is **customer-operated software, not a hosted service** (see
[SECURITY.md](../SECURITY.md)). It is therefore not itself SOC 2 / FedRAMP
*certified*; instead it provides **controls and evidence** that you fold into
*your* compliance program, which your auditor assesses against your environment.
The split:

- **Beskar provides** — the control *capabilities* below (encryption, secret
  handling, audit events, egress restriction, FIPS mode, signed/attested
  releases) and the documentation/evidence for each.
- **You operate** — your PostgreSQL instance, model endpoints, secret store, key
  rotation, log retention/SIEM, access management, and the policies and
  monitoring that turn these capabilities into satisfied controls.

The 800-53 control IDs are the families/controls Beskar **contributes to**; full
satisfaction generally requires organizational controls beyond the tool.

## Phase E1 controls

| Control | Beskar implementation / evidence | SOC 2 (TSC) | NIST 800-53 Rev. 5 |
|---|---|---|---|
| **E1.1** Pluggable secret backend | `scheme://` secret references resolved at runtime (`src/secrets/`, [README §Secret backends](../README.md#secret-backends)); no literal secrets required on disk | CC6.1 | IA-5, SC-12, SC-28 |
| **E1.2** Plaintext-fallback warning + `config lint` | Warns on literal secrets / lax file modes; `beskar config lint` exits non-zero on findings (`src/utils/` `lint()`) | CC6.1, CC7.1 | IA-5, CM-6 |
| **E1.3** No secret leakage | Redaction registry scrubs secrets from logs, audit events, errors, `--verbose`; DB password passed via connection params, never argv (`src/secrets/` redaction, `src/database/connect`) | CC6.1, C1.1 | AU-9, SC-28, IA-5, AC-6 |
| **E1.4** Private model endpoints | `azure-openai` / `bedrock` / `openai-compatible` with arbitrary `base_url`; embedding + generation configured independently, keeping corpus data in the customer boundary (`src/utils/`, `src/embed/`, `src/generate/`) | CC6.6, CC6.7, C1.1 | SC-7, AC-4 |
| **E1.5** Embedding-model/dimension guard | Corpus records model + vector dimension; mismatched ingest/query is refused (`src/database/` `_meta`, guards in `document`/`generate`) | CC7.1, PI1.2 | SI-7, CM-6 |
| **E1.6** Egress controls | Proxy + custom CA bundle + host allowlist; `--offline` fails closed on any non-allowlisted egress (`src/net/`) | CC6.6, CC6.7 | SC-7, SC-7(5), AC-4 |
| **E1.7** Postgres TLS hardening | `sslmode` up to `verify-full` with pinned root CA + optional client-cert mTLS (`src/database/connect`) | CC6.1, CC6.7 | SC-8, SC-13, SC-23, IA-5 |
| **E1.8** Structured audit log | JSON event per security-relevant action (timestamp, actor, host, command, corpus, outcome; no secrets) to stderr/file/syslog; documented schema ([`docs/audit-log.md`](audit-log.md), `src/audit/`) | CC7.2, CC7.3 | AU-2, AU-3, AU-9, AU-12 |
| **E1.9** FIPS build/runtime mode | All TLS + hashing via the OpenSSL 3 FIPS 140-3 validated provider; `beskar version` reports status; fails closed ([`docs/fips.md`](fips.md), `src/fips/`) | CC6.1, CC6.7 | SC-13, SC-8, SC-28 |
| **E1.10** Deterministic config via flags/env | Every `init` prompt has a flag/env equivalent; `--non-interactive` fails closed on missing values, enabling reproducible golden-image config (`src/init/`) | CC8.1, CC7.1 | CM-2, CM-6 |
| **E1.11** Data-handling controls | Optional pre-embedding PII/secret redaction + per-provider "what leaves the machine" data-flow statement ([`docs/data-flow.md`](data-flow.md), `src/redact/`) | C1.1, CC6.7, P4.1 | SI-19, AC-4, SC-7 |
| **E1.12** Backup/restore + `db --verify` | Documented backup/restore procedure + structural integrity check with pass/fail exit ([`docs/backup-restore.md`](backup-restore.md), `db --verify`) | A1.2, CC7.1, PI1.2 | CP-9, CP-10, SI-7 |

## Supply-chain controls (PRD §12)

| Control | Beskar implementation / evidence | SOC 2 (TSC) | NIST 800-53 Rev. 5 |
|---|---|---|---|
| **§12.1** Artifact signing + checksums | Sigstore keyless signature + `SHA256SUMS` per release (`.github/workflows/release.yml`, [`docs/release-verification.md`](release-verification.md)) | CC6.8, CC8.1 | SR-4, SR-11, CM-14 |
| **§12.2** SBOM per release | CycloneDX SBOM generated from `Cargo.lock` and attached (`beskar.cdx.json`) | CC6.8, CC7.1 | SR-3, SR-4, SA-15 |
| **§12.3** SLSA build provenance | Verifiable build-provenance attestation per artifact (`actions/attest-build-provenance`) | CC8.1 | SR-4, SR-11 |
| **§12.4–5** Vulnerability + license gates | `cargo audit` (RustSec) + `cargo deny` (bans/licenses/sources) gate CI (`deny.toml`, supply-chain job) | CC7.1, CC6.8 | RA-5, SR-3, SA-10 |
| **§12.6** Reproducible build + pinned toolchain | Pinned `rust-toolchain.toml` + reproducible-build recipe ([`docs/reproducible-builds.md`](reproducible-builds.md)) | CC8.1 | SA-15, SR-4, CM-2 |
| **§12.7** Coordinated disclosure | [`SECURITY.md`](../SECURITY.md): private reporting channel + response SLA | CC7.3, CC2.3 | IR-6, SI-5 |

## Notes

- **SOC 2 TSC** references are to the AICPA 2017 Trust Services Criteria (CC =
  Common Criteria; A = Availability; C = Confidentiality; PI = Processing
  Integrity; P = Privacy). A/C/PI/P criteria apply only if in your audit scope.
- **800-53** references are families/controls from Rev. 5, including the SR
  (Supply Chain Risk Management) family. They denote *contribution*, not
  standalone satisfaction.
- This appendix is maintained alongside [PRD §11](../PRD.md#11-compliance--certifications);
  update it whenever an E1 / §12 control is added or changed.
