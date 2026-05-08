
FROM rust:1.88-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-deb

WORKDIR /app

COPY . .
RUN cargo build --release && cargo deb

FROM debian:bookworm-slim AS runtime

# Bind-mount the builder's debian output dir and install directly — no COPY, no cleanup needed
RUN --mount=type=bind,from=builder,source=/app/target/debian,target=/pkgs \
    dpkg -i /pkgs/*.deb || apt-get install -fy

CMD ["beskar"]
