# FIPS mode (E1.9, §8.1)

Regulated and public-sector deployments (FedRAMP, FISMA) require that **all
cryptography use a FIPS 140-3 validated module**. Beskar supports a build/runtime
mode in which every TLS handshake and hash runs under OpenSSL 3's FIPS provider,
and `beskar version` reports whether that mode is active.

This mode is **off by default**: a stock build uses OpenSSL's standard providers
and behaves exactly as before.

## What FIPS mode covers

Beskar's cryptographic surface is small and routed entirely through OpenSSL, so a
single provider switch covers all of it:

| Operation | Path | Module under FIPS mode |
| --- | --- | --- |
| Postgres TLS | `postgres-openssl` → OpenSSL | FIPS provider |
| Outbound HTTPS (embeddings, generation, Key Vault) | `reqwest` → native-tls → OpenSSL (Linux) | FIPS provider |
| Content hashing (SHA-256, ingestion idempotency) | `openssl::hash` | FIPS provider |

There is no separate pure-Rust hashing path: SHA-256 goes through OpenSSL
specifically so it shares the validated module (`src/fips/mod.rs`).

## How it works

A binary built with the `fips` Cargo feature calls `fips::activate()` at startup
before any subcommand does cryptographic work. That function loads the OpenSSL
**FIPS provider** (the validated module) plus the **base provider** (for the
non-cryptographic algorithms OpenSSL still needs) into the default library
context and keeps them resident.

If the FIPS provider cannot be loaded, `activate()` returns an error and the
command **fails closed** — a FIPS build never silently falls back to
non-validated crypto. The `version` subcommand is the one exception: it reports
the failure instead of aborting, so operators can diagnose a misconfigured host.

## Building

```bash
cargo build --release --locked --features fips
```

The `fips` feature adds no crates; it only changes runtime behavior. It requires
linking against an **OpenSSL 3** build whose FIPS provider is available
(`openssl::provider` is OpenSSL 3-only). On Linux beskar links the system
OpenSSL, so the host's OpenSSL must be 3.x with the FIPS provider installed; the
macOS/Windows builds vendor OpenSSL 3.

## Host configuration

Loading the provider requires it to be present and registered on the host. The
validated module and its configuration are supplied by the OS/OpenSSL vendor, not
by beskar:

1. **Install the FIPS provider** for your OpenSSL 3 distribution (for example,
   the `openssl-fips` / FIPS-validated package for your platform, or a build
   produced with `./Configure enable-fips`). Generate the module config with
   `openssl fipsinstall` if your distribution does not ship one.
2. **Register it** in `openssl.cnf` (or `fipsmodule.cnf`) so the `fips` provider
   section is active. To force validated crypto for the whole host, set the
   default properties to `fips=yes` and activate the `base` provider alongside
   `fips`.
3. **Verify** with `openssl list -providers`, which should show the `fips`
   provider as active.

Use a host (or container image) whose OpenSSL build is itself
FIPS 140-3 validated for your compliance boundary — enabling the provider on an
unvalidated build is a functional switch, not a certification.

## Verifying

```bash
$ beskar version
beskar 0.1.0
FIPS mode: active (OpenSSL FIPS provider loaded)
```

A standard build instead reports:

```
FIPS mode: unavailable (standard build; rebuild with --features fips)
```

and a FIPS build whose host lacks a usable provider reports
`FIPS mode: inactive — <reason>` (and every other subcommand exits non-zero).

## Platform support

| Platform | FIPS mode |
| --- | --- |
| Linux (x86_64/arm64) | Supported; requires a system OpenSSL 3 with the FIPS provider installed and configured. |
| macOS / Windows | Build links a vendored OpenSSL 3; the FIPS provider must still be present/configured at runtime. Linux is the primary supported FIPS target. |
