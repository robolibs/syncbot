//! Zone policy and edge traffic semantics.
//!
//! Port of `include/syncbot/zone_policy.hpp`. Parses, validates, merges, and
//! derives traffic-typed views over the `traffic.*` string properties on
//! zones and edges.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::core::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZonePolicyKind {
    Informational,
    ExclusiveAccess,
    SharedAccess,
    CapacityLimited,
    Corridor,
    Replanning,
    Restricted,
    NoStop,
    Slowdown,
}

impl Default for ZonePolicyKind {
    fn default() -> Self {
        Self::Informational
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrafficIssueSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficParseIssue {
    pub severity: TrafficIssueSeverity,
    pub key: String,
    pub message: String,
}

impl Default for TrafficParseIssue {
    fn default() -> Self {
        Self {
            severity: TrafficIssueSeverity::Error,
            key: String::new(),
            message: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ZonePolicy {
    pub kind: ZonePolicyKind,
    pub capacity: u64,
    pub capacity_is_explicit: bool,
    pub requires_claim: bool,
    pub blocks_traversal_without_grant: bool,
    pub blocks_entry_without_grant: bool,
    pub priority: Option<f64>,
    pub speed_limit: Option<f64>,
    pub waiting_allowed: Option<bool>,
    pub stop_allowed: Option<bool>,
    pub blocked: Option<bool>,
    pub replan_trigger: Option<bool>,
    pub entry_rule: Option<String>,
    pub exit_rule: Option<String>,
    pub robot_class: Option<String>,
    pub schedule_window: Option<String>,
    pub access_group: Option<String>,
    pub properties: BTreeMap<String, String>,
}

impl ZonePolicy {
    pub fn empty() -> Self {
        Self {
            capacity: 1,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeTrafficSemantics {
    pub directed: bool,
    pub speed_limit: Option<f64>,
    pub lane_type: Option<String>,
    pub reversible: Option<bool>,
    pub passing_allowed: Option<bool>,
    pub requires_claim: Option<bool>,
    pub waiting_allowed: Option<bool>,
    pub stop_allowed: Option<bool>,
    pub blocked: Option<bool>,
    pub priority: Option<f64>,
    pub capacity: Option<u64>,
    pub capacity_is_explicit: bool,
    pub clearance_width: Option<f64>,
    pub clearance_height: Option<f64>,
    pub surface_type: Option<String>,
    pub robot_class: Option<String>,
    pub allowed_payload: Option<String>,
    pub cost_bias: Option<f64>,
    pub no_stop: Option<bool>,
    pub preferred_direction: Option<String>,
    pub schedule_window: Option<String>,
    pub access_group: Option<String>,
    pub properties: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// internal parsing helpers
// ---------------------------------------------------------------------------

mod parse {
    pub fn trim(s: &str) -> &str {
        s.trim_matches(|c: char| c.is_whitespace())
    }

    pub fn lower(s: &str) -> String {
        s.to_ascii_lowercase()
    }

    pub fn parse_bool(value: &str) -> Option<bool> {
        match lower(trim(value)).as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        }
    }

    pub fn parse_u64(value: &str) -> Option<u64> {
        trim(value).parse::<u64>().ok()
    }

    pub fn parse_f64(value: &str) -> Option<f64> {
        trim(value).parse::<f64>().ok()
    }

    pub fn parse_zone_policy_kind(value: &str) -> super::ZonePolicyKind {
        use super::ZonePolicyKind::*;
        match lower(trim(value)).as_str() {
            "exclusive" => ExclusiveAccess,
            "shared" => SharedAccess,
            "corridor" => Corridor,
            "restricted" => Restricted,
            "slow" => Slowdown,
            "replanning" => Replanning,
            "no_stop" => NoStop,
            _ => Informational,
        }
    }

    pub fn is_known_zone_policy_kind(value: &str) -> bool {
        matches!(
            lower(trim(value)).as_str(),
            "informational"
                | "exclusive"
                | "shared"
                | "corridor"
                | "restricted"
                | "slow"
                | "replanning"
                | "no_stop"
        )
    }

    pub fn is_known_lane_type(value: &str) -> bool {
        matches!(
            lower(trim(value)).as_str(),
            "corridor" | "service" | "shared" | "restricted" | "passing" | "staging"
        )
    }

    pub fn is_known_preferred_direction(value: &str) -> bool {
        matches!(
            lower(trim(value)).as_str(),
            "forward"
                | "reverse"
                | "bidirectional"
                | "source_to_target"
                | "target_to_source"
                | "eastbound"
                | "westbound"
                | "northbound"
                | "southbound"
        )
    }

    pub fn is_known_zone_traffic_key(key: &str) -> bool {
        matches!(
            key,
            "traffic.policy"
                | "traffic.mode"
                | "traffic.capacity"
                | "traffic.max_occupancy"
                | "traffic.maxOccupancy"
                | "traffic.priority"
                | "traffic.claim_required"
                | "traffic.claimRequired"
                | "traffic.entry_rule"
                | "traffic.entryRule"
                | "traffic.exit_rule"
                | "traffic.exitRule"
                | "traffic.speed_limit"
                | "traffic.speedLimit"
                | "traffic.waiting_allowed"
                | "traffic.waitingAllowed"
                | "traffic.stop_allowed"
                | "traffic.stopAllowed"
                | "traffic.no_stop"
                | "traffic.noStop"
                | "traffic.replan_trigger"
                | "traffic.replanTrigger"
                | "traffic.blocked"
                | "traffic.robot_class"
                | "traffic.robotClass"
                | "traffic.schedule_window"
                | "traffic.scheduleWindow"
                | "traffic.access_group"
                | "traffic.accessGroup"
                | "traffic.blocks_entry_without_grant"
                | "traffic.blocksEntryWithoutGrant"
                | "traffic.blocks_traversal_without_grant"
                | "traffic.blocksTraversalWithoutGrant"
        )
    }

    pub fn is_known_edge_traffic_key(key: &str) -> bool {
        matches!(
            key,
            "traffic.speed_limit"
                | "traffic.speedLimit"
                | "traffic.lane_type"
                | "traffic.laneType"
                | "traffic.lane_kind"
                | "traffic.laneKind"
                | "traffic.reversible"
                | "traffic.passing_allowed"
                | "traffic.passingAllowed"
                | "traffic.blocked"
                | "traffic.priority"
                | "traffic.capacity"
                | "traffic.max_occupancy"
                | "traffic.maxOccupancy"
                | "traffic.clearance_width"
                | "traffic.clearanceWidth"
                | "traffic.clearance_height"
                | "traffic.clearanceHeight"
                | "traffic.surface_type"
                | "traffic.surfaceType"
                | "traffic.robot_class"
                | "traffic.robotClass"
                | "traffic.allowed_payload"
                | "traffic.allowedPayload"
                | "traffic.cost_bias"
                | "traffic.costBias"
                | "traffic.no_stop"
                | "traffic.noStop"
                | "traffic.preferred_direction"
                | "traffic.preferredDirection"
                | "traffic.direction"
                | "traffic.claim_required"
                | "traffic.claimRequired"
                | "traffic.waiting_allowed"
                | "traffic.waitingAllowed"
                | "traffic.stop_allowed"
                | "traffic.stopAllowed"
                | "traffic.schedule_window"
                | "traffic.scheduleWindow"
                | "traffic.access_group"
                | "traffic.accessGroup"
        )
    }

    pub fn canonical_zone_traffic_key(key: &str) -> &str {
        match key {
            "traffic.maxOccupancy" => "traffic.max_occupancy",
            "traffic.claimRequired" => "traffic.claim_required",
            "traffic.entryRule" => "traffic.entry_rule",
            "traffic.exitRule" => "traffic.exit_rule",
            "traffic.speedLimit" => "traffic.speed_limit",
            "traffic.waitingAllowed" => "traffic.waiting_allowed",
            "traffic.stopAllowed" => "traffic.stop_allowed",
            "traffic.noStop" => "traffic.no_stop",
            "traffic.replanTrigger" => "traffic.replan_trigger",
            "traffic.robotClass" => "traffic.robot_class",
            "traffic.scheduleWindow" => "traffic.schedule_window",
            "traffic.accessGroup" => "traffic.access_group",
            "traffic.blocksEntryWithoutGrant" => "traffic.blocks_entry_without_grant",
            "traffic.blocksTraversalWithoutGrant" => "traffic.blocks_traversal_without_grant",
            other => other,
        }
    }

    pub fn canonical_edge_traffic_key(key: &str) -> &str {
        match key {
            "traffic.speedLimit" => "traffic.speed_limit",
            "traffic.laneType" => "traffic.lane_type",
            "traffic.laneKind" => "traffic.lane_kind",
            "traffic.passingAllowed" => "traffic.passing_allowed",
            "traffic.maxOccupancy" => "traffic.max_occupancy",
            "traffic.clearanceWidth" => "traffic.clearance_width",
            "traffic.clearanceHeight" => "traffic.clearance_height",
            "traffic.surfaceType" => "traffic.surface_type",
            "traffic.robotClass" => "traffic.robot_class",
            "traffic.allowedPayload" => "traffic.allowed_payload",
            "traffic.costBias" => "traffic.cost_bias",
            "traffic.noStop" => "traffic.no_stop",
            "traffic.preferredDirection" => "traffic.preferred_direction",
            "traffic.claimRequired" => "traffic.claim_required",
            "traffic.waitingAllowed" => "traffic.waiting_allowed",
            "traffic.stopAllowed" => "traffic.stop_allowed",
            "traffic.scheduleWindow" => "traffic.schedule_window",
            "traffic.accessGroup" => "traffic.access_group",
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------
// public scalar parsers
// ---------------------------------------------------------------------------

pub fn parse_traffic_bool(value: &str) -> Result<bool> {
    parse::parse_bool(value)
        .ok_or_else(|| Error::parse(format!("invalid boolean traffic value: {value}")))
}

pub fn parse_traffic_u64(value: &str) -> Result<u64> {
    parse::parse_u64(value)
        .ok_or_else(|| Error::parse(format!("invalid unsigned traffic value: {value}")))
}

pub fn parse_traffic_f64(value: &str) -> Result<f64> {
    parse::parse_f64(value)
        .ok_or_else(|| Error::parse(format!("invalid numeric traffic value: {value}")))
}

pub fn parse_traffic_string(value: &str) -> Result<String> {
    let trimmed = parse::trim(value);
    if trimmed.is_empty() {
        return Err(Error::parse("traffic string value must not be empty"));
    }
    Ok(trimmed.to_owned())
}

// ---------------------------------------------------------------------------
// zone policy parsing
// ---------------------------------------------------------------------------

/// Parse supported `zone.properties` traffic keys into a typed policy model.
///
/// Mirrors `parse_zone_policy(...)` in `zone_policy.hpp`.
pub fn parse_zone_policy(properties: &BTreeMap<String, String>) -> ZonePolicy {
    let mut policy = ZonePolicy::empty();

    for (key, value) in properties {
        policy.properties.insert(key.clone(), value.clone());
        let canonical = parse::canonical_zone_traffic_key(key);

        match canonical {
            "traffic.policy" | "traffic.mode" => {
                policy.kind = parse::parse_zone_policy_kind(value);
            }
            "traffic.capacity" | "traffic.max_occupancy" => {
                if let Ok(n) = parse_traffic_u64(value) {
                    if n >= 1 {
                        policy.capacity = n;
                        policy.capacity_is_explicit = true;
                        if n > 1 && policy.kind == ZonePolicyKind::Informational {
                            policy.kind = ZonePolicyKind::CapacityLimited;
                        }
                    }
                }
            }
            "traffic.claim_required" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.requires_claim = b;
                    policy.blocks_entry_without_grant = b;
                }
            }
            "traffic.blocked" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.blocked = Some(b);
                    if b {
                        policy.kind = ZonePolicyKind::Restricted;
                        policy.blocks_entry_without_grant = true;
                        policy.blocks_traversal_without_grant = true;
                    }
                }
            }
            "traffic.blocks_entry_without_grant" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.blocks_entry_without_grant = b;
                }
            }
            "traffic.blocks_traversal_without_grant" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.blocks_traversal_without_grant = b;
                }
            }
            "traffic.priority" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    policy.priority = Some(v);
                }
            }
            "traffic.speed_limit" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    policy.speed_limit = Some(v);
                }
            }
            "traffic.waiting_allowed" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.waiting_allowed = Some(b);
                }
            }
            "traffic.stop_allowed" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.stop_allowed = Some(b);
                }
            }
            "traffic.no_stop" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.stop_allowed = Some(!b);
                    if b {
                        policy.kind = ZonePolicyKind::NoStop;
                    }
                }
            }
            "traffic.replan_trigger" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    policy.replan_trigger = Some(b);
                }
                if policy.replan_trigger.unwrap_or(false) {
                    policy.kind = ZonePolicyKind::Replanning;
                }
            }
            "traffic.entry_rule" => {
                if let Ok(s) = parse_traffic_string(value) {
                    policy.entry_rule = Some(s);
                }
            }
            "traffic.exit_rule" => {
                if let Ok(s) = parse_traffic_string(value) {
                    policy.exit_rule = Some(s);
                }
            }
            "traffic.robot_class" => {
                if let Ok(s) = parse_traffic_string(value) {
                    policy.robot_class = Some(s);
                }
            }
            "traffic.schedule_window" => {
                if let Ok(s) = parse_traffic_string(value) {
                    policy.schedule_window = Some(s);
                }
            }
            "traffic.access_group" => {
                if let Ok(s) = parse_traffic_string(value) {
                    policy.access_group = Some(s);
                }
            }
            _ => {}
        }
    }

    if policy.blocked.unwrap_or(false) {
        policy.kind = ZonePolicyKind::Restricted;
        policy.blocks_entry_without_grant = true;
        policy.blocks_traversal_without_grant = true;
    } else if policy.replan_trigger.unwrap_or(false) {
        policy.kind = ZonePolicyKind::Replanning;
    } else if policy.kind != ZonePolicyKind::Restricted && policy.stop_allowed == Some(false) {
        policy.kind = ZonePolicyKind::NoStop;
    }

    policy
}

