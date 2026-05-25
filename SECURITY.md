# Security Policy

Beskar is **customer-operated software**, not a hosted service. It runs on your
machines, against your PostgreSQL instance and your model endpoints. There is no
Beskar-operated backend that can be attacked on your behalf — but the binary,
its dependencies, and its handling of your secrets still matter, and we treat
vulnerabilities in them seriously.

## Supported versions

Security fixes are applied to the latest release. Older releases are not
patched; please upgrade to the most recent [GitHub
release](https://github.com/Mandoa-Labs/beskar/releases/latest).

| Version            | Supported          |
| ------------------ | ------------------ |
| Latest release     | :white_check_mark: |
| Older releases     | :x:                |

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately via GitHub's coordinated-disclosure channel:

1. Go to the repository's **Security** tab.
2. Choose **Report a vulnerability** (GitHub Private Vulnerability Reporting).
3. Describe the issue, including affected version, reproduction steps, and
   impact. A proof-of-concept and your suggested remediation are welcome.

This routes the report to the maintainers privately and lets us collaborate on a
fix and a CVE before any public disclosure.

If GitHub private reporting is unavailable to you, contact the maintainer
(`@evanallen13`) to arrange a private channel before sharing details.

## Response targets (SLA)

We aim to meet the following timelines, in business days:

| Stage                                   | Target            |
| --------------------------------------- | ----------------- |
| Acknowledge receipt                     | 3 business days   |
| Initial assessment & severity triage    | 7 business days   |
| Fix or mitigation for High/Critical     | 30 calendar days  |
| Coordinated public disclosure           | by mutual agreement, default 90 days |

We will keep you updated as we work the report, and we are happy to credit you
in the release notes and advisory unless you prefer to remain anonymous.

## Scope

In scope:

- The `beskar` binary and source in this repository.
- Mishandling of secrets (leakage to logs, argv, audit events, or disk).
- Egress controls and air-gap (`--offline`) bypasses.
- The release pipeline and published artifacts (signing, SBOM, provenance).

Out of scope:

- Vulnerabilities in your own PostgreSQL deployment, model endpoints, or secret
  store — those are operated by you.
- Issues that require a pre-compromised host or already-leaked credentials.

## Verifying releases

Every release is signed and accompanied by checksums, an SBOM, and SLSA build
provenance. See the [README](README.md#supply-chain-security) for how to verify
an artifact's signature, checksum, and provenance before installing it.
