# Dockerfile for Multiway Gateway Controller (Control Plane)
#
# This builds the Kubernetes controller that manages Gateway API resources
# and provisions data plane instances.

ARG RUST_VERSION=1.87

# Build stage
FROM rust:${RUST_VERSION}-bookworm AS builder

WORKDIR /build

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src

# Build release binary
RUN cargo build --release --bin multiway

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -r -u 1000 -U multiway

# Copy the binary
COPY --from=builder /build/target/release/multiway /usr/local/bin/multiway

USER multiway

ENTRYPOINT ["/usr/local/bin/multiway"]
CMD ["controller", "--help"]
