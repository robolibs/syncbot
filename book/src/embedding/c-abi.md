# C ABI

When the caller lives in the same process — or in another language that can link
a shared library — syncbot exposes a header-free **C ABI**. No network, no
serialisation framework: opaque handles for stateful types, JSON strings for
everything else.

- **Build artifact:** `cdylib` → `libsyncbot.so` / `.dylib` / `.dll`
- **Module:** `src/ffi.rs`
- **Pattern:** opaque handle pointers + JSON `char *`

## Conventions

- Every `char *` returned by the library must be freed with `sb_string_free`.
- On error a function returns `NULL` / `-1`; `sb_last_error()` describes the cause
  (thread-local).
- Stateful types (`ClaimManager`, `Coordinator`, `WorkspaceIndex`, `Workspace`)
  are opaque handles created/destroyed with `_new` / `_free`.

## Minimal example

```c
#include <stdio.h>

extern const char *sb_version(void);
extern const char *sb_last_error(void);
extern void sb_string_free(char *s);

typedef struct SbClaimManager SbClaimManager;
extern SbClaimManager *sb_claim_manager_new(void);
extern void sb_claim_manager_free(SbClaimManager *);
extern int  sb_claim_manager_add_request(SbClaimManager *, const char *json);
extern char *sb_claim_manager_evaluate(const SbClaimManager *, const char *json);

int main(void) {
    printf("syncbot %s\n", sb_version());

    SbClaimManager *m = sb_claim_manager_new();
    const char *req =
        "{\"id\":1,\"robot_id\":1,\"mission_id\":0,"
        "\"access_mode\":\"Exclusive\",\"priority\":0,"
        "\"window\":{\"start_tick\":null,\"end_tick\":null},"
        "\"targets\":[{\"kind\":\"Zone\","
        "\"resource_id\":\"00000000-0000-0000-0000-000000000001\"}]}";
    sb_claim_manager_add_request(m, req);
    char *eval = sb_claim_manager_evaluate(m, req);
    printf("eval: %s\n", eval);
    sb_string_free(eval);
    sb_claim_manager_free(m);
    return 0;
}
```

A complete demo with a `Makefile` lives in `examples/c_abi/`:

```sh
cd examples/c_abi && make run
```

## Surface (selected)

```text
sb_version(), sb_last_error(), sb_string_free(*)

# Workspace + index
sb_workspace_load(path) -> *SbWorkspace
sb_workspace_index_new(*) -> *SbWorkspaceIndex
sb_workspace_index_validation_issues(*) -> json
sb_workspace_index_root_zone_id(*) -> uuid string

# Route planning
sb_plan_route(idx, start_uuid, goal_uuid, use_penalties) -> json

# ClaimManager
sb_claim_manager_new() / _with_index(idx) / _free(*)
sb_claim_manager_add_request / _remove_request
sb_claim_manager_add_lease / _release_lease / _expire_leases
sb_claim_manager_refresh_lease / _revoke_lease
sb_claim_manager_evaluate(*, request_json) -> json
sb_claim_manager_requests / _leases -> json

# Coordinator
sb_coordinator_new() / _with_index(idx) / _free(*)
sb_coordinator_register_robot / _unregister_robot
sb_coordinator_assign_route_plan
sb_coordinator_schedule_robot_route -> json (ScheduleDecision)
sb_coordinator_handle_missed_schedule_slot
sb_coordinator_robot_state(robot_id) -> json

# Helpers
sb_arbitrate_right_of_way(*ctx) -> 0/1/2
sb_parse_zone_policy(props_json) -> json
sb_validate_zone_traffic / _validate_edge_traffic
```

The JSON shapes are the same core types the [wire transports](../wire/overview.md)
use. Unlike the wire `*Wire` types, the C ABI takes core JSON directly — resource
IDs are UUIDs here.
