# Dockerfile for Multiway Data Plane
#
# This builds the Pingora-based HTTP proxy that handles actual traffic.
# Each Gateway resource gets its own data plane deployment.

ARG RUST_VERSION=1.87

# Build stage
FROM rust:${RUST_VERSION}-bookworm AS builder

WORKDIR /build

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    clang \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace files (needed for workspace dependencies)
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src

# Build the dataplane binary
RUN cargo build --release -p multiway-dataplane

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -r -u 1000 -U dataplane

# Create config directory
RUN mkdir -p /etc/multiway && chown dataplane:dataplane /etc/multiway

# Copy the binary
COPY --from=builder /build/target/release/multiway-dataplane /usr/local/bin/multiway-dataplane

USER dataplane

# Default config path
ENV CONFIG_PATH=/etc/multiway/config.json

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/multiway-dataplane"]
CMD ["--config", "/etc/multiway/config.json"]
