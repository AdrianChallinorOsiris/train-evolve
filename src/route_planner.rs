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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::TrackLayout;

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
            sensor: 5,
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
            sensor: 1,
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
            sensor: 10,
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
            sensor: 3,
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
                sensor: 1,
                destination: Some(5),
            },
            TrainPosition {
                sensor: 10,
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
            sensor: 1,
            destination: None,
        }];
        let err = plan_routes_with_graph(&trains, &graph).unwrap_err();
        assert!(err.to_string().contains("no destination"));
    }

    #[test]
    fn plan_unreachable_sensor_errors() {
        let graph = test_graph();
        let trains = vec![TrainPosition {
            sensor: 1,
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
            sensor: 1,
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
            sensor: 2,
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
}