// ---------------------------------------------------------------------------
// edge traffic semantics
// ---------------------------------------------------------------------------

/// Parse supported `edge.properties` traffic keys into typed traversal
/// semantics. The `directed` argument is structural and wins over property
/// hints. Mirrors `parse_edge_traffic_semantics(...)` in `zone_policy.hpp`.
pub fn parse_edge_traffic_semantics(
    properties: &BTreeMap<String, String>,
    directed: bool,
) -> EdgeTrafficSemantics {
    let mut semantics = EdgeTrafficSemantics {
        directed,
        ..Default::default()
    };

    for (key, value) in properties {
        semantics.properties.insert(key.clone(), value.clone());
        let canonical = parse::canonical_edge_traffic_key(key);

        match canonical {
            "traffic.speed_limit" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    semantics.speed_limit = Some(v);
                }
            }
            "traffic.lane_type" | "traffic.lane_kind" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.lane_type = Some(parse::lower(&s));
                }
            }
            "traffic.reversible" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.reversible = Some(b);
                }
            }
            "traffic.passing_allowed" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.passing_allowed = Some(b);
                }
            }
            "traffic.claim_required" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.requires_claim = Some(b);
                }
            }
            "traffic.waiting_allowed" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.waiting_allowed = Some(b);
                }
            }
            "traffic.stop_allowed" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.stop_allowed = Some(b);
                }
            }
            "traffic.blocked" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.blocked = Some(b);
                }
            }
            "traffic.priority" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    semantics.priority = Some(v);
                }
            }
            "traffic.capacity" | "traffic.max_occupancy" => {
                if let Ok(n) = parse_traffic_u64(value) {
                    semantics.capacity = Some(n);
                    semantics.capacity_is_explicit = true;
                }
            }
            "traffic.clearance_width" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    semantics.clearance_width = Some(v);
                }
            }
            "traffic.clearance_height" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    semantics.clearance_height = Some(v);
                }
            }
            "traffic.surface_type" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.surface_type = Some(s);
                }
            }
            "traffic.robot_class" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.robot_class = Some(s);
                }
            }
            "traffic.allowed_payload" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.allowed_payload = Some(s);
                }
            }
            "traffic.cost_bias" => {
                if let Ok(v) = parse_traffic_f64(value) {
                    semantics.cost_bias = Some(v);
                }
            }
            "traffic.no_stop" => {
                if let Ok(b) = parse_traffic_bool(value) {
                    semantics.no_stop = Some(b);
                }
            }
            "traffic.preferred_direction" | "traffic.direction" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.preferred_direction = Some(parse::lower(&s));
                }
            }
            "traffic.schedule_window" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.schedule_window = Some(s);
                }
            }
            "traffic.access_group" => {
                if let Ok(s) = parse_traffic_string(value) {
                    semantics.access_group = Some(s);
                }
            }
            _ => {}
        }
    }

    if semantics.no_stop.unwrap_or(false) {
        semantics.stop_allowed = Some(false);
    }

    semantics
}

