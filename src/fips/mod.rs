//! FIPS 140-3 build/runtime mode (PRD §6.2 E1.9, §8.1).
//!
//! Regulated buyers (FedRAMP / public sector) require that all cryptography use
//! a FIPS 140-3 *validated* module. Beskar's crypto surface is small and already
//! routed through OpenSSL: TLS for Postgres (`postgres-openssl`) and for HTTP
//! (`reqwest` → native-tls → OpenSSL on Linux), plus SHA-256 content hashing,
//! which this module also routes through OpenSSL ([`sha256_hex`]) so a single,
//! switchable provider backs every hash and handshake.
//!
//! The validated module ships as the OpenSSL 3 **FIPS provider**. A build that
//! enables the `fips` Cargo feature loads that provider at startup via
//! [`activate`] and fails closed if it is unavailable, so a FIPS build can never
//! silently fall back to non-validated crypto. A standard build is unaffected.
//! `beskar version` reports the resulting mode via [`status_line`].
//!
//! Activating the provider requires it to be present and configured on the host;
//! the per-platform build/runtime configuration is documented in `docs/fips.md`.

use anyhow::Result;

/// Activate FIPS mode for all subsequent crypto (TLS + hashing).
///
/// On a FIPS build this loads the OpenSSL FIPS provider and keeps it resident
/// for the life of the process, returning an error — so the caller can fail
/// closed — when the validated module is unavailable. On a standard build it is
/// a no-op.
#[cfg(feature = "fips")]
pub fn activate() -> Result<()> {
    use anyhow::Context;
    use openssl::provider::Provider;

    // The FIPS provider is the validated module. The base provider supplies the
    // non-cryptographic algorithms (encoders, serialization) the rest of OpenSSL
    // still needs once a non-default provider is in play. Both are loaded into
    // the default library context and leaked so they outlive every later use.
    let fips = Provider::load(None, "fips").context(
        "could not load the OpenSSL FIPS provider — this binary was built with \
         --features fips but the host has no usable FIPS provider; see docs/fips.md",
    )?;
    let base = Provider::load(None, "base").context("could not load the OpenSSL base provider")?;
    std::mem::forget(fips);
    std::mem::forget(base);
    Ok(())
}

/// No-op on a standard (non-FIPS) build.
#[cfg(not(feature = "fips"))]
pub fn activate() -> Result<()> {
    Ok(())
}

/// Human-readable FIPS status for `beskar version`.
///
/// Unlike [`activate`] this never aborts: a FIPS build whose validated module
/// cannot be loaded reports the reason so operators can diagnose it.
pub fn status_line() -> String {
    #[cfg(not(feature = "fips"))]
    {
        "FIPS mode: unavailable (standard build; rebuild with --features fips)".to_string()
    }
    #[cfg(feature = "fips")]
    {
        match activate() {
            Ok(()) => "FIPS mode: active (OpenSSL FIPS provider loaded)".to_string(),
            Err(e) => format!("FIPS mode: inactive — {e:#}"),
        }
    }
}

/// SHA-256 over `data`, returned as lowercase hex.
///
/// Routed through OpenSSL (rather than a separate pure-Rust crate) so that in a
/// FIPS build the digest comes from the validated module. SHA-256 is permitted
/// under FIPS, so this succeeds in both modes; an OpenSSL failure here would
/// mean a broken crypto install and is treated as unrecoverable.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), data)
        .expect("OpenSSL SHA-256 must be available");
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vectors() {
        // Standard SHA-256 of "hello" and of the empty string.
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_is_64_hex_chars_and_stable() {
        let a = sha256_hex(b"beskar");
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, sha256_hex(b"beskar"));
    }

    #[test]
    fn status_line_reports_a_mode() {
        let line = status_line();
        assert!(line.starts_with("FIPS mode: "));
        // The default test build has no `fips` feature, so it must report that
        // rather than claiming validated crypto.
        #[cfg(not(feature = "fips"))]
        assert!(line.contains("unavailable"));
    }
}
