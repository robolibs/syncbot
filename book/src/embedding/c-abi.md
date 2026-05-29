# C ABI

When the caller lives in the same process — or in another language that can link
a shared library — timenav exposes a header-free **C ABI**. No network, no
serialisation framework: opaque handles for stateful types, JSON strings for
everything else.

- **Build artifact:** `cdylib` → `libtimenav.so` / `.dylib` / `.dll`
- **Module:** `src/ffi.rs`
- **Pattern:** opaque handle pointers + JSON `char *`

## Conventions

- Every `char *` returned by the library must be freed with `tn_string_free`.
- On error a function returns `NULL` / `-1`; `tn_last_error()` describes the cause
  (thread-local).
- Stateful types (`ClaimManager`, `Coordinator`, `WorkspaceIndex`, `Workspace`)
  are opaque handles created/destroyed with `_new` / `_free`.

## Minimal example

```c
#include <stdio.h>

extern const char *tn_version(void);
extern const char *tn_last_error(void);
extern void tn_string_free(char *s);

typedef struct TnClaimManager TnClaimManager;
extern TnClaimManager *tn_claim_manager_new(void);
extern void tn_claim_manager_free(TnClaimManager *);
extern int  tn_claim_manager_add_request(TnClaimManager *, const char *json);
extern char *tn_claim_manager_evaluate(const TnClaimManager *, const char *json);

int main(void) {
    printf("timenav %s\n", tn_version());

    TnClaimManager *m = tn_claim_manager_new();
    const char *req =
        "{\"id\":1,\"robot_id\":1,\"mission_id\":0,"
        "\"access_mode\":\"Exclusive\",\"priority\":0,"
        "\"window\":{\"start_tick\":null,\"end_tick\":null},"
        "\"targets\":[{\"kind\":\"Zone\","
        "\"resource_id\":\"00000000-0000-0000-0000-000000000001\"}]}";
    tn_claim_manager_add_request(m, req);
    char *eval = tn_claim_manager_evaluate(m, req);
    printf("eval: %s\n", eval);
    tn_string_free(eval);
    tn_claim_manager_free(m);
    return 0;
}
```

A complete demo with a `Makefile` lives in `examples/c_abi/`:

```sh
cd examples/c_abi && make run
```

## Surface (selected)

```text
tn_version(), tn_last_error(), tn_string_free(*)

# Workspace + index
tn_workspace_load(path) -> *TnWorkspace
tn_workspace_index_new(*) -> *TnWorkspaceIndex
tn_workspace_index_validation_issues(*) -> json
tn_workspace_index_root_zone_id(*) -> uuid string

# Route planning
tn_plan_route(idx, start_uuid, goal_uuid, use_penalties) -> json

# ClaimManager
tn_claim_manager_new() / _with_index(idx) / _free(*)
tn_claim_manager_add_request / _remove_request
tn_claim_manager_add_lease / _release_lease / _expire_leases
tn_claim_manager_refresh_lease / _revoke_lease
tn_claim_manager_evaluate(*, request_json) -> json
tn_claim_manager_requests / _leases -> json

# Coordinator
tn_coordinator_new() / _with_index(idx) / _free(*)
tn_coordinator_register_robot / _unregister_robot
tn_coordinator_assign_route_plan
tn_coordinator_schedule_robot_route -> json (ScheduleDecision)
tn_coordinator_handle_missed_schedule_slot
tn_coordinator_robot_state(robot_id) -> json

# Helpers
tn_arbitrate_right_of_way(*ctx) -> 0/1/2
tn_parse_zone_policy(props_json) -> json
tn_validate_zone_traffic / _validate_edge_traffic
```

The JSON shapes are the same core types the [wire transports](../wire/overview.md)
use. Unlike the wire `*Wire` types, the C ABI takes core JSON directly — resource
IDs are UUIDs here.
