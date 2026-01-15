# Dockerfile for Multiway Gateway Controller (Control Plane)
#
# Alpine-based build with static musl linking and distroless runtime.
#
# Build from workspace root:
#   docker build -t multiway:latest -f crates/controlplane/Dockerfile .

ARG RUST_VERSION=1.92

# Build stage
FROM rust:${RUST_VERSION}-alpine AS builder

ARG TARGETARCH

# Install build dependencies (aws-lc-rs needs cmake, perl, make)
RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    cmake \
    perl \
    make

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
RUN mkdir -p crates/controlplane/src/bin crates/dataplane/src crates/gateway-crds/src && \
    echo "fn main() {}" > crates/controlplane/src/bin/main.rs && \
    echo "" > crates/controlplane/src/lib.rs && \
    echo "fn main() {}" > crates/dataplane/src/main.rs && \
    echo "" > crates/gateway-crds/src/lib.rs && \
    cargo build --release --target $(cat /rust-target.txt) -p multiway && \
    rm -rf crates && \
    rm -rf /usr/local/cargo/registry/cache && \
    rm -rf /usr/local/cargo/git/checkouts

# Copy actual source code
COPY . .

# Build the real binary (touch to invalidate cache for lib, bin, and dependencies)
RUN touch crates/controlplane/src/lib.rs crates/controlplane/src/bin/main.rs crates/gateway-crds/src/lib.rs && \
    cargo build --release --target $(cat /rust-target.txt) -p multiway --bin multiway

# Copy binary to predictable location and strip debug symbols
RUN cp /build/target/$(cat /rust-target.txt)/release/multiway /multiway && \
    strip /multiway

# Clean up build artifacts to reduce final layer size
RUN rm -rf /build/target /usr/local/cargo/registry /usr/local/cargo/git

# Runtime stage
FROM gcr.io/distroless/static:nonroot

COPY --from=builder /multiway /usr/local/bin/multiway

ENTRYPOINT ["/usr/local/bin/multiway"]
CMD ["controller", "--help"]
