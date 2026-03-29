//! Route planner: given train positions + destinations, compute routes and track commands.
//!
//! Uses [`TrackGraph`] for BFS pathfinding and converts the resulting
//! [`Route`] into a sequence of [`TrackCommand`]s that can be sent to
//! the Pi hardware API.

use serde::{Deserialize, Serialize};

use crate::layout::graph::{PointSetting, Route, TrackGraph};
use crate::layout::TrackLayout;
use crate::pi_client::{PointDirection, TrackDirection};
use crate::state::TrainPosition;

// ---------------------------------------------------------------------------
// Track commands
// ---------------------------------------------------------------------------

/// A single command to be sent to the Pi track hardware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum TrackCommand {
    /// Set a point (switch).
    SetPoint {
        point_id: u8,
        direction: PointDirection,
    },
    /// Set a track segment's speed and direction.
    SetTrackSpeed {
        track_id: u8,
        direction: TrackDirection,
        speed: u8,
    },
    /// Stop a specific track segment.
    StopTrack { track_id: u8 },
}

impl std::fmt::Display for TrackCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackCommand::SetPoint {
                point_id,
                direction,
            } => write!(f, "SET POINT {point_id} → {direction}"),
            TrackCommand::SetTrackSpeed {
                track_id,
                direction,
                speed,
            } => write!(f, "SET TRACK {track_id} → {direction} @ {speed}%"),
            TrackCommand::StopTrack { track_id } => write!(f, "STOP TRACK {track_id}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Planned route (enriched with commands and description)
// ---------------------------------------------------------------------------

/// A planned route for one train: source, destination, the hops, and the
/// concrete commands to execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRoute {
    /// Train index (0-based) in the original request.
    pub train_index: usize,
    /// Starting sensor.
    pub from_sensor: u8,
    /// Destination sensor.
    pub to_sensor: u8,
    /// Track segments traversed (in order).
    pub track_ids: Vec<u8>,
    /// Number of sensor hops.
    pub hop_count: usize,
    /// Human-readable description.
    pub description: String,
    /// Commands to execute (in order: points first, then tracks).
    pub commands: Vec<TrackCommand>,
}

// ---------------------------------------------------------------------------
// Route-planning errors
// ---------------------------------------------------------------------------

/// Error from the route planner.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("no destination set for train {train_index} (sensor {sensor})")]
    NoDestination { train_index: usize, sensor: u8 },
    #[error("no route from sensor {from} to sensor {to} for train {train_index}")]
    NoRoute {
        train_index: usize,
        from: u8,
        to: u8,
    },
    #[error("train {train_id} not found in current positions — run /initialise first")]
    TrainNotFound { train_id: u8 },
    #[error("no current train state — POST /initialise before /route")]
    NoCurrentState,
    #[error("layout load/validate error: {0}")]
    Layout(String),
}

// ---------------------------------------------------------------------------
// Default speed for route execution
// ---------------------------------------------------------------------------

/// Default speed percentage for powered tracks in a route (safe for curves).
pub const DEFAULT_ROUTE_SPEED: u8 = 40;

// ---------------------------------------------------------------------------
// Route planning
// ---------------------------------------------------------------------------

/// Build a graph from the canonical layout file and plan routes for all trains
/// that have a `destination` set.
pub fn plan_routes(trains: &[TrainPosition]) -> Result<Vec<PlannedRoute>, PlanError> {
    let layout = load_and_validate_layout()?;
    let graph = TrackGraph::from_layout(&layout);
    plan_routes_with_graph(trains, &graph)
}

/// Plan routes using a pre-built graph (useful for testing without file I/O).
pub fn plan_routes_with_graph(
    trains: &[TrainPosition],
    graph: &TrackGraph,
) -> Result<Vec<PlannedRoute>, PlanError> {
    let mut planned = Vec::new();
    for (i, train) in trains.iter().enumerate() {
        let dest = train.destination.ok_or(PlanError::NoDestination {
            train_index: i,
            sensor: train.sensor,
        })?;
        let route = graph
            .find_route(train.sensor, dest)
            .ok_or(PlanError::NoRoute {
                train_index: i,
                from: train.sensor,
                to: dest,
            })?;
        planned.push(route_to_plan(i, &route, graph));
    }
    Ok(planned)
}

