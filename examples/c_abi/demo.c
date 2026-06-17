/*
 * syncbot C ABI smoke test.
 *
 * Demonstrates the opaque-handle + JSON marshaling pattern across a few of
 * the major surfaces. Build with `make`, run `./demo`.
 */

#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "syncbot.h"

/* --- demo --------------------------------------------------------------- */

int main(void) {
    printf("syncbot version: %s\n", sb_version());

    if (sb_parse_traffic_bool("yes") != 1) {
        fprintf(stderr, "expected yes -> 1\n"); return 1;
    }

    char *policy = sb_parse_zone_policy("{\"traffic.policy\":\"exclusive\"}");
    if (!policy) { fprintf(stderr, "parse_zone_policy failed: %s\n", sb_last_error()); return 1; }
    printf("policy json: %s\n", policy);
    sb_string_free(policy);

    /* claim manager */
    SbClaimManager *mgr = sb_claim_manager_new();
    const char *req =
        "{\"id\":1,\"robot_id\":1,\"mission_id\":0,"
        "\"access_mode\":\"Exclusive\",\"priority\":0,"
        "\"requested_at_tick\":null,"
        "\"window\":{\"start_tick\":null,\"end_tick\":null},"
        "\"targets\":[{\"kind\":\"Zone\",\"resource_id\":\"00000000-0000-0000-0000-000000000001\"}]}";
    if (sb_claim_manager_add_request(mgr, req) != 0) {
        fprintf(stderr, "add_request failed: %s\n", sb_last_error()); return 1;
    }
    printf("claim manager requests: %llu\n",
           (unsigned long long) sb_claim_manager_request_count(mgr));
    char *eval = sb_claim_manager_evaluate(mgr, req);
    printf("evaluation: %s\n", eval);
    sb_string_free(eval);
    sb_claim_manager_free(mgr);

    /* coordinator */
    SbCoordinator *c = sb_coordinator_new();
    const char *state =
        "{\"robot_id\":7,\"mission_id\":0,"
        "\"current_node_id\":null,\"current_edge_id\":null,"
        "\"route_plan\":null,\"pending_claim_ids\":[],\"active_lease_ids\":[],"
        "\"progress_state\":\"Idle\",\"next_route_step_index\":0,"
        "\"hold_reason\":null,\"last_claim_tick\":null,"
        "\"scheduled_start_tick\":null,\"reserved_until_tick\":null,"
        "\"wait_ticks\":0,\"needs_replan\":false,\"horizon\":0,"
        "\"updated_at_tick\":0}";
    if (sb_coordinator_register_robot(c, state) != 0) {
        fprintf(stderr, "register_robot failed: %s\n", sb_last_error()); return 1;
    }
    printf("coordinator robots: %llu\n",
           (unsigned long long) sb_coordinator_robot_count(c));
    sb_coordinator_free(c);

    /* arbitration */
    SbArbitrationContext ctx = {0};
    ctx.self_is_emergency = 1;
    int decision = sb_arbitrate_right_of_way(&ctx);
    printf("arbitration (emergency self): %d (0=proceed, 1=yield, 2=replan)\n", decision);

    printf("\nall checks passed\n");
    return 0;
}
