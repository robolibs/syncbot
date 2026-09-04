//! Per-robot navigation state. Port of `include/syncbot/robot_state.hpp`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claim::{ClaimId, LeaseId};
use crate::core::ids::{MissionId, RobotId};
use crate::route::RoutePlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RobotProgressState {
    Idle,
    FollowingRoute,
    Waiting,
    Queued,
    Blocked,
    Replanning,
}

impl Default for RobotProgressState {
    fn default() -> Self {
        Self::Idle
    }
}

/// Which frame a robot reported its position in.
///
/// Both forms below describe the same point; the other is derived from the
/// workspace datum. This records which one actually came off the wire, so a
/// reader can tell a measurement from a conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionFrame {
    /// Latitude, longitude, altitude (WGS84).
    Global,
    /// x/y/z in metres east/north/up of the datum.
    Local,
}

/// Where a robot says it is.
///
/// Carries both frames whenever a datum is bound, because the audiences differ:
/// a map wants lat/lon, and anything reasoning about robots relative to each
/// other wants metres. Converting once, here, beats every consumer
/// reimplementing it against the datum and getting it subtly wrong.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RobotPosition {
    /// The frame the robot actually sent; the other side is derived.
    pub reported: PositionFrame,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    /// Metres east of the datum.
    pub x: f64,
    /// Metres north of the datum.
    pub y: f64,
    /// Metres up from the datum.
    pub z: f64,
    /// False when no workspace — and so no datum — was bound when this
    /// arrived, leaving only the reported frame meaningful; the other is zeroed
    /// rather than measured.
    pub converted: bool,
}

impl RobotPosition {
    /// From metres east/north/up of the datum, deriving lat/lon.
    ///
    /// With no index there is nothing to anchor against, so the derived frame
    /// is zeroed behind `converted: false` — which says "unknown", where a
    /// bare zero would claim the robot is sitting on the origin.
    pub fn from_local(
        x: f64,
        y: f64,
        z: f64,
        index: Option<&crate::index::WorkspaceIndex>,
    ) -> Self {
        let global =
            index.and_then(|index| index.pose_local_to_global(datapod::Point::new(x, y, z)));
        Self {
            reported: PositionFrame::Local,
            lat: global.map_or(0.0, |g| g.latitude),
            lon: global.map_or(0.0, |g| g.longitude),
            alt: global.map_or(0.0, |g| g.altitude),
            x,
            y,
            z,
            converted: global.is_some(),
        }
    }

    /// From WGS84, deriving metres off the datum. See [`from_local`].
    ///
    /// [`from_local`]: RobotPosition::from_local
    pub fn from_global(
        lat: f64,
        lon: f64,
        alt: f64,
        index: Option<&crate::index::WorkspaceIndex>,
    ) -> Self {
        let local =
            index.and_then(|index| index.pose_global_to_local(datapod::Geo::new(lat, lon, alt)));
        Self {
            reported: PositionFrame::Global,
            lat,
            lon,
            alt,
            x: local.map_or(0.0, |p| p.x),
            y: local.map_or(0.0, |p| p.y),
            z: local.map_or(0.0, |p| p.z),
            converted: local.is_some(),
        }
    }
}

/// Which way a robot is pointing.
///
/// Robots send ROS REP-103 yaw, so that is stored verbatim; the compass bearing
/// is derived, because "relative to true north" is what a person reads off a
/// map and the conversion is easy to get backwards.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RobotHeading {
    /// Radians counter-clockwise from east, as sent (REP-103 yaw in ENU).
    pub yaw_rad: f64,
    /// Degrees clockwise from true north, in `[0, 360)`. Derived.
    pub bearing_deg: f64,
}

impl RobotHeading {
    /// ENU yaw → compass bearing.
    ///
    /// REP-103 measures counter-clockwise from east; a bearing measures
    /// clockwise from north. So it is `90 - yaw`, wrapped — not a sign flip.
    pub fn from_yaw_rad(yaw_rad: f64) -> Self {
        Self {
            yaw_rad,
            bearing_deg: (90.0 - yaw_rad.to_degrees()).rem_euclid(360.0),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RobotState {
    pub robot_id: RobotId,
    pub mission_id: MissionId,
    pub current_node_id: Option<Uuid>,
    pub current_edge_id: Option<Uuid>,
    pub route_plan: Option<RoutePlan>,
    pub pending_claim_ids: Vec<ClaimId>,
    pub active_lease_ids: Vec<LeaseId>,
    pub progress_state: RobotProgressState,
    pub next_route_step_index: u64,
    pub hold_reason: Option<String>,
    pub last_claim_tick: Option<u64>,
    pub scheduled_start_tick: Option<u64>,
    pub reserved_until_tick: Option<u64>,
    pub wait_ticks: u64,
    pub needs_replan: bool,
    pub horizon: u64,
    pub updated_at_tick: u64,
    /// Last reported position, if the robot sends one. Optional by design: a
    /// robot that only heartbeats still coordinates fine, it just cannot be
    /// drawn.
    pub position: Option<RobotPosition>,
    /// Last reported heading, if the robot sends one. Independent of
    /// `position` — a robot may report either, both, or neither.
    pub heading: Option<RobotHeading>,
    /// Wall clock of the last position or heading report, so a reader can tell
    /// a live pose from one left over by a robot that has since gone quiet.
    pub pose_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REP-103 yaw is counter-clockwise from east; a bearing is clockwise from
    /// north. Getting that backwards points every robot the wrong way, and the
    /// error is invisible at 45°, so the cardinals are pinned here.
    #[test]
    fn yaw_becomes_a_compass_bearing() {
        let cases = [
            (0.0, 90.0),                           // east
            (std::f64::consts::FRAC_PI_2, 0.0),    // north
            (std::f64::consts::PI, 270.0),         // west
            (-std::f64::consts::FRAC_PI_2, 180.0), // south
            (std::f64::consts::FRAC_PI_4, 45.0),   // north-east
        ];
        for (yaw_rad, expected) in cases {
            let heading = RobotHeading::from_yaw_rad(yaw_rad);
            assert!(
                (heading.bearing_deg - expected).abs() < 1e-9,
                "yaw {yaw_rad} should bear {expected}, got {}",
                heading.bearing_deg
            );
            assert_eq!(heading.yaw_rad, yaw_rad, "the reported yaw is kept as sent");
        }
    }

    #[test]
    fn a_bearing_always_lands_in_zero_to_360() {
        for yaw_deg in [-720.0, -180.0, 0.0, 180.0, 540.0, 1080.0] {
            let bearing = RobotHeading::from_yaw_rad(f64::to_radians(yaw_deg)).bearing_deg;
            assert!(
                (0.0..360.0).contains(&bearing),
                "yaw {yaw_deg}° gave a bearing of {bearing}"
            );
        }
    }
}
