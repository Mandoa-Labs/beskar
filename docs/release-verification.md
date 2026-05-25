# Verifying a release (§12.1–12.3)

Every Beskar release is built by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
and ships with the supply-chain evidence needed to verify it independently:

| Asset | Purpose | Issue |
| --- | --- | --- |
| `*.deb` `*.rpm` `*.tar.gz` `*.zip` | the build artifacts | — |
| `SHA256SUMS` | SHA-256 of every published asset | #54 |
| `<asset>.cosign.bundle` | Sigstore keyless signature, one per asset | #54 |
| `beskar.cdx.json` | CycloneDX SBOM of all crate dependencies | #55 |
| (attestation API) | SLSA build provenance, one per artifact | #56 |

All signing is **keyless** (Sigstore): there is no long-lived private key. The
signing identity is the release workflow's GitHub OIDC identity, recorded in a
public Rekor transparency log and checked at verification time.

## 0. Download the release

```bash
gh release download release-<N> --repo Mandoa-Labs/beskar --dir beskar-release
cd beskar-release
```

## 1. Checksums

```bash
sha256sum -c SHA256SUMS
```

Every listed asset must report `OK`.

## 2. Signatures

Each asset (including `SHA256SUMS` and the SBOM) has a `*.cosign.bundle`. Verify
with [cosign](https://github.com/sigstore/cosign), pinning the expected signer:

```bash
cosign verify-blob \
  --bundle beskar-debian-amd64.deb.cosign.bundle \
  --certificate-identity-regexp '^https://github.com/Mandoa-Labs/beskar/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  beskar-debian-amd64.deb
```

The two `--certificate-*` flags are essential: without them cosign will accept a
signature from *any* identity. They assert the artifact was signed by the Beskar
release workflow and no one else.

To verify everything at once:

```bash
for f in *; do
  case "$f" in *.cosign.bundle) continue;; esac
  [ -f "$f.cosign.bundle" ] || continue
  cosign verify-blob \
    --bundle "$f.cosign.bundle" \
    --certificate-identity-regexp '^https://github.com/Mandoa-Labs/beskar/' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    "$f"
done
```

## 3. SLSA build provenance

GitHub stores a SLSA provenance attestation for each artifact. Verify it with
the GitHub CLI (no extra tooling):

```bash
gh attestation verify beskar-debian-amd64.deb --repo Mandoa-Labs/beskar
```

This confirms the artifact was produced by this repository's Actions workflow
from a specific commit. To verify offline with `slsa-verifier` or `cosign`,
fetch the bundle first:

```bash
gh attestation download beskar-debian-amd64.deb --repo Mandoa-Labs/beskar
```

## 4. SBOM

`beskar.cdx.json` is a [CycloneDX](https://cyclonedx.org/) SBOM generated from
`Cargo.lock`; it enumerates every crate dependency and version. Feed it to your
vulnerability scanner of choice, for example:

```bash
grype sbom:beskar.cdx.json
```

## 5. Reproduce the build (optional)

The integrity signals above are authoritative. To additionally rebuild the
binary from source and compare checksums, follow
[`reproducible-builds.md`](reproducible-builds.md).
