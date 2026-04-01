# Unified Dockerfile for Multiway Gateway (Control Plane + Data Plane)
#
# Alpine-based build with static musl linking and distroless runtime.
# Single builder stage compiles both binaries to share dependency compilation.
#
# Build from workspace root:
#   docker build --target controlplane -t multiway:latest .
#   docker build --target dataplane -t multiway-dataplane:latest .

ARG RUST_VERSION=1.92

# =============================================================================
# Builder Stage - Compiles BOTH binaries with shared dependencies
# =============================================================================
FROM rust:${RUST_VERSION}-alpine AS builder

ARG TARGETARCH

# Install ALL build dependencies (union of controlplane + dataplane needs)
# - musl-dev, pkgconfig: basic Rust/C compilation
# - cmake, perl, make: aws-lc-rs (controlplane TLS)
# - clang, clang-dev, linux-headers, g++: monoio/io-uring (dataplane proxy)
RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    cmake \
    perl \
    make \
    clang \
    clang-dev \
    linux-headers \
    g++

# Add appropriate musl target based on architecture
RUN case "${TARGETARCH}" in \
    "amd64") echo "x86_64-unknown-linux-musl" > /rust-target.txt ;; \
    "arm64") echo "aarch64-unknown-linux-musl" > /rust-target.txt ;; \
    *) echo "x86_64-unknown-linux-musl" > /rust-target.txt ;; \
    esac && rustup target add $(cat /rust-target.txt)

WORKDIR /build

# Copy workspace Cargo files for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/controlplane/Cargo.toml ./crates/controlplane/
COPY crates/dataplane/Cargo.toml ./crates/dataplane/
COPY crates/gateway-crds/Cargo.toml ./crates/gateway-crds/

# Create dummy source files to cache dependencies
# Build BOTH packages in one command to share dependency compilation
RUN mkdir -p crates/controlplane/src/bin crates/dataplane/src crates/gateway-crds/src && \
    echo "fn main() {}" > crates/controlplane/src/bin/main.rs && \
    echo "" > crates/controlplane/src/lib.rs && \
    echo "fn main() {}" > crates/dataplane/src/main.rs && \
    echo "" > crates/gateway-crds/src/lib.rs && \
    cargo build --release --target $(cat /rust-target.txt) \
        -p multiway \
        -p multiway-dataplane && \
    rm -rf crates

# Copy actual source code
COPY . .

# Build BOTH real binaries in one command
# Touch all source files to invalidate cache
RUN touch crates/controlplane/src/lib.rs \
          crates/controlplane/src/bin/main.rs \
          crates/dataplane/src/main.rs \
          crates/gateway-crds/src/lib.rs && \
    cargo build --release --target $(cat /rust-target.txt) \
        -p multiway --bin multiway \
        -p multiway-dataplane --bin multiway-dataplane

# Copy binaries to predictable locations
RUN cp /build/target/$(cat /rust-target.txt)/release/multiway /multiway && \
    cp /build/target/$(cat /rust-target.txt)/release/multiway-dataplane /multiway-dataplane

# Create config directory structure for dataplane (distroless can't mkdir)
RUN mkdir -p /etc/multiway

# =============================================================================
# Control Plane Runtime Stage
# =============================================================================
FROM gcr.io/distroless/static:nonroot AS controlplane

COPY --from=builder /multiway /usr/local/bin/multiway

ENTRYPOINT ["/usr/local/bin/multiway"]
CMD ["controller", "--help"]

# =============================================================================
# Data Plane Runtime Stage
# =============================================================================
FROM gcr.io/distroless/static:nonroot AS dataplane

# Copy the binary
COPY --from=builder /multiway-dataplane /usr/local/bin/multiway-dataplane

# Copy config directory (owned by nonroot user in distroless)
COPY --from=builder --chown=nonroot:nonroot /etc/multiway /etc/multiway

# Default config path (matches binary default and control plane mount point)
ENV CONFIG_PATH=/config/config.json

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/multiway-dataplane"]
# Config path is set via CONFIG_PATH env var by the control plane
# Default in binary is /config/config.json which matches control plane mount