/// Convert a graph [`Route`] into a [`PlannedRoute`] with commands and description.
fn route_to_plan(train_index: usize, route: &Route, graph: &TrackGraph) -> PlannedRoute {
    use crate::layout::graph::TraverseDirection;
    use std::collections::BTreeMap;

    let point_settings = route.point_settings();
    let track_ids = route.track_ids();

    // Build commands: first set all required points, then power the tracks.
    let mut commands: Vec<TrackCommand> = Vec::new();

    // 1. Point settings
    for ps in &point_settings {
        commands.push(TrackCommand::SetPoint {
            point_id: ps.point_id,
            direction: ps.direction,
        });
    }

    // 2. Track power: determine direction from the hop's traverse_direction.
    //    For each track segment, use the direction from the first hop on that track.
    let mut track_direction: BTreeMap<u8, TrackDirection> = BTreeMap::new();
    for hop in &route.hops {
        track_direction
            .entry(hop.track_id)
            .or_insert_with(|| match hop.traverse_direction {
                TraverseDirection::Fwd => TrackDirection::Fwd,
                TraverseDirection::Bck => TrackDirection::Bck,
            });
    }

    for &tid in &track_ids {
        let direction = track_direction
            .get(&tid)
            .copied()
            .unwrap_or(TrackDirection::Fwd);
        commands.push(TrackCommand::SetTrackSpeed {
            track_id: tid,
            direction,
            speed: DEFAULT_ROUTE_SPEED,
        });
    }

    // Build description
    let description = describe_route(route, &point_settings, &track_ids, graph);

    PlannedRoute {
        train_index,
        from_sensor: route.from,
        to_sensor: route.to,
        track_ids,
        hop_count: route.hops.len(),
        description,
        commands,
    }
}

/// Human-readable description of a route.
fn describe_route(
    route: &Route,
    point_settings: &[PointSetting],
    track_ids: &[u8],
    graph: &TrackGraph,
) -> String {
    if route.hops.is_empty() {
        return format!(
            "Train already at sensor {} — no movement needed.",
            route.from
        );
    }

    let mut parts = Vec::new();

    // Sensor path
    let mut sensor_path: Vec<u8> = vec![route.from];
    for hop in &route.hops {
        sensor_path.push(hop.to);
    }
    let sensor_str: Vec<String> = sensor_path.iter().map(|s| format!("S{s}")).collect();
    parts.push(format!("Path: {}", sensor_str.join(" → ")));

    // Tracks
    let track_str: Vec<String> = track_ids.iter().map(|t| format!("T{t}")).collect();
    parts.push(format!("Tracks: {}", track_str.join(", ")));

    // Points
    if !point_settings.is_empty() {
        let point_str: Vec<String> = point_settings.iter().map(|ps| ps.to_string()).collect();
        parts.push(format!("Points: {}", point_str.join(", ")));
    }

    // Station names (if source or destination is a station sensor)
    // This would require the layout's stations, which we don't have here.
    // For now, note the sensor's owning track.
    if let Some(&from_track) = graph.sensor_track.get(&route.from) {
        if let Some(&to_track) = graph.sensor_track.get(&route.to) {
            if from_track != to_track {
                parts.push(format!(
                    "Crosses from track {from_track} to track {to_track}"
                ));
            }
        }
    }

    parts.push(format!("{} hop(s)", route.hops.len()));
    parts.join(". ")
}

/// Load and validate the canonical track layout.
fn load_and_validate_layout() -> Result<TrackLayout, PlanError> {
    let path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
    let layout = TrackLayout::from_path(&path).map_err(|e| PlanError::Layout(e.to_string()))?;
    layout
        .validate()
        .map_err(|e| PlanError::Layout(e.to_string()))?;
    Ok(layout)
}

// ---------------------------------------------------------------------------
// Target-based route planning (POST /route)
//
// The route command takes the same format as /initialise — an InitialiseRequest
// describing where each train should end up and which direction it should face.
// Current positions are loaded from data/runtime/trains.json.
//
// The planner produces a RoutePlan: an ordered list of steps that can be
// executed sequentially. Each step either energises a track, sets a point,
// or de-energises a track. No two trains may occupy the same track
// simultaneously. Tracks are de-energised after a train leaves.
// Points are never reset — the track hardware handles that automatically.
// ---------------------------------------------------------------------------

