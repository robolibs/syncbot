#ifndef TIMENAV_H
#define TIMENAV_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct TnClaimManager TnClaimManager;

typedef struct TnCoordinator TnCoordinator;

typedef struct TnWorkspace TnWorkspace;

typedef struct TnWorkspaceIndex TnWorkspaceIndex;

typedef struct {
  double self_priority;
  double other_priority;
  uint8_t self_holds_lease;
  uint8_t other_holds_lease;
  uint8_t self_is_emergency;
  uint8_t other_is_emergency;
  uint8_t self_state;
  uint8_t other_state;
  uint64_t self_wait_ticks;
  uint64_t other_wait_ticks;
  uint64_t self_remaining_steps;
  uint64_t other_remaining_steps;
} TnArbitrationContext;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Returns a pointer to the last error message, or NULL if there is none.
 * The pointer is valid until the next FFI call on this thread.
 */
const char *tn_last_error(void);

/**
 * Crate version, as a static NUL-terminated string.
 */
const char *tn_version(void);

/**
 * Free a C string returned by any `tn_*` function that documents it.
 */
void tn_string_free(char *s);

int tn_parse_traffic_bool(const char *value);

int tn_parse_traffic_u64(const char *value, uint64_t *out);

int tn_parse_traffic_f64(const char *value, double *out);

/**
 * Parse a zone-policy `properties` object (JSON map of strings) and return
 * the parsed `ZonePolicy` as a JSON string. Caller frees with
 * `tn_string_free`.
 */
char *tn_parse_zone_policy(const char *properties_json);

char *tn_validate_zone_traffic(const char *properties_json);

char *tn_validate_edge_traffic(const char *properties_json);

/**
 * Load a workspace from a directory path on disk.
 * Returns NULL on error.
 */
TnWorkspace *tn_workspace_load(const char *path);

/**
 * Free a workspace handle.
 */
void tn_workspace_free(TnWorkspace *ws);

/**
 * Build a `WorkspaceIndex` from a workspace handle. The index keeps a
 * shared reference to the workspace via Arc — the workspace handle remains
 * valid to free after this call.
 */
TnWorkspaceIndex *tn_workspace_index_new(const TnWorkspace *ws);

void tn_workspace_index_free(TnWorkspaceIndex *idx);

int tn_workspace_index_refresh(TnWorkspaceIndex *idx);

/**
 * Returns the JSON list of `ValidationIssue`s. Caller frees with
 * `tn_string_free`.
 */
char *tn_workspace_index_validation_issues(const TnWorkspaceIndex *idx);

int tn_workspace_index_is_valid(const TnWorkspaceIndex *idx);

/**
 * Returns the root zone UUID (as a hyphenated string), or NULL on error.
 * Caller frees with `tn_string_free`.
 */
char *tn_workspace_index_root_zone_id(const TnWorkspaceIndex *idx);

/**
 * Plan a route between two node UUIDs. Returns a JSON-encoded
 * `RoutePlanningResult` (which has `search`, `plan`, `failure` fields).
 * Caller frees with `tn_string_free`.
 */
char *tn_plan_route(const TnWorkspaceIndex *idx,
                    const char *start_node_id,
                    const char *goal_node_id,
                    int use_penalties);

TnClaimManager *tn_claim_manager_new(void);

TnClaimManager *tn_claim_manager_with_index(const TnWorkspaceIndex *idx);

void tn_claim_manager_free(TnClaimManager *mgr);

uint64_t tn_claim_manager_request_count(const TnClaimManager *mgr);

uint64_t tn_claim_manager_lease_count(const TnClaimManager *mgr);

/**
 * Add a claim request (JSON-encoded `ClaimRequest`).
 */
int tn_claim_manager_add_request(TnClaimManager *mgr, const char *request_json);

int tn_claim_manager_remove_request(TnClaimManager *mgr, uint64_t claim_id);

/**
 * Add a lease (JSON-encoded `Lease`).
 */
int tn_claim_manager_add_lease(TnClaimManager *mgr, const char *lease_json);

int tn_claim_manager_release_lease(TnClaimManager *mgr,
                                   uint64_t lease_id,
                                   int64_t released_at_tick_or_neg1);

uint64_t tn_claim_manager_expire_leases(TnClaimManager *mgr, uint64_t current_tick);

int tn_claim_manager_refresh_lease(TnClaimManager *mgr,
                                   uint64_t lease_id,
                                   uint64_t refreshed_at_tick,
                                   int64_t expires_at_tick_or_neg1);

int tn_claim_manager_revoke_lease(TnClaimManager *mgr,
                                  uint64_t lease_id,
                                  const char *reason,
                                  uint64_t revoked_at_tick);

/**
 * Evaluate a request. Returns JSON-encoded `ClaimEvaluation`.
 */
char *tn_claim_manager_evaluate(const TnClaimManager *mgr, const char *request_json);

/**
 * Returns JSON list of all active requests.
 */
char *tn_claim_manager_requests(const TnClaimManager *mgr);

/**
 * Returns JSON list of all active leases.
 */
char *tn_claim_manager_leases(const TnClaimManager *mgr);

TnCoordinator *tn_coordinator_new(void);

TnCoordinator *tn_coordinator_with_index(const TnWorkspaceIndex *idx);

void tn_coordinator_free(TnCoordinator *c);

uint64_t tn_coordinator_robot_count(const TnCoordinator *c);

/**
 * Register a robot from a JSON-encoded `RobotState`.
 */
int tn_coordinator_register_robot(TnCoordinator *c, const char *state_json);

int tn_coordinator_unregister_robot(TnCoordinator *c, uint64_t robot_id);

/**
 * JSON-encoded `RobotState` for the given robot, or NULL if unknown.
 */
char *tn_coordinator_robot_state(const TnCoordinator *c, uint64_t robot_id);

int tn_coordinator_assign_route_plan(TnCoordinator *c,
                                     uint64_t robot_id,
                                     const char *route_plan_json,
                                     uint64_t horizon,
                                     uint64_t updated_at_tick);

/**
 * Schedule a robot's route. Returns JSON-encoded `ScheduleDecision`.
 */
char *tn_coordinator_schedule_robot_route(TnCoordinator *c,
                                          uint64_t robot_id,
                                          uint64_t claim_id,
                                          uint64_t start_tick,
                                          double ticks_per_cost_unit,
                                          int access_mode_shared);

int tn_coordinator_handle_missed_schedule_slot(TnCoordinator *c,
                                               uint64_t robot_id,
                                               uint64_t current_tick,
                                               uint64_t grace_ticks);

int tn_arbitrate_right_of_way(const TnArbitrationContext *ctx);

/**
 * Map a JSON-encoded `RoutePlan` to a JSON-encoded VDA `Order`.
 */
char *tn_vda_order_from_route(const char *route_plan_json);

/**
 * Map a JSON-encoded `RobotState` to a JSON-encoded VDA `State`.
 */
char *tn_vda_state_from_robot(const char *robot_state_json);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* TIMENAV_H */
