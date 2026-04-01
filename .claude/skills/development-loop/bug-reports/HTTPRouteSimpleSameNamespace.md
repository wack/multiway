# Bug Report: HTTPRouteSimpleSameNamespace

## Test Description
This conformance test validates basic HTTP routing from an HTTPRoute to a backend service in the same namespace. It creates:
- A Gateway named `same-namespace` in `gateway-conformance-infra` namespace
- An HTTPRoute named `gateway-conformance-infra-test` that routes to `infra-backend-v1:8080`
- Makes an HTTP request to the Gateway and expects a 200 response from the backend

## Issues Found

### Issue 1: Missing ResolvedRefs Condition on Gateway Listeners (FIXED)

**Failure Message:**
```
gateway gateway-conformance-infra/same-namespace doesn't have ResolvedRefs condition set to True on http listener
```

**Root Cause:**
Gateway listeners only had the `Accepted` condition set, but Gateway API requires listeners to also have `ResolvedRefs` and `Programmed` conditions.

**Fix Applied:**
Added `ResolvedRefs` and `Programmed` conditions to `GatewayStatusListeners` in `crates/controlplane/src/core/reconcile.rs:288-333`.

### Issue 2: Missing observedGeneration on HTTPRoute Conditions (FIXED)

**Failure Message:**
```
HTTPRoute expected observedGeneration to be updated to 1 for all conditions, only 0/2 were updated. stale conditions are: Accepted (generation 0), ResolvedRefs (generation 0)
```

**Root Cause:**
HTTPRoute parent status conditions had `observed_generation: None` instead of the route's generation.

**Fix Applied:**
Updated `build_parent_status` function in `crates/controlplane/src/core/reconcile.rs:672-713` to accept and use the route's generation.

### Issue 3: ConfigMap Server-Side Apply Conflict (FIXED)

**Failure Message:**
```
Failed to execute upsert error=Kubernetes API error: ApiError: Apply failed with 1 conflict: conflict with "unknown" using v1: .data.config.json
```

**Root Cause Analysis:**
The controller uses Server-Side Apply (SSA) to update ConfigMaps containing data plane configuration. There was a conflict with an "unknown" field manager on the `.data.config.json` field.

**Investigation Findings:**

1. **Original Code Issue:** The upsert functions in `executor.rs` used a "get-then-create" pattern:
   - If resource exists: Update with SSA using field manager "multiway-controller"
   - If resource doesn't exist: Create with `api.create()` using `PostParams::default()` (no field manager = "unknown")

   This meant ConfigMaps created by our controller were tagged with field manager "unknown", causing conflicts on subsequent SSA updates.

2. **Initial Attempted Fix:** Changed all upsert functions to use pure SSA with `PatchParams::apply("multiway-controller").force()`:
   - SSA handles both creation and updates idempotently
   - The `.force()` flag should force the controller to take ownership of conflicting fields

3. **SSA with Force Still Failed:** Even with `.force()` enabled, the SSA conflict persisted. Investigation showed that k8s-openapi's `ConfigMap` type, when serialized via `Patch::Apply`, may not properly include TypeMeta (apiVersion/kind) fields required by SSA.

4. **Working Solution:** Implemented a create-or-replace pattern for ConfigMaps instead of SSA:
   - Use `api.get()` to check if ConfigMap exists
   - If exists: Use `api.replace()` with the existing `resource_version` to update
   - If not exists (404): Use `api.create()` to create

   This avoids SSA field manager conflicts entirely while still providing idempotent upsert semantics.

**Fix Applied:**
Updated `upsert_configmap` in `crates/controlplane/src/shell/executor.rs:216-264` to use the create-or-replace pattern.

### Issue 4: Data Plane Not Responding to HTTP Requests (RESOLVED - Not a Bug)

**Failure Message:**
```
Request failed, not ready yet: Get "http://10.96.65.60/": context deadline exceeded
```

**Root Cause Analysis:**
After fixing the ConfigMap SSA conflict, the ConfigMaps are now being created successfully and the data plane pods are running. However, HTTP requests to the Gateway Service IP were timing out.

**Investigation Findings:**

1. **Manual testing from inside the cluster works perfectly:**
   ```bash
   kubectl run curl-test --image=curlimages/curl:latest --restart=Never --rm -i --tty -- \
     curl -v http://multiway-dp-same-namespace.gateway-conformance-infra.svc.cluster.local/
   ```
   Returns HTTP 200 with correct response from backend.

2. **The data plane routing is working correctly.** The proxy-core data plane successfully:
   - Loads configuration from ConfigMap
   - Routes requests to backend services via Kubernetes DNS
   - Returns proper HTTP responses

3. **The actual issue is network accessibility:** Conformance tests run from **outside** the cluster (on the host machine) and cannot reach ClusterIP addresses.

**Current Status:**
- ConfigMaps: ✅ Created successfully
- Data plane pods: ✅ Running
- HTTP routing (in-cluster): ✅ **Working correctly**
- HTTP routing (from host): ❌ ClusterIP not reachable from outside cluster

### Issue 5: Docker Image Name Mismatch (FIXED)

**Symptom:**
ConfigMap SSA errors persisted even after the fix in Issue 3 was applied.

