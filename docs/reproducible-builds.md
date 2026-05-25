# Reproducible builds (§12.6)

Beskar pins its compiler and dependencies so that a given commit builds the same
way everywhere, and so a third party can rebuild a release and compare it to the
published, signed artifacts.

## What is pinned

- **Toolchain** — [`rust-toolchain.toml`](../rust-toolchain.toml) pins the exact
  Rust version (`channel`), components, and cross-compilation targets. `rustup`
  honors this file for every `cargo`/`rustc` invocation in the repo, in CI and
  locally, and auto-installs the pinned toolchain if it is missing.
- **Dependencies** — [`Cargo.lock`](../Cargo.lock) is committed and exact crate
  versions are resolved from it. Always build with `--locked` to fail closed if
  the lock and manifest disagree.

## Rebuild recipe

```bash
git clone https://github.com/Mandoa-Labs/beskar
cd beskar
git checkout <release-tag>          # the tag of the release you are verifying

# rustup will install the pinned toolchain from rust-toolchain.toml on first use.
cargo build --release --locked --target x86_64-unknown-linux-gnu

sha256sum target/x86_64-unknown-linux-gnu/release/beskar
```

Compare the printed checksum against the `SHA256SUMS` file attached to the
release, and verify the signature and SLSA provenance as described in the
[README](../README.md#supply-chain-security).

## Known, justified deltas

Byte-for-byte reproducibility of Rust binaries depends on factors outside the
source tree. When checksums differ, it is almost always one of the following —
none of which indicate tampering:

- **Absolute paths** embedded in panic messages and debug info. Builds in
  different working directories differ unless paths are remapped. To minimize
  this, build from an identical path or add:

  ```bash
  export RUSTFLAGS="--remap-path-prefix=$PWD=/build --remap-path-prefix=$HOME/.cargo=/cargo"
  ```

- **`$CARGO_HOME` / registry layout** differences, addressed by the remap above.
- **Build timestamps.** Set `SOURCE_DATE_EPOCH` to the commit time for any step
  that embeds a date.
- **Linker / system library versions** on the build host, especially for the
  dynamically-linked Linux `.deb` (which intentionally links the system
  `libssl`). The macOS and Windows builds vendor OpenSSL and are less sensitive.
- **Compression metadata** in the packaged `.deb` / `.rpm` / `.tar.gz` / `.zip`.
  Verify the **inner binary** checksum rather than the archive when in doubt.

The signed `SHA256SUMS` and SLSA provenance are the authoritative integrity
signal; this recipe lets you independently corroborate them.
