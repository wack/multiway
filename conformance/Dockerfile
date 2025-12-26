# Dockerfile for Gateway API Conformance Tests
#
# This image downloads and runs the official Gateway API conformance test suite.
# It's designed to run inside a Kubernetes cluster as a Job.

ARG GO_VERSION=1.24
ARG GATEWAY_API_VERSION=1.4.1

# Build stage: compile the conformance test binary
FROM golang:${GO_VERSION}-alpine AS builder

ARG GATEWAY_API_VERSION

RUN apk add --no-cache git make

WORKDIR /build

# Clone the gateway-api repository at the specified version
RUN git clone --depth 1 --branch v${GATEWAY_API_VERSION} \
    https://github.com/kubernetes-sigs/gateway-api.git .

# Build the conformance test binary
# Build from repo root, targeting the conformance package specifically
RUN CGO_ENABLED=0 GOOS=linux go test -c -o /conformance.test ./conformance

# Runtime stage: minimal image with kubectl and the test binary
FROM alpine:3.21

RUN apk add --no-cache curl bash

# Install kubectl
ARG KUBECTL_VERSION=1.31.4
RUN curl -LO "https://dl.k8s.io/release/v${KUBECTL_VERSION}/bin/linux/amd64/kubectl" && \
    chmod +x kubectl && \
    mv kubectl /usr/local/bin/

# Copy the conformance test binary
COPY --from=builder /conformance.test /usr/local/bin/conformance.test

# Copy entrypoint script
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Default environment variables (can be overridden)
ENV GATEWAY_CLASS_NAME="multiway"
ENV SUPPORTED_FEATURES="Gateway,HTTPRoute"
ENV CONFORMANCE_PROFILES=""
ENV EXEMPT_FEATURES=""
ENV SKIP_TESTS=""
ENV CLEANUP_BASE_RESOURCES="true"
ENV SHOW_DEBUG="false"
ENV REPORT_OUTPUT=""

ENTRYPOINT ["/entrypoint.sh"]