// ---------------------------------------------------------------------------
// validation
// ---------------------------------------------------------------------------

fn sort_traffic_issues(issues: &mut [TrafficParseIssue]) {
    issues.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then_with(|| (a.severity as u8).cmp(&(b.severity as u8)))
            .then_with(|| a.message.cmp(&b.message))
    });
}

pub fn validate_zone_traffic_properties(
    properties: &BTreeMap<String, String>,
) -> Vec<TrafficParseIssue> {
    let mut issues: Vec<TrafficParseIssue> = Vec::new();

    for (key, value) in properties {
        let canonical = parse::canonical_zone_traffic_key(key);
        if key.starts_with("traffic.") && !parse::is_known_zone_traffic_key(key) {
            issues.push(TrafficParseIssue {
                severity: TrafficIssueSeverity::Warning,
                key: key.clone(),
                message: "unknown zone traffic key".into(),
            });
        } else if canonical == "traffic.policy" || canonical == "traffic.mode" {
            if !parse::is_known_zone_policy_kind(value) {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Warning,
                    key: key.clone(),
                    message: "unknown zone policy keyword".into(),
                });
            }
        } else if canonical == "traffic.capacity" || canonical == "traffic.max_occupancy" {
            let parsed = parse_traffic_u64(value);
            if parsed.is_err() || parsed.as_ref().ok().copied().unwrap_or(0) < 1 {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic.capacity must be an integer >= 1".into(),
                });
            }
        } else if canonical == "traffic.priority" {
            if parse_traffic_f64(value).is_err() {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic.priority must be numeric".into(),
                });
            }
        } else if canonical == "traffic.speed_limit" {
            let parsed = parse_traffic_f64(value);
            if parsed.is_err() || parsed.as_ref().ok().copied().unwrap_or(0.0) <= 0.0 {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic.speed_limit must be positive".into(),
                });
            }
        } else if matches!(
            canonical,
            "traffic.claim_required"
                | "traffic.waiting_allowed"
                | "traffic.stop_allowed"
                | "traffic.blocked"
                | "traffic.replan_trigger"
                | "traffic.no_stop"
                | "traffic.blocks_entry_without_grant"
                | "traffic.blocks_traversal_without_grant"
        ) {
            if parse_traffic_bool(value).is_err() {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic boolean key must parse as true/false".into(),
                });
            }
        } else if matches!(
            canonical,
            "traffic.entry_rule"
                | "traffic.exit_rule"
                | "traffic.robot_class"
                | "traffic.schedule_window"
                | "traffic.access_group"
        ) {
            if parse_traffic_string(value).is_err() {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic string key must not be empty".into(),
                });
            }
        }
    }

    if let Some(no_stop) = properties.get("traffic.no_stop") {
        if parse::parse_bool(no_stop).unwrap_or(false) {
            if let Some(stop_allowed) = properties.get("traffic.stop_allowed") {
                if parse::parse_bool(stop_allowed).unwrap_or(false) {
                    issues.push(TrafficParseIssue {
                        severity: TrafficIssueSeverity::Warning,
                        key: "traffic.stop_allowed".into(),
                        message: "traffic.stop_allowed=true conflicts with traffic.no_stop=true"
                            .into(),
                    });
                }
            }
        }
    }

    sort_traffic_issues(&mut issues);
    issues
}

