//! Opt-in state persistence: snapshot the live [`Coordinator`] to a JSON file
//! and restore it on boot.
//!
//! This module is entirely OPT-IN. Nothing here runs unless a caller invokes
//! it (the servers only do so when `SYNCBOT_STATE` is set). With the feature
//! unused the server stays fully in-memory and no file is ever touched.
//!
//! # The snapshot holds secrets
//!
//! Every robot's auth key round-trips through this file in plaintext, because
//! that is how [`Key`] compares them today (see `src/core/key.rs`; hashing is
//! deferred to the sibling `keylock` crate). Anyone who can read the state
//! file can impersonate every robot in it. The file is therefore created
//! `0600`, and it belongs on the same trust footing as a private key — not in
//! a world-readable directory, a backup that travels, or a container image.
//!
//! The [`WorkspaceIndex`] is deliberately NOT part of a snapshot — the
//! workspace is loaded separately at startup and re-attached on
//! [`Coordinator::restore`]. `ClaimManager`'s monotonic `next_id`
//! (`AtomicU64`) is captured as a plain `u64` and rebuilt into an atomic on
//! load so minted ids keep increasing across a restart.

use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::claim::{ClaimRequest, Lease};
use crate::coordinator::{AliveInfo, Coordinator};
use crate::core::ids::RobotId;
use crate::core::key::Key;

/// Serializable image of a [`ClaimManager`](crate::claim::ClaimManager).
///
/// Holds only the owned data — the `index` is never serialized and is
/// re-attached on restore. `next_id` is the current value of the manager's
/// monotonic `AtomicU64` counter, stored as a plain `u64`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaimManagerSnapshot {
    pub active_requests: Vec<ClaimRequest>,
    pub active_leases: Vec<Lease>,
    pub released_leases: Vec<Lease>,
    pub next_id: u64,
}

/// Serializable image of a [`Coordinator`] plus its embedded claim manager.
///
/// Round-trips every piece of state a restart would otherwise lose:
/// registrations, auth keys, the UUID→id map, the synthetic-id counter,
/// heartbeat liveness, and the full claim/lease ledger. The transient
/// `pending_uuid_bindings` is intentionally excluded (it only matters between
/// mint and register within a single process).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoordinatorSnapshot {
    pub robot_states: Vec<crate::robot::RobotState>,
    pub robot_keys: BTreeMap<RobotId, Key>,
    pub robot_id_by_uuid: BTreeMap<String, RobotId>,
    pub next_synthetic_robot_id: u64,
    pub robot_alive: BTreeMap<RobotId, AliveInfo>,
    pub claims: ClaimManagerSnapshot,
}

/// Load a snapshot from `path`. Returns `Ok(None)` if the file does not exist
/// (a fresh boot), `Err` only on an I/O error or a parse failure.
pub fn load(path: &Path) -> io::Result<Option<CoordinatorSnapshot>> {
    match fs::read(path) {
        Ok(bytes) => {
            let snapshot = serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(snapshot))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Atomically write `snapshot` to `path`: serialize to a temp file in the same
/// directory, then `fs::rename` it over the target so a reader never observes a
/// half-written file. The temp file is removed by the rename; nothing is left
/// behind on success.
pub fn write_atomic(path: &Path, snapshot: &CoordinatorSnapshot) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(snapshot)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state path has no file name")
    })?;

    // Temp name in the SAME directory (so rename is atomic on the same
    // filesystem), tagged with the pid to avoid clashes between processes.
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(file_name);
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp = dir.join(tmp_name);

    fs::write(&tmp, &json)?;
    // The snapshot carries every robot's key in plaintext, so narrow the
    // permissions before it is visible under its final name.
    restrict_permissions(&tmp)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup so a failed rename does not litter temps.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Restrict a state file to its owner. A no-op on platforms without Unix
/// permission bits, where the caller owns the directory's access control.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Snapshot the coordinator behind a shared lock and atomically write it to
/// `path`. Convenience for the periodic flush task and the shutdown flush; the
/// lock is released before the (blocking) file write. A poisoned lock is
/// recovered defensively so a single panicking request can never wedge flushes.
pub fn flush(coordinator: &Arc<RwLock<Coordinator>>, path: &Path) -> io::Result<()> {
    let snapshot = {
        let guard = coordinator
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.snapshot()
    };
    write_atomic(path, &snapshot)
}
