# rust-cli-template
A template repository for Rust programs executed from the CLI (including webservers).

## Prerequisites

- [kopium](https://github.com/kube-rs/kopium) - Required for generating Rust bindings from Kubernetes CRDs

## Gateway API CRDs

This project includes Kubernetes Gateway API CRD bindings. To update the CRDs and regenerate Rust bindings:

```bash
# Update GATEWAY_API_VERSION in Makefile.toml, then run:
cargo make gateway-api-sync
```

This downloads the CRD definitions to `.crds/v<version>/` and generates Rust bindings in `crates/gateway-crds/src/`.
