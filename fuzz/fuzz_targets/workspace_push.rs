//! Fuzz the one entry point that parses untrusted structured data.
//!
//! `set_workspace` takes arbitrary JSON from the network and turns it into a
//! recursive zone tree, then indexes and validates it. Everything downstream
//! of that walk assumes a well-formed workspace; this checks the walk itself
//! survives one that is not. It found the unbounded recursion that
//! `MAX_ZONE_DEPTH` now caps.
//!
//! ```sh
//! cargo +nightly fuzz run workspace_push
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The full pipeline a pushed document goes through, minus the coordinator
    // binding: parse, load, index, validate. A panic or abort here is a fleet
    // outage triggered by one POST.
    let Ok(wire) = serde_json::from_slice::<zoneout::WorkspaceJson>(data) else {
        return;
    };
    let Ok(workspace) = zoneout::Workspace::from_wire(wire) else {
        return;
    };
    let index = syncbot::WorkspaceIndex::from_workspace(workspace);
    let _ = index.max_zone_depth();
    let _ = index.validation_issues();
    if let Some(root) = index.root_zone_id() {
        let _ = index.descendant_zones(root);
    }
});
