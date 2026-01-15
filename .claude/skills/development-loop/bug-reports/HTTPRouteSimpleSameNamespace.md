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

### Issue 4: Data Plane Not Responding to HTTP Requests (IN PROGRESS)

**Failure Message:**
```
Request failed, not ready yet: Get "http://10.96.65.60/": context deadline exceeded
```

**Root Cause Analysis:**
After fixing the ConfigMap SSA conflict, the ConfigMaps are now being created successfully and the data plane pods are running. However, HTTP requests to the Gateway Service IP are timing out.

**Current Status:**
- ConfigMaps: ✅ Created successfully (`multiway-config-same-namespace`, etc.)
- Data plane pods: ✅ Running (`multiway-dp-same-namespace-*`)
- HTTP routing: ❌ Requests timeout

This is a separate issue from the SSA conflict and requires further investigation into:
1. Data plane configuration loading
2. Envoy/Pingora routing setup
3. Service endpoints and networking

## Test Status

The status condition and ConfigMap fixes are working:
- Gateway listener ResolvedRefs condition: ✅ PASSING
- HTTPRoute observedGeneration on conditions: ✅ PASSING
- ConfigMap creation/update (SSA conflict fix): ✅ FIXED

The HTTP request test fails due to data plane routing issues:
- Simple HTTP request should reach infra-backend: ❌ FAILING (timeout - data plane not routing traffic)

## Files Modified

1. `crates/controlplane/src/core/reconcile.rs`:
   - Lines 288-334: Added ResolvedRefs and Programmed conditions to listener status
   - Lines 639-646: Pass route generation to build_parent_status
   - Lines 672-713: Updated build_parent_status to accept and use generation parameter
   - Lines 385-405: Updated build_configmap to explicitly set all fields

2. `crates/controlplane/src/shell/executor.rs`:
   - Lines 216-264: Changed `upsert_configmap` from SSA to create-or-replace pattern
   - This fix avoids SSA field manager conflicts that persisted even with `.force()` enabled
