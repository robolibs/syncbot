#ifndef SYNCBOT_H
#define SYNCBOT_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define OK 0

#define MISMATCHED_KEY 1

#define ALREADY_REGISTERED 2

#define BAD_ID 3

#define UNSUPPORTED_KEY 4

#define NOT_REGISTERED 2

#define CONFLICT 2

#define CAPACITY 3

#define UNKNOWN_RESOURCE 4

#define BAD_REQUEST 5

#define NO_SUCH_LEASE 2

#define UNKNOWN_OR_BAD 3

typedef struct SbClaimManager SbClaimManager;

typedef struct SbCoordinator SbCoordinator;

typedef struct SbWorkspace SbWorkspace;

typedef struct SbWorkspaceIndex SbWorkspaceIndex;

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
} SbArbitrationContext;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Returns a pointer to the last error message, or NULL if there is none.
 * The pointer is valid until the next FFI call on this thread.
 */
const char *sb_last_error(void);

/**
 * Crate version, as a static NUL-terminated string.
 */
const char *sb_version(void);

/**
 * Free a C string returned by any `sb_*` function that documents it.
 */
void sb_string_free(char *s);

int sb_parse_traffic_bool(const char *value);

int sb_parse_traffic_u64(const char *value, uint64_t *out);

int sb_parse_traffic_f64(const char *value, double *out);

/**
 * Parse a zone-policy `properties` object (JSON map of strings) and return
 * the parsed `ZonePolicy` as a JSON string. Caller frees with
 * `sb_string_free`.
 */
char *sb_parse_zone_policy(const char *properties_json);

char *sb_validate_zone_traffic(const char *properties_json);

char *sb_validate_edge_traffic(const char *properties_json);

/**
 * Load a workspace from a directory path on disk.
 * Returns NULL on error.
 */
SbWorkspace *sb_workspace_load(const char *path);

/**
 * Free a workspace handle.
 */
void sb_workspace_free(SbWorkspace *ws);

/**
 * Build a `WorkspaceIndex` from a workspace handle. The index keeps a
 * shared reference to the workspace via Arc — the workspace handle remains
 * valid to free after this call.
 */
SbWorkspaceIndex *sb_workspace_index_new(const SbWorkspace *ws);

void sb_workspace_index_free(SbWorkspaceIndex *idx);

int sb_workspace_index_refresh(SbWorkspaceIndex *idx);

/**
 * Returns the JSON list of `ValidationIssue`s. Caller frees with
 * `sb_string_free`.
 */
char *sb_workspace_index_validation_issues(const SbWorkspaceIndex *idx);

int sb_workspace_index_is_valid(const SbWorkspaceIndex *idx);

/**
 * Returns the root zone UUID (as a hyphenated string), or NULL on error.
 * Caller frees with `sb_string_free`.
 */
char *sb_workspace_index_root_zone_id(const SbWorkspaceIndex *idx);

/**
 * Plan a route between two node UUIDs. Returns a JSON-encoded
 * `RoutePlanningResult` (which has `search`, `plan`, `failure` fields).
 * Caller frees with `sb_string_free`.
 */
char *sb_plan_route(const SbWorkspaceIndex *idx,
                    const char *start_node_id,
                    const char *goal_node_id,
                    int use_penalties);

SbClaimManager *sb_claim_manager_new(void);

SbClaimManager *sb_claim_manager_with_index(const SbWorkspaceIndex *idx);

void sb_claim_manager_free(SbClaimManager *mgr);

uint64_t sb_claim_manager_request_count(const SbClaimManager *mgr);

uint64_t sb_claim_manager_lease_count(const SbClaimManager *mgr);

/**
 * Add a claim request (JSON-encoded `ClaimRequest`).
 */
int sb_claim_manager_add_request(SbClaimManager *mgr, const char *request_json);

int sb_claim_manager_remove_request(SbClaimManager *mgr, uint64_t claim_id);

/**
 * Add a lease (JSON-encoded `Lease`).
 */
int sb_claim_manager_add_lease(SbClaimManager *mgr, const char *lease_json);

int sb_claim_manager_release_lease(SbClaimManager *mgr,
                                   uint64_t lease_id,
                                   int64_t released_at_tick_or_neg1);

uint64_t sb_claim_manager_expire_leases(SbClaimManager *mgr, uint64_t current_tick);

int sb_claim_manager_refresh_lease(SbClaimManager *mgr,
                                   uint64_t lease_id,
                                   uint64_t refreshed_at_tick,
                                   int64_t expires_at_tick_or_neg1);

int sb_claim_manager_revoke_lease(SbClaimManager *mgr,
                                  uint64_t lease_id,
                                  const char *reason,
                                  uint64_t revoked_at_tick);

/**
 * Evaluate a request. Returns JSON-encoded `ClaimEvaluation`.
 */
char *sb_claim_manager_evaluate(const SbClaimManager *mgr, const char *request_json);

/**
 * Returns JSON list of all active requests.
 */
char *sb_claim_manager_requests(const SbClaimManager *mgr);

/**
 * Returns JSON list of all active leases.
 */
char *sb_claim_manager_leases(const SbClaimManager *mgr);

SbCoordinator *sb_coordinator_new(void);

SbCoordinator *sb_coordinator_with_index(const SbWorkspaceIndex *idx);

void sb_coordinator_free(SbCoordinator *c);

uint64_t sb_coordinator_robot_count(const SbCoordinator *c);

/**
 * Register a robot from a JSON-encoded `RobotState`.
 */
int sb_coordinator_register_robot(SbCoordinator *c, const char *state_json);

int sb_coordinator_unregister_robot(SbCoordinator *c, uint64_t robot_id);

/**
 * JSON-encoded `RobotState` for the given robot, or NULL if unknown.
 */
char *sb_coordinator_robot_state(const SbCoordinator *c, uint64_t robot_id);

int sb_coordinator_assign_route_plan(SbCoordinator *c,
                                     uint64_t robot_id,
                                     const char *route_plan_json,
                                     uint64_t horizon,
                                     uint64_t updated_at_tick);

/**
 * Schedule a robot's route. Returns JSON-encoded `ScheduleDecision`.
 */
char *sb_coordinator_schedule_robot_route(SbCoordinator *c,
                                          uint64_t robot_id,
                                          uint64_t claim_id,
                                          uint64_t start_tick,
                                          double ticks_per_cost_unit,
                                          int access_mode_shared);

int sb_coordinator_handle_missed_schedule_slot(SbCoordinator *c,
                                               uint64_t robot_id,
                                               uint64_t current_tick,
                                               uint64_t grace_ticks);

int sb_arbitrate_right_of_way(const SbArbitrationContext *ctx);

/**
 * Map a JSON-encoded `RoutePlan` to a JSON-encoded VDA `Order`.
 */
char *sb_vda_order_from_route(const char *route_plan_json);

/**
 * Map a JSON-encoded `RobotState` to a JSON-encoded VDA `State`.
 */
char *sb_vda_state_from_robot(const char *robot_state_json);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* SYNCBOT_H */
