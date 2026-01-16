# Claude Agents for Multiway

This directory contains Claude Code agents that help automate common development and testing workflows for the multiway Kubernetes Gateway API implementation.

## Available Agents

### docker-builder

**Purpose**: Build Docker images for the control plane and data plane components.

**Usage**: Invoke when you need to build container images for local development or deployment.

**Key Features**:
- Verifies Rust code compiles with `cargo check` before building Docker images
- Supports single-platform builds for local development
- Supports multi-platform builds (amd64 + arm64) for deployment
- Can push images to container registries

**Common Commands** (executed by the agent):
- `cargo make docker-build-all` - Build both images for local platform
- `cargo make docker-build-all-cross` - Build both images for amd64 and arm64
- `cargo make docker-build-all-push` - Build and push multi-platform images

**Example Invocations**:
```
"Build the Docker images"
"Rebuild the images after my code changes"
"Build and push multi-platform images to the registry"
```

**Reference**: See `Makefile.docker.toml` for all available Docker build tasks.

---

### gateway-conformance-runner

**Purpose**: Run the official Kubernetes Gateway API conformance test suite.

**Usage**: Invoke when you need to verify the gateway implementation meets conformance standards.

**Key Features**:
- Sets up Kind cluster for testing
- Builds and deploys the gateway controller
- Runs the official conformance test suite
- Retrieves and analyzes test results

**Common Commands** (executed by the agent):
- `cargo make conformance` - Full conformance test workflow
- `cargo make conformance-build` - Build conformance test image
- `cargo make conformance-logs` - View test results

**Example Invocations**:
```
"Run the conformance tests"
"Check if the gateway passes conformance after my changes"
"Show me the conformance test logs"
```

**Reference**: See `conformance/` directory and conformance tasks in `Makefile.toml`.

---

## Makefile References

The agents directory includes symlinks to relevant Makefiles for easy reference:

- `Makefile.docker.toml` → Used by docker-builder agent
- `Makefile.kind.toml` → Used by gateway-conformance-runner agent

These symlinks point to the canonical Makefiles in the project root, ensuring agents always use the latest task definitions.

## Creating New Agents

To create a new agent:

1. Create a new `.md` file in this directory following the naming pattern: `agent-name.md`
2. Use the YAML frontmatter format:
   ```yaml
   ---
   name: agent-name
   description: Brief description with usage examples
   model: haiku  # or sonnet/opus
   color: blue   # or other color
   ---
   ```
3. Write detailed instructions for the agent's behavior
4. Add reference documentation and symlinks to relevant files if needed
5. Update this README with the new agent's information

## Testing Agents

To test an agent, invoke it using the Task tool:

```
Use the Task tool with subagent_type="docker-builder" to build the Docker images.
```

The agent will receive its instructions from the markdown file and execute the workflow accordingly.