use crate::layout::graph::TraverseDirection;
use crate::state::{InitialiseRequest, TrainDirection};
use std::collections::{BTreeMap, BTreeSet};

/// One step in a route execution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RouteStep {
    /// Set a point before a train enters the next track.
    SetPoint {
        train: u8,
        point_id: u8,
        direction: PointDirection,
    },
    /// Energise a track segment so a train can move onto it.
    EnergiseTrack {
        train: u8,
        track_id: u8,
        direction: TrackDirection,
        speed: u8,
    },
    /// Wait for a train to reach a sensor (confirms train has arrived).
    AwaitSensor {
        train: u8,
        sensor: u8,
        /// Human note about what we're waiting for.
        note: String,
    },
    /// De-energise a track after the train has left it.
    DeEnergiseTrack { train: u8, track_id: u8 },
}

impl std::fmt::Display for RouteStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteStep::SetPoint {
                train,
                point_id,
                direction,
            } => write!(f, "Train {train}: SET POINT {point_id} → {direction}"),
            RouteStep::EnergiseTrack {
                train,
                track_id,
                direction,
                speed,
            } => write!(
                f,
                "Train {train}: ENERGISE TRACK {track_id} → {direction} @ {speed}%"
            ),
            RouteStep::AwaitSensor {
                train,
                sensor,
                note,
            } => write!(f, "Train {train}: AWAIT SENSOR {sensor} ({note})"),
            RouteStep::DeEnergiseTrack { train, track_id } => {
                write!(f, "Train {train}: DE-ENERGISE TRACK {track_id}")
            }
        }
    }
}

/// A complete plan for routing one train from its current position to a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainRoutePlan {
    /// Train identifier.
    pub train: u8,
    /// Current sensor.
    pub from_sensor: u8,
    /// Target sensor.
    pub to_sensor: u8,
    /// Target direction the train should face on arrival.
    pub target_direction: String,
    /// Track segments this route passes through (in order).
    pub track_ids: Vec<u8>,
    /// Number of sensor hops.
    pub hop_count: usize,
    /// Human-readable description of the route.
    pub description: String,
    /// Ordered steps to execute.
    pub steps: Vec<RouteStep>,
    /// True if the train is already at the target.
    pub already_there: bool,
}

/// Full route plan for all trains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutePlan {
    /// Per-train plans.
    pub trains: Vec<TrainRoutePlan>,
    /// Warnings (e.g. track conflicts requiring sequencing).
    pub warnings: Vec<String>,
}

/// Plan routes from current positions to targets.
///
/// `current` — the current state (from `/initialise`, saved in `data/runtime/trains.json`).
/// `target` — the desired end state (same format).
///
/// Each train in `target` must have a matching `train` id in `current`.
pub fn plan_target_routes(
    current: &InitialiseRequest,
    target: &InitialiseRequest,
) -> Result<RoutePlan, PlanError> {
    let layout = load_and_validate_layout()?;
    let graph = TrackGraph::from_layout(&layout);
    plan_target_routes_with(current, target, &graph)
}

/// Plan target routes using a pre-built graph (testable without I/O).
pub fn plan_target_routes_with(
    current: &InitialiseRequest,
    target: &InitialiseRequest,
    graph: &TrackGraph,
) -> Result<RoutePlan, PlanError> {
    // Build current position lookup: train_id → (sensor, direction)
    let current_map: BTreeMap<u8, &crate::state::TrainPosition> =
        current.trains.iter().map(|t| (t.train, t)).collect();

    let mut train_plans = Vec::new();
    let mut warnings = Vec::new();
    // Track segments reserved by trains that have already been planned.
    let mut reserved_segments: BTreeSet<u8> = BTreeSet::new();

    for target_train in &target.trains {
        let cur = current_map
            .get(&target_train.train)
            .ok_or(PlanError::TrainNotFound {
                train_id: target_train.train,
            })?;

        let plan = plan_single_train(
            target_train.train,
            cur.sensor,
            target_train.sensor,
            target_train.direction,
            &reserved_segments,
            graph,
        )?;

        // Check for track conflicts with other planned trains.
        for &tid in &plan.track_ids {
            if reserved_segments.contains(&tid) {
                warnings.push(format!(
                    "Train {} needs track {} which is reserved by another train — \
                     trains must be sequenced (move one at a time through shared tracks)",
                    target_train.train, tid
                ));
            }
        }

        // Reserve tracks for this train.
        for &tid in &plan.track_ids {
            reserved_segments.insert(tid);
        }

        train_plans.push(plan);
    }

    Ok(RoutePlan {
        trains: train_plans,
        warnings,
    })
}