pub fn validate_edge_traffic_properties(
    properties: &BTreeMap<String, String>,
) -> Vec<TrafficParseIssue> {
    let mut issues: Vec<TrafficParseIssue> = Vec::new();

    for (key, value) in properties {
        let canonical = parse::canonical_edge_traffic_key(key);
        if key.starts_with("traffic.") && !parse::is_known_edge_traffic_key(key) {
            issues.push(TrafficParseIssue {
                severity: TrafficIssueSeverity::Warning,
                key: key.clone(),
                message: "unknown edge traffic key".into(),
            });
        } else if canonical == "traffic.capacity" || canonical == "traffic.max_occupancy" {
            let parsed = parse_traffic_u64(value);
            if parsed.is_err() || parsed.as_ref().ok().copied().unwrap_or(0) < 1 {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic.capacity must be an integer >= 1".into(),
                });
            }
        } else if matches!(
            canonical,
            "traffic.speed_limit"
                | "traffic.priority"
                | "traffic.clearance_width"
                | "traffic.clearance_height"
                | "traffic.cost_bias"
        ) {
            let parsed = parse_traffic_f64(value);
            if parsed.is_err() {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic numeric key must parse as number".into(),
                });
            } else if matches!(
                canonical,
                "traffic.speed_limit" | "traffic.clearance_width" | "traffic.clearance_height"
            ) && parsed.as_ref().ok().copied().unwrap_or(0.0) <= 0.0
            {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic positive numeric key must be > 0".into(),
                });
            }
        } else if matches!(
            canonical,
            "traffic.reversible"
                | "traffic.passing_allowed"
                | "traffic.no_stop"
                | "traffic.blocked"
                | "traffic.claim_required"
                | "traffic.waiting_allowed"
                | "traffic.stop_allowed"
        ) {
            if parse_traffic_bool(value).is_err() {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic boolean key must parse as true/false".into(),
                });
            }
        } else if matches!(
            canonical,
            "traffic.lane_type"
                | "traffic.lane_kind"
                | "traffic.surface_type"
                | "traffic.robot_class"
                | "traffic.allowed_payload"
                | "traffic.preferred_direction"
                | "traffic.direction"
                | "traffic.schedule_window"
                | "traffic.access_group"
        ) {
            if parse_traffic_string(value).is_err() {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Error,
                    key: key.clone(),
                    message: "traffic string key must not be empty".into(),
                });
            } else if matches!(canonical, "traffic.lane_type" | "traffic.lane_kind")
                && !parse::is_known_lane_type(value)
            {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Warning,
                    key: key.clone(),
                    message: "unknown lane type keyword".into(),
                });
            } else if matches!(
                canonical,
                "traffic.preferred_direction" | "traffic.direction"
            ) && !parse::is_known_preferred_direction(value)
            {
                issues.push(TrafficParseIssue {
                    severity: TrafficIssueSeverity::Warning,
                    key: key.clone(),
                    message: "unknown preferred direction keyword".into(),
                });
            }
        }
    }

    if let Some(no_stop) = properties.get("traffic.no_stop") {
        if parse::parse_bool(no_stop).unwrap_or(false) {
            if let Some(stop_allowed) = properties.get("traffic.stop_allowed") {
                if parse::parse_bool(stop_allowed).unwrap_or(true) {
                    issues.push(TrafficParseIssue {
                        severity: TrafficIssueSeverity::Warning,
                        key: "traffic.stop_allowed".into(),
                        message: "traffic.stop_allowed=true conflicts with traffic.no_stop=true"
                            .into(),
                    });
                }
            }
        }
    }

    sort_traffic_issues(&mut issues);
    issues
}

