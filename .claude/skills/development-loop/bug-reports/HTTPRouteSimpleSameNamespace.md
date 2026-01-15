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

### Issue 3: ConfigMap Server-Side Apply Conflict (IN PROGRESS)

**Failure Message:**
```
Failed to execute upsert error=Kubernetes API error: ApiError: Apply failed with 1 conflict: conflict with "unknown" using v1: .data.config.json
```

**Root Cause Analysis:**
The controller uses Server-Side Apply (SSA) to update ConfigMaps containing data plane configuration. There's a conflict with an "unknown" field manager on the `.data.config.json` field.

**Investigation Findings:**

1. **Original Code Issue:** The upsert functions in `executor.rs` used a "get-then-create" pattern:
   - If resource exists: Update with SSA using field manager "multiway-controller"
   - If resource doesn't exist: Create with `api.create()` using `PostParams::default()` (no field manager = "unknown")

   This meant ConfigMaps created by our controller were tagged with field manager "unknown", causing conflicts on subsequent SSA updates.

2. **Attempted Fix:** Changed all upsert functions to use pure SSA with `PatchParams::apply("multiway-controller").force()`:
   - SSA handles both creation and updates idempotently
   - The `.force()` flag should force the controller to take ownership of conflicting fields

3. **Current Status:** Even with `.force()` enabled, the SSA conflict persists:
   ```
   Apply failed with 1 conflict: conflict with "unknown" using v1: .data.config.json
   ```

**Remaining Questions:**
1. **Is `.force()` being properly passed to the API server?** The kube-rs `PatchParams::apply().force()` should set `force: true` in the patch request, but this needs verification.

2. **Source of "unknown" field manager:** Something is creating/modifying ConfigMaps with field manager "unknown" before our SSA update runs. Possible sources:
   - Another controller or admission webhook
   - Race condition between Gateway and HTTPRoute reconciliation
   - The conformance test setup process

3. **kube-rs SSA behavior:** According to Kubernetes docs, SSA with force should always succeed by taking over field ownership. If `.force()` isn't working as expected, there may be:
   - A bug in kube-rs's SSA implementation
   - Missing TypeMeta (apiVersion/kind) in the ConfigMap object being applied
   - Serialization issues with the patch request

**Proposed Next Steps:**
1. Add debug logging to capture the exact HTTP request being sent for SSA patches
2. Verify the ConfigMap object has proper TypeMeta fields set for SSA
3. Check if the kube-rs `PatchParams::force()` method is correctly setting the force flag
4. Test with `kubectl apply --server-side --force-conflicts` to confirm the API server accepts force apply

**Files Modified (for this issue):**
- `crates/controlplane/src/shell/executor.rs`:
  - Removed `PostParams` import (no longer using `api.create()`)
  - Changed all `upsert_*` functions to use pure SSA with `.force()`
  - Lines 178-197: `upsert_deployment` now uses SSA
  - Lines 199-214: `upsert_service` now uses SSA
  - Lines 216-231: `upsert_configmap` now uses SSA with force
  - Lines 233-248: `upsert_serviceaccount` now uses SSA

## Test Status

The status condition fixes are correct and working:
- Gateway listener ResolvedRefs condition: PASSING
- HTTPRoute observedGeneration on conditions: PASSING

The HTTP request test fails due to the ConfigMap SSA conflict:
- Simple HTTP request should reach infra-backend: FAILING (timeout due to ConfigMap not being updated)

## Files Modified

1. `crates/controlplane/src/core/reconcile.rs`:
   - Lines 288-334: Added ResolvedRefs and Programmed conditions to listener status
   - Lines 639-646: Pass route generation to build_parent_status
   - Lines 672-713: Updated build_parent_status to accept and use generation parameter
   - Lines 385-405: Updated build_configmap to explicitly set all fields

2. `crates/controlplane/src/shell/executor.rs`:
   - Lines 178-248: Changed all upsert functions from get-then-create to pure SSA with force