/// Plan a single train's route from current sensor to target sensor.
fn plan_single_train(
    train_id: u8,
    from_sensor: u8,
    to_sensor: u8,
    target_direction: TrainDirection,
    reserved: &BTreeSet<u8>,
    graph: &TrackGraph,
) -> Result<TrainRoutePlan, PlanError> {
    // Already there?
    if from_sensor == to_sensor {
        return Ok(TrainRoutePlan {
            train: train_id,
            from_sensor,
            to_sensor,
            target_direction: target_direction.to_string(),
            track_ids: vec![],
            hop_count: 0,
            description: format!(
                "Train {} already at sensor {} — no movement needed.",
                train_id, from_sensor
            ),
            steps: vec![],
            already_there: true,
        });
    }

    // Find a route.
    let route = graph
        .find_route(from_sensor, to_sensor)
        .ok_or(PlanError::NoRoute {
            train_index: 0,
            from: from_sensor,
            to: to_sensor,
        })?;

    // Check the arrival direction matches the target.
    // The last hop's traverse_direction tells us which way the train arrives.
    let arrives_fwd = if let Some(last_hop) = route.hops.last() {
        last_hop.traverse_direction == TraverseDirection::Fwd
    } else {
        true
    };
    let target_fwd = target_direction == TrainDirection::Fwd;

    let mut route_warnings = Vec::new();
    if arrives_fwd != target_fwd {
        route_warnings.push(format!(
            "Train {} arrives {} but target direction is {} — \
             the route reaches sensor {} from the {} direction",
            train_id,
            if arrives_fwd { "fwd" } else { "bwd" },
            target_direction,
            to_sensor,
            if arrives_fwd { "forward" } else { "backward" },
        ));
    }

    let point_settings = route.point_settings();
    let track_ids = route.track_ids();

    // Check for reserved track conflicts.
    let mut conflicts = Vec::new();
    for &tid in &track_ids {
        if reserved.contains(&tid) {
            conflicts.push(tid);
        }
    }

    // Build execution steps: for each hop, set points → energise track → await sensor → de-energise.
    let mut steps = Vec::new();

    // Track which tracks the train is currently powering (for de-energise).
    let mut prev_track: Option<u8> = None;

    // Group hops by track segment to determine per-track direction.
    let mut track_direction: BTreeMap<u8, TrackDirection> = BTreeMap::new();
    for hop in &route.hops {
        track_direction
            .entry(hop.track_id)
            .or_insert_with(|| match hop.traverse_direction {
                TraverseDirection::Fwd => TrackDirection::Fwd,
                TraverseDirection::Bck => TrackDirection::Bck,
            });
    }

    // Set all required points up front (before any movement).
    for ps in &point_settings {
        steps.push(RouteStep::SetPoint {
            train: train_id,
            point_id: ps.point_id,
            direction: ps.direction,
        });
    }

    // Now walk through hops: energise tracks as needed, await sensors, de-energise.
    for hop in &route.hops {
        let current_track = hop.track_id;

        // If we've moved to a new track segment, de-energise the old one.
        if let Some(pt) = prev_track {
            if pt != current_track {
                steps.push(RouteStep::DeEnergiseTrack {
                    train: train_id,
                    track_id: pt,
                });
            }
        }

        // Energise the current track if we haven't already (or if we just de-energised it).
        let need_energise = prev_track != Some(current_track);
        if need_energise {
            let direction = track_direction
                .get(&current_track)
                .copied()
                .unwrap_or(TrackDirection::Fwd);
            steps.push(RouteStep::EnergiseTrack {
                train: train_id,
                track_id: current_track,
                direction,
                speed: DEFAULT_ROUTE_SPEED,
            });
        }

        // Await the destination sensor of this hop.
        steps.push(RouteStep::AwaitSensor {
            train: train_id,
            sensor: hop.to,
            note: format!("train {} reaching sensor {}", train_id, hop.to),
        });

        prev_track = Some(current_track);
    }

    // De-energise the final track after the train arrives.
    if let Some(pt) = prev_track {
        steps.push(RouteStep::DeEnergiseTrack {
            train: train_id,
            track_id: pt,
        });
    }

    // Build description.
    let description = describe_route(&route, &point_settings, &track_ids, graph);
    let full_description = if route_warnings.is_empty() {
        description
    } else {
        format!("{}. WARNING: {}", description, route_warnings.join("; "))
    };

    Ok(TrainRoutePlan {
        train: train_id,
        from_sensor,
        to_sensor,
        target_direction: target_direction.to_string(),
        track_ids: track_ids.clone(),
        hop_count: route.hops.len(),
        description: full_description,
        steps,
        already_there: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::TrackLayout;
    use crate::state::TrainDirection;

    fn test_graph() -> TrackGraph {
        let path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
        let layout = TrackLayout::from_path(&path).expect("load");
        layout.validate().expect("validate");
        TrackGraph::from_layout(&layout)
    }

    #[test]
    fn plan_same_sensor_no_commands() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 5,
            direction: TrainDirection::default(),
            destination: Some(5),
        }];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].hop_count, 0);
        assert!(plans[0].commands.is_empty());
        assert!(plans[0].description.contains("already at"));
    }

    #[test]
    fn plan_adjacent_sensors() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 1,
            direction: TrainDirection::default(),
            destination: Some(2),
        }];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].from_sensor, 1);
        assert_eq!(plans[0].to_sensor, 2);
        assert_eq!(plans[0].hop_count, 1);
        // Should have at least one SetTrackSpeed command
        assert!(plans[0]
            .commands
            .iter()
            .any(|c| matches!(c, TrackCommand::SetTrackSpeed { .. })));
    }

    #[test]
    fn plan_route_to_siding_requires_points() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 10,
            direction: TrainDirection::default(),
            destination: Some(18),
        }];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        assert_eq!(plans.len(), 1);
        // Route to a siding should require point commands
        let point_cmds: Vec<_> = plans[0]
            .commands
            .iter()
            .filter(|c| matches!(c, TrackCommand::SetPoint { .. }))
            .collect();
        assert!(
            !point_cmds.is_empty(),
            "route to siding should require point settings"
        );
    }

    #[test]
    fn plan_cross_track_route() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 3,
            direction: TrainDirection::default(),
            destination: Some(4),
        }];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        assert_eq!(plans.len(), 1);
        // Should cross from track 1 to track 2
        assert!(!plans[0].track_ids.is_empty());
        assert!(plans[0].description.contains("Crosses"));
    }

    #[test]
    fn plan_multiple_trains() {
        let graph = test_graph();
        let trains = vec![
            TrainPosition {
                train: 1,
                sensor: 1,
                direction: TrainDirection::default(),
                destination: Some(5),
            },
            TrainPosition {
                train: 2,
                sensor: 10,
                direction: TrainDirection::default(),
                destination: Some(12),
            },
        ];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].train_index, 0);
        assert_eq!(plans[1].train_index, 1);
    }

    #[test]
    fn plan_no_destination_errors() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 1,
            direction: TrainDirection::default(),
            destination: None,
        }];
        let err = plan_routes_with_graph(&trains, &graph).unwrap_err();
        assert!(err.to_string().contains("no destination"));
    }

    #[test]
    fn plan_unreachable_sensor_errors() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 1,
            direction: TrainDirection::default(),
            destination: Some(99),
        }];
        let err = plan_routes_with_graph(&trains, &graph).unwrap_err();
        assert!(err.to_string().contains("no route"));
    }

    #[test]
    fn track_command_display() {
        let cmd = TrackCommand::SetPoint {
            point_id: 5,
            direction: PointDirection::Branch,
        };
        assert_eq!(cmd.to_string(), "SET POINT 5 → BRANCH");

        let cmd = TrackCommand::SetTrackSpeed {
            track_id: 3,
            direction: TrackDirection::Fwd,
            speed: 40,
        };
        assert_eq!(cmd.to_string(), "SET TRACK 3 → FWD @ 40%");

        let cmd = TrackCommand::StopTrack { track_id: 7 };
        assert_eq!(cmd.to_string(), "STOP TRACK 7");
    }

    #[test]
    fn track_command_serializes_to_json() {
        let cmd = TrackCommand::SetPoint {
            point_id: 5,
            direction: PointDirection::Thru,
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["command"], "set_point");
        assert_eq!(json["point_id"], 5);
        assert_eq!(json["direction"], "THRU");
    }

    #[test]
    fn planned_route_serializes() {
        let plan = PlannedRoute {
            train_index: 0,
            from_sensor: 1,
            to_sensor: 5,
            track_ids: vec![1, 2],
            hop_count: 4,
            description: "test".into(),
            commands: vec![TrackCommand::SetTrackSpeed {
                track_id: 1,
                direction: TrackDirection::Fwd,
                speed: 40,
            }],
        };
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["from_sensor"], 1);
        assert_eq!(json["to_sensor"], 5);
        assert!(json["commands"].is_array());
    }

    #[test]
    fn forward_route_uses_fwd_direction() {
        let graph = test_graph();
        // Sensor 1 → 2 is forward along track 1's along_fwd
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 1,
            direction: TrainDirection::default(),
            destination: Some(2),
        }];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        let track_cmds: Vec<_> = plans[0]
            .commands
            .iter()
            .filter_map(|c| match c {
                TrackCommand::SetTrackSpeed {
                    track_id,
                    direction,
                    ..
                } => Some((*track_id, *direction)),
                _ => None,
            })
            .collect();
        // Track 1 should be FWD (sensor 1 comes before sensor 2 in along_fwd)
        assert!(
            track_cmds
                .iter()
                .any(|(tid, dir)| *tid == 1 && *dir == TrackDirection::Fwd),
            "route 1→2 should use FWD on track 1, got: {:?}",
            track_cmds
        );
    }

    #[test]
    fn backward_route_uses_bck_direction() {
        let graph = test_graph();
        // Sensor 2 → 1 is backward along track 1's along_fwd
        let trains = vec![TrainPosition {
            train: 1,
            sensor: 2,
            direction: TrainDirection::default(),
            destination: Some(1),
        }];
        let plans = plan_routes_with_graph(&trains, &graph).unwrap();
        let track_cmds: Vec<_> = plans[0]
            .commands
            .iter()
            .filter_map(|c| match c {
                TrackCommand::SetTrackSpeed {
                    track_id,
                    direction,
                    ..
                } => Some((*track_id, *direction)),
                _ => None,
            })
            .collect();
        // Track 1 should be BCK (sensor 2 comes after sensor 1 in along_fwd)
        assert!(
            track_cmds
                .iter()
                .any(|(tid, dir)| *tid == 1 && *dir == TrackDirection::Bck),
            "route 2→1 should use BCK on track 1, got: {:?}",
            track_cmds
        );
    }

    // -----------------------------------------------------------------------
    // Target-based route planner tests
    // -----------------------------------------------------------------------

    fn make_train(train: u8, sensor: u8) -> crate::state::TrainPosition {
        crate::state::TrainPosition {
            train,
            sensor,
            direction: TrainDirection::Fwd,
            destination: None,
        }
    }

    fn make_train_dir(
        train: u8,
        sensor: u8,
        direction: TrainDirection,
    ) -> crate::state::TrainPosition {
        crate::state::TrainPosition {
            train,
            sensor,
            direction,
            destination: None,
        }
    }

    #[test]
    fn target_route_already_there() {
        let graph = test_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 5)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train(1, 5)],
        };
        let plan = plan_target_routes_with(&current, &target, &graph).unwrap();
        assert_eq!(plan.trains.len(), 1);
        assert!(plan.trains[0].already_there);
        assert!(plan.trains[0].steps.is_empty());
    }

    #[test]
    fn target_route_adjacent_sensor() {
        let graph = test_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train(1, 2)],
        };
        let plan = plan_target_routes_with(&current, &target, &graph).unwrap();
        assert_eq!(plan.trains.len(), 1);
        assert!(!plan.trains[0].already_there);
        assert_eq!(plan.trains[0].from_sensor, 1);
        assert_eq!(plan.trains[0].to_sensor, 2);

        // Should contain: set points (if any), energise track, await sensor, de-energise.
        let has_energise = plan.trains[0]
            .steps
            .iter()
            .any(|s| matches!(s, RouteStep::EnergiseTrack { .. }));
        let has_await = plan.trains[0]
            .steps
            .iter()
            .any(|s| matches!(s, RouteStep::AwaitSensor { .. }));
        let has_deenergise = plan.trains[0]
            .steps
            .iter()
            .any(|s| matches!(s, RouteStep::DeEnergiseTrack { .. }));
        assert!(has_energise, "should energise a track");
        assert!(has_await, "should await a sensor");
        assert!(has_deenergise, "should de-energise track after arrival");
    }

    #[test]
    fn target_route_train_not_found() {
        let graph = test_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train(99, 2)],
        };
        let err = plan_target_routes_with(&current, &target, &graph).unwrap_err();
        assert!(err.to_string().contains("train 99"));
    }

    #[test]
    fn target_route_cross_track() {
        let graph = test_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 3)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train(1, 4)],
        };
        let plan = plan_target_routes_with(&current, &target, &graph).unwrap();
        assert!(!plan.trains[0].already_there);
        // Route should exist and have at least one track.
        assert!(
            !plan.trains[0].track_ids.is_empty(),
            "route should use at least one track"
        );

        // Should de-energise each track after leaving it.
        let deenergise_count = plan.trains[0]
            .steps
            .iter()
            .filter(|s| matches!(s, RouteStep::DeEnergiseTrack { .. }))
            .count();
        assert!(deenergise_count >= 1, "should de-energise at least 1 track");
    }

    #[test]
    fn target_route_two_trains_reserves_tracks() {
        let graph = test_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1), make_train(2, 10)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train(1, 3), make_train(2, 12)],
        };
        let plan = plan_target_routes_with(&current, &target, &graph).unwrap();
        assert_eq!(plan.trains.len(), 2);
        assert_eq!(plan.trains[0].train, 1);
        assert_eq!(plan.trains[1].train, 2);
    }

    #[test]
    fn target_route_direction_warning() {
        let graph = test_graph();
        // Train at sensor 1, target sensor 2 with direction bwd.
        // Route 1→2 is forward on track 1, so arrival is fwd — but target wants bwd.
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train_dir(1, 2, TrainDirection::Bwd)],
        };
        let plan = plan_target_routes_with(&current, &target, &graph).unwrap();
        assert!(
            plan.trains[0].description.contains("WARNING"),
            "should warn about direction mismatch: {}",
            plan.trains[0].description
        );
    }

    #[test]
    fn target_route_step_display() {
        let step = RouteStep::SetPoint {
            train: 1,
            point_id: 5,
            direction: PointDirection::Branch,
        };
        assert_eq!(step.to_string(), "Train 1: SET POINT 5 → BRANCH");

        let step = RouteStep::EnergiseTrack {
            train: 2,
            track_id: 3,
            direction: TrackDirection::Fwd,
            speed: 40,
        };
        assert_eq!(step.to_string(), "Train 2: ENERGISE TRACK 3 → FWD @ 40%");

        let step = RouteStep::AwaitSensor {
            train: 1,
            sensor: 7,
            note: "arriving".into(),
        };
        assert_eq!(step.to_string(), "Train 1: AWAIT SENSOR 7 (arriving)");

        let step = RouteStep::DeEnergiseTrack {
            train: 1,
            track_id: 4,
        };
        assert_eq!(step.to_string(), "Train 1: DE-ENERGISE TRACK 4");
    }

    #[test]
    fn target_route_serializes_to_json() {
        let graph = test_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = InitialiseRequest {
            trains: vec![make_train(1, 2)],
        };
        let plan = plan_target_routes_with(&current, &target, &graph).unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        assert!(json["trains"].is_array());
        assert_eq!(json["trains"][0]["train"], 1);
        assert_eq!(json["trains"][0]["from_sensor"], 1);
        assert_eq!(json["trains"][0]["to_sensor"], 2);
        assert!(json["trains"][0]["steps"].is_array());
    }
}