// ---------------------------------------------------------------------------
// merge / derive
// ---------------------------------------------------------------------------

/// Merge recursive zone policies deterministically.
///
/// Rules:
/// - `Restricted` dominates everything.
/// - `ExclusiveAccess` dominates `SharedAccess`.
/// - smaller capacity wins when capacity is explicitly constrained.
/// - child optionals override inherited values when present.
pub fn merge_zone_policy(parent: &ZonePolicy, child: &ZonePolicy) -> ZonePolicy {
    let mut merged = parent.clone();

    let merged_kind =
        if parent.kind == ZonePolicyKind::Restricted || child.kind == ZonePolicyKind::Restricted {
            ZonePolicyKind::Restricted
        } else if parent.kind == ZonePolicyKind::ExclusiveAccess
            || child.kind == ZonePolicyKind::ExclusiveAccess
        {
            ZonePolicyKind::ExclusiveAccess
        } else if child.kind != ZonePolicyKind::Informational {
            child.kind
        } else {
            parent.kind
        };

    merged.kind = merged_kind;

    if parent.capacity_is_explicit && child.capacity_is_explicit {
        merged.capacity = parent.capacity.min(child.capacity);
        merged.capacity_is_explicit = true;
    } else if child.capacity_is_explicit {
        merged.capacity = child.capacity;
        merged.capacity_is_explicit = true;
    } else if parent.capacity_is_explicit {
        merged.capacity = parent.capacity;
        merged.capacity_is_explicit = true;
    } else {
        merged.capacity = child.capacity;
        merged.capacity_is_explicit = false;
    }

    merged.requires_claim = parent.requires_claim || child.requires_claim;
    merged.blocks_traversal_without_grant =
        parent.blocks_traversal_without_grant || child.blocks_traversal_without_grant;
    merged.blocks_entry_without_grant =
        parent.blocks_entry_without_grant || child.blocks_entry_without_grant;

    if child.priority.is_some() {
        merged.priority = child.priority;
    }
    if child.speed_limit.is_some() {
        merged.speed_limit = child.speed_limit;
    }
    if child.waiting_allowed.is_some() {
        merged.waiting_allowed = child.waiting_allowed;
    }
    if child.stop_allowed.is_some() {
        merged.stop_allowed = child.stop_allowed;
    }
    if child.blocked.is_some() {
        merged.blocked = child.blocked;
    }
    if child.replan_trigger.is_some() {
        merged.replan_trigger = child.replan_trigger;
    }
    if child.entry_rule.is_some() {
        merged.entry_rule = child.entry_rule.clone();
    }
    if child.exit_rule.is_some() {
        merged.exit_rule = child.exit_rule.clone();
    }
    if child.robot_class.is_some() {
        merged.robot_class = child.robot_class.clone();
    }
    if child.schedule_window.is_some() {
        merged.schedule_window = child.schedule_window.clone();
    }
    if child.access_group.is_some() {
        merged.access_group = child.access_group.clone();
    }

    for (k, v) in &child.properties {
        merged.properties.insert(k.clone(), v.clone());
    }

    if merged.kind == ZonePolicyKind::Restricted {
        merged.blocks_entry_without_grant = true;
        merged.blocks_traversal_without_grant = true;
        merged.waiting_allowed = Some(false);
        merged.stop_allowed = Some(false);
    } else if merged.kind == ZonePolicyKind::NoStop {
        merged.stop_allowed = Some(false);
    }

    merged
}