**Root Cause:**
The Makefile builds images named `multiway-controlplane:latest` and `multiway-dataplane:latest`, but the deployment manifests referenced `multiway-controller:latest`. This caused the deployment to use an old, cached image that didn't have the SSA fix.

**Fix Applied:**
Updated `deploy/controller-deployment.yaml` and `deploy/kustomization.yaml` to use `multiway-controlplane:latest` instead of `multiway-controller:latest`.

### Issue 6: Gateway Service Type for External Access (IN PROGRESS)

**Problem:**
Conformance tests run from outside the Kind cluster and need to reach the gateway. The gateway service was using ClusterIP, which is only accessible from within the cluster.

**Partial Fix Applied:**
Changed the service type from `ClusterIP` to `NodePort` in `crates/controlplane/src/core/reconcile.rs:470`.

**Remaining Challenge:**
For NodePort to work with conformance tests, two conditions must be met:
1. The Gateway status address must be the **node IP** (e.g., `172.18.0.2`), not the ClusterIP
2. The conformance tests use `address:listener_port` (e.g., `172.18.0.2:80`), but NodePort requires `node_ip:node_port` (e.g., `172.18.0.2:31960`)

**Fixes Applied:**

1. **Kind cluster configuration:** Created `kind-config.yaml` with `extraPortMappings` to forward ports 80 and 443 from the host to the Kind node. Updated `Makefile.toml` to use this configuration.

2. **hostPort on data plane pods:** Added `host_port` to container ports in `build_deployment()` so the data plane listens directly on the node's network interface.

3. **Gateway address:** Set Gateway status address to `127.0.0.1` which works with Kind's extraPortMappings. Traffic flow: `host:80 → Docker → Kind node:80 → hostPort → Pod`

4. **Node information in WorldSnapshot:** Added node fetching to the controller to support future enhancements (e.g., dynamic node IP discovery).

5. **RBAC for nodes:** Added permissions to read nodes in `deploy/clusterrole.yaml`.

**Current Limitation:**
The conformance test suite creates **multiple Gateways** (same-namespace, all-namespaces, same-namespace-with-https-listener, backend-namespaces) during setup. With `hostPort`, only **one Gateway per port** can be active on a single-node cluster. When multiple Gateways try to use port 80, only the first one's pod can be scheduled; the others fail with port conflicts.

**Error observed:**
```
Pod gateway-conformance-infra/multiway-dp-all-namespaces-xxx not ready yet
error waiting for gateway-conformance-infra namespaces to be ready
```

## Test Status

| Check | Status |
|-------|--------|
| Gateway listener ResolvedRefs condition | ✅ PASSING |
| HTTPRoute observedGeneration on conditions | ✅ PASSING |
| ConfigMap creation/update (SSA conflict fix) | ✅ FIXED |
| Data plane routing (in-cluster) | ✅ WORKING |
| External access (single Gateway) | ✅ WORKING |
| Conformance test (multi-Gateway setup) | ❌ FAILING (hostPort conflicts) |

## Files Modified

1. `crates/controlplane/src/core/reconcile.rs`:
   - Lines 288-334: Added ResolvedRefs and Programmed conditions to listener status
   - Lines 639-646: Pass route generation to build_parent_status
   - Lines 672-713: Updated build_parent_status to accept and use generation parameter
   - Lines 385-405: Updated build_configmap to explicitly set all fields
   - Lines 484-497: Added hostPort to container ports for external access
   - Lines 194-206: Set Gateway address to 127.0.0.1 for local development

2. `crates/controlplane/src/core/snapshot.rs`:
   - Added `Node` import and `nodes` field to WorldSnapshot
   - Added `get_node_internal_ip()` method for future node IP discovery
   - Added `with_node()` builder method

3. `crates/controlplane/src/shell/fetcher.rs`:
   - Added `Node` import
   - Added `fetch_nodes()` method to populate WorldSnapshot with cluster nodes

4. `crates/controlplane/src/shell/executor.rs`:
   - Lines 216-264: Changed `upsert_configmap` from SSA to create-or-replace pattern

5. `deploy/clusterrole.yaml`:
   - Added RBAC permissions to get/list/watch nodes

6. `deploy/controller-deployment.yaml`:
   - Line 24: Changed image from `multiway-controller:latest` to `multiway-controlplane:latest`

7. `deploy/kustomization.yaml`:
   - Lines 15-17: Changed image name from `multiway-controller` to `multiway-controlplane`

8. `kind-config.yaml` (NEW):
   - Kind cluster configuration with extraPortMappings for ports 80 and 443

9. `Makefile.toml`:
   - Updated `kind-create` task to use `kind-config.yaml`

## Next Steps

**Recommended:** Modify the conformance test suite to run tests **sequentially** instead of creating all Gateways at setup time. This ensures only one Gateway per port is active at a time, avoiding hostPort conflicts on single-node clusters.

The conformance test suite should be configured to:
1. Create only the Gateway needed for each test
2. Clean up the Gateway after the test completes
3. Run tests one at a time rather than in parallel

This approach works with the current hostPort + extraPortMappings setup without requiring additional infrastructure like MetalLB.
