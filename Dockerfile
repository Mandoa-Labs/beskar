# =============================================================================
# Stage 1: Builder
#   - Compiles the Rust binary
#   - Packages it into a .deb using cargo-deb
# =============================================================================
FROM rust:1.88-slim-bookworm AS builder

# Install cargo-deb and any build-time deps
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-deb

WORKDIR /app

# Copy the project source
COPY . .

# Build in release mode and produce a .deb package
# The .deb is written to target/debian/<package>_<version>_<arch>.deb
RUN cargo build --release && cargo deb

# =============================================================================
# Stage 2: Runtime
#   - Minimal Debian image
#   - Copies and installs only the .deb from Stage 1
#   - Runs the installed binary
# =============================================================================
FROM debian:bookworm-slim AS runtime

# Bind-mount the builder's debian output dir and install directly — no COPY, no cleanup needed
RUN --mount=type=bind,from=builder,source=/app/target/debian,target=/pkgs \
    dpkg -i /pkgs/*.deb || apt-get install -fy

CMD ["beskar"]