/// Combine structural edge facts, parsed edge properties, and containing-zone
/// restrictions. Mirrors `derive_effective_edge_semantics(...)`.
pub fn derive_effective_edge_semantics(
    properties: &BTreeMap<String, String>,
    directed: bool,
    zone_policies: &[ZonePolicy],
) -> EdgeTrafficSemantics {
    let mut semantics = parse_edge_traffic_semantics(properties, directed);
    semantics.directed = directed;

    for zone_policy in zone_policies {
        if let Some(zsl) = zone_policy.speed_limit {
            semantics.speed_limit = Some(match semantics.speed_limit {
                None => zsl,
                Some(curr) => curr.min(zsl),
            });
        }

        if zone_policy.capacity_is_explicit || zone_policy.kind == ZonePolicyKind::CapacityLimited {
            semantics.capacity = Some(match semantics.capacity {
                None => zone_policy.capacity,
                Some(curr) => curr.min(zone_policy.capacity),
            });
            semantics.capacity_is_explicit = true;
        }

        if semantics.robot_class.is_none() && zone_policy.robot_class.is_some() {
            semantics.robot_class = zone_policy.robot_class.clone();
        }
        if semantics.schedule_window.is_none() && zone_policy.schedule_window.is_some() {
            semantics.schedule_window = zone_policy.schedule_window.clone();
        }
        if semantics.access_group.is_none() && zone_policy.access_group.is_some() {
            semantics.access_group = zone_policy.access_group.clone();
        }
        if semantics.waiting_allowed.is_none() && zone_policy.waiting_allowed.is_some() {
            semantics.waiting_allowed = zone_policy.waiting_allowed;
        }
        if semantics.stop_allowed.is_none() && zone_policy.stop_allowed.is_some() {
            semantics.stop_allowed = zone_policy.stop_allowed;
        }
        if semantics.requires_claim.is_none() {
            semantics.requires_claim = Some(zone_policy.requires_claim);
        }

        if semantics.priority.is_none() && zone_policy.priority.is_some() {
            semantics.priority = zone_policy.priority;
        }

        if zone_policy.kind == ZonePolicyKind::NoStop
            || zone_policy.blocked.unwrap_or(false)
            || zone_policy.kind == ZonePolicyKind::Restricted
        {
            semantics.no_stop = Some(true);
            semantics.stop_allowed = Some(false);
        }

        if zone_policy.blocked.unwrap_or(false) || zone_policy.kind == ZonePolicyKind::Restricted {
            semantics.blocked = Some(true);
        }

        if zone_policy.kind == ZonePolicyKind::Slowdown {
            semantics.cost_bias = Some(semantics.cost_bias.unwrap_or(0.0) + 1.0);
        }
    }

    semantics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), (*v).to_string());
        }
        m
    }

    #[test]
    fn parse_zone_policy_blocked_upgrades_to_restricted() {
        let p = parse_zone_policy(&props(&[("traffic.blocked", "true")]));
        assert_eq!(p.kind, ZonePolicyKind::Restricted);
        assert!(p.blocks_entry_without_grant);
        assert!(p.blocks_traversal_without_grant);
        assert_eq!(p.blocked, Some(true));
    }

    #[test]
    fn parse_zone_policy_camelcase_aliases_resolve() {
        let p = parse_zone_policy(&props(&[("traffic.maxOccupancy", "3")]));
        assert!(p.capacity_is_explicit);
        assert_eq!(p.capacity, 3);
        assert_eq!(p.kind, ZonePolicyKind::CapacityLimited);
    }

    #[test]
    fn parse_zone_policy_no_stop_implies_stop_allowed_false() {
        let p = parse_zone_policy(&props(&[("traffic.no_stop", "true")]));
        assert_eq!(p.kind, ZonePolicyKind::NoStop);
        assert_eq!(p.stop_allowed, Some(false));
    }

    #[test]
    fn parse_edge_no_stop_clears_stop_allowed() {
        let s = parse_edge_traffic_semantics(&props(&[("traffic.no_stop", "true")]), false);
        assert_eq!(s.no_stop, Some(true));
        assert_eq!(s.stop_allowed, Some(false));
    }

    #[test]
    fn merge_priority_restricted_over_exclusive() {
        let parent = ZonePolicy {
            kind: ZonePolicyKind::ExclusiveAccess,
            ..ZonePolicy::empty()
        };
        let child = ZonePolicy {
            kind: ZonePolicyKind::Restricted,
            ..ZonePolicy::empty()
        };
        let merged = merge_zone_policy(&parent, &child);
        assert_eq!(merged.kind, ZonePolicyKind::Restricted);
        assert!(merged.blocks_entry_without_grant);
        assert_eq!(merged.stop_allowed, Some(false));
    }

    #[test]
    fn merge_capacity_picks_minimum() {
        let mut parent = ZonePolicy::empty();
        parent.capacity = 5;
        parent.capacity_is_explicit = true;
        let mut child = ZonePolicy::empty();
        child.capacity = 2;
        child.capacity_is_explicit = true;
        let merged = merge_zone_policy(&parent, &child);
        assert_eq!(merged.capacity, 2);
        assert!(merged.capacity_is_explicit);
    }

    #[test]
    fn validate_zone_unknown_key_warns() {
        let issues = validate_zone_traffic_properties(&props(&[("traffic.bogus", "x")]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, TrafficIssueSeverity::Warning);
        assert!(issues[0].message.contains("unknown"));
    }

    #[test]
    fn validate_edge_speed_limit_must_be_positive() {
        let issues = validate_edge_traffic_properties(&props(&[("traffic.speed_limit", "0")]));
        assert!(
            issues.iter().any(
                |i| i.severity == TrafficIssueSeverity::Error && i.message.contains("positive")
            )
        );
    }

    #[test]
    fn derive_picks_min_speed_limit_from_zone() {
        let edge_props = props(&[("traffic.speed_limit", "5.0")]);
        let zone_policy = ZonePolicy {
            speed_limit: Some(2.0),
            ..ZonePolicy::empty()
        };
        let s = derive_effective_edge_semantics(&edge_props, true, &[zone_policy]);
        assert_eq!(s.speed_limit, Some(2.0));
        assert!(s.directed);
    }
}
