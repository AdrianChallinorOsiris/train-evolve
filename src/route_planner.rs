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
    #[error("unknown station \"{name}\" for train {train_id}")]
    UnknownStation { train_id: u8, name: String },
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
use crate::state::{Destination, InitialiseRequest, RouteRequest, TrainDirection};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// Station name at the departure sensor (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_station: Option<String>,
    /// Station name at the destination sensor (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_station: Option<String>,
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
/// `target` — the desired end state: each train has a destination (sensor or station) and direction.
///
/// Each train in `target` must have a matching `train` id in `current`.
pub fn plan_target_routes(
    current: &InitialiseRequest,
    target: &RouteRequest,
) -> Result<RoutePlan, PlanError> {
    let layout = load_and_validate_layout()?;
    let graph = TrackGraph::from_layout(&layout);
    plan_target_routes_with(current, target, &layout, &graph)
}

/// Plan target routes using a pre-built layout and graph (testable without I/O).
pub fn plan_target_routes_with(
    current: &InitialiseRequest,
    target: &RouteRequest,
    layout: &TrackLayout,
    graph: &TrackGraph,
) -> Result<RoutePlan, PlanError> {
    // Build current position lookup: train_id → TrainPosition
    let current_map: BTreeMap<u8, &crate::state::TrainPosition> =
        current.trains.iter().map(|t| (t.train, t)).collect();

    // Build station name → sensor list lookup.
    let station_map: HashMap<String, Vec<u8>> = layout
        .stations
        .iter()
        .map(|s| (s.name.to_lowercase(), s.sensor_ids.clone()))
        .collect();

    let mut train_plans = Vec::new();
    let mut warnings = Vec::new();
    // Track segments reserved by trains that have already been planned.
    let mut reserved_segments: BTreeSet<u8> = BTreeSet::new();
    // Sensors occupied by trains that have already been assigned destinations.
    let mut occupied_sensors: BTreeSet<u8> = current.trains.iter().map(|t| t.sensor).collect();

    for target_train in &target.trains {
        let cur = current_map
            .get(&target_train.train)
            .ok_or(PlanError::TrainNotFound {
                train_id: target_train.train,
            })?;

        // Resolve destination to a concrete sensor id.
        let to_sensor = resolve_destination(
            target_train.train,
            cur.sensor,
            &target_train.destination,
            target_train.direction,
            &station_map,
            &occupied_sensors,
            &reserved_segments,
            graph,
        )?;

        let plan = plan_single_train(
            target_train.train,
            cur.sensor,
            to_sensor,
            target_train.direction,
            &reserved_segments,
            graph,
            &station_map,
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
        // Mark destination sensor as occupied, free the old one.
        occupied_sensors.insert(to_sensor);
        if to_sensor != cur.sensor {
            occupied_sensors.remove(&cur.sensor);
        }

        train_plans.push(plan);
    }

    Ok(RoutePlan {
        trains: train_plans,
        warnings,
    })
}

/// Resolve a destination (sensor number or station name) to a concrete sensor id.
///
/// When the destination is a station name:
/// 1. Look up the station's sensor list
/// 2. Try each sensor that's not occupied and is reachable, preferring shorter routes
/// 3. Among reachable sensors, prefer ones whose arrival direction matches the target
#[allow(clippy::too_many_arguments)]
fn resolve_destination(
    train_id: u8,
    from_sensor: u8,
    destination: &Destination,
    target_direction: TrainDirection,
    station_map: &HashMap<String, Vec<u8>>,
    occupied: &BTreeSet<u8>,
    reserved: &BTreeSet<u8>,
    graph: &TrackGraph,
) -> Result<u8, PlanError> {
    match destination {
        Destination::Sensor(s) => Ok(*s),
        Destination::Station(name) => {
            let key = name.to_lowercase();
            let sensors = station_map
                .get(&key)
                .ok_or_else(|| PlanError::UnknownStation {
                    train_id,
                    name: name.clone(),
                })?;

            // Already at one of the station's sensors?
            if sensors.contains(&from_sensor) {
                return Ok(from_sensor);
            }

            // Score each candidate: (direction_match, hop_count, sensor_id).
            let mut candidates: Vec<(bool, usize, u8)> = Vec::new();
            for &ss in sensors {
                if occupied.contains(&ss) {
                    continue;
                }
                if let Some(route) = graph.find_route(from_sensor, ss) {
                    // Check route doesn't conflict with reserved segments.
                    let segments: BTreeSet<u8> = route.hops.iter().map(|h| h.track_id).collect();
                    if !segments.is_disjoint(reserved) {
                        continue;
                    }
                    let arrives_fwd = route
                        .hops
                        .last()
                        .map(|h| h.traverse_direction == TraverseDirection::Fwd)
                        .unwrap_or(true);
                    let target_fwd = target_direction == TrainDirection::Fwd;
                    let dir_match = arrives_fwd == target_fwd;
                    candidates.push((dir_match, route.hops.len(), ss));
                }
            }

            // Sort: prefer direction match, then shortest route.
            candidates.sort_by_key(|(dir_match, hops, _)| (!dir_match, *hops));

            if let Some((_, _, sensor)) = candidates.first() {
                return Ok(*sensor);
            }

            // All occupied or unreachable — try occupied sensors as a fallback
            // (train will get a "no route" if truly blocked).
            for &ss in sensors {
                if graph.find_route(from_sensor, ss).is_some() {
                    return Ok(ss);
                }
            }

            Err(PlanError::NoRoute {
                train_index: 0,
                from: from_sensor,
                to: *sensors.first().unwrap_or(&0),
            })
        }
    }
}

/// Find the station name for a sensor, if any.
fn station_for_sensor(sensor: u8, station_map: &HashMap<String, Vec<u8>>) -> Option<String> {
    for (name, sensors) in station_map {
        if sensors.contains(&sensor) {
            return Some(name.clone());
        }
    }
    None
}

/// Plan a single train's route from current sensor to target sensor.
fn plan_single_train(
    train_id: u8,
    from_sensor: u8,
    to_sensor: u8,
    target_direction: TrainDirection,
    reserved: &BTreeSet<u8>,
    graph: &TrackGraph,
    station_map: &HashMap<String, Vec<u8>>,
) -> Result<TrainRoutePlan, PlanError> {
    // Look up station names for source and destination
    let from_station = station_for_sensor(from_sensor, station_map);
    let to_station = station_for_sensor(to_sensor, station_map);
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
            from_station: from_station.clone(),
            to_station: to_station.clone(),
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
        from_station,
        to_station,
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

    fn test_layout_and_graph() -> (TrackLayout, TrackGraph) {
        let path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
        let layout = TrackLayout::from_path(&path).expect("load");
        layout.validate().expect("validate");
        let graph = TrackGraph::from_layout(&layout);
        (layout, graph)
    }

    fn make_train(train: u8, sensor: u8) -> crate::state::TrainPosition {
        crate::state::TrainPosition {
            train,
            sensor,
            direction: TrainDirection::Fwd,
            destination: None,
        }
    }

    fn make_route_train(
        train: u8,
        destination: crate::state::Destination,
        direction: TrainDirection,
    ) -> crate::state::RouteTrainRequest {
        crate::state::RouteTrainRequest {
            train,
            destination,
            direction,
        }
    }

    fn route_sensor(train: u8, sensor: u8) -> crate::state::RouteTrainRequest {
        make_route_train(
            train,
            crate::state::Destination::Sensor(sensor),
            TrainDirection::Fwd,
        )
    }

    fn route_sensor_dir(
        train: u8,
        sensor: u8,
        direction: TrainDirection,
    ) -> crate::state::RouteTrainRequest {
        make_route_train(train, crate::state::Destination::Sensor(sensor), direction)
    }

    fn route_station(train: u8, name: &str) -> crate::state::RouteTrainRequest {
        make_route_train(
            train,
            crate::state::Destination::Station(name.to_string()),
            TrainDirection::Fwd,
        )
    }

    #[test]
    fn target_route_already_there() {
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 5)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor(1, 5)],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert_eq!(plan.trains.len(), 1);
        assert!(plan.trains[0].already_there);
        assert!(plan.trains[0].steps.is_empty());
    }

    #[test]
    fn target_route_adjacent_sensor() {
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor(1, 2)],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
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
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor(99, 2)],
        };
        let err = plan_target_routes_with(&current, &target, &layout, &graph).unwrap_err();
        assert!(err.to_string().contains("train 99"));
    }

    #[test]
    fn target_route_cross_track() {
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 3)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor(1, 4)],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
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
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1), make_train(2, 10)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor(1, 3), route_sensor(2, 12)],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert_eq!(plan.trains.len(), 2);
        assert_eq!(plan.trains[0].train, 1);
        assert_eq!(plan.trains[1].train, 2);
    }

    #[test]
    fn target_route_direction_warning() {
        let (layout, graph) = test_layout_and_graph();
        // Train at sensor 1, target sensor 2 with direction bwd.
        // Route 1→2 is forward on track 1, so arrival is fwd — but target wants bwd.
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor_dir(1, 2, TrainDirection::Bwd)],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert!(
            plan.trains[0].description.contains("WARNING"),
            "should warn about direction mismatch: {}",
            plan.trains[0].description
        );
    }

    #[test]
    fn target_route_by_station_name() {
        let (layout, graph) = test_layout_and_graph();
        // Train at sensor 1, route to station "waterloo" which has sensors [2, 7, 12, 17].
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = RouteRequest {
            trains: vec![route_station(1, "waterloo")],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert_eq!(plan.trains.len(), 1);
        assert!(!plan.trains[0].already_there);
        let waterloo_sensors = [2u8, 7, 12, 17];
        assert!(
            waterloo_sensors.contains(&plan.trains[0].to_sensor),
            "should route to a waterloo station sensor, got {}",
            plan.trains[0].to_sensor
        );
    }

    #[test]
    fn target_route_unknown_station() {
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = RouteRequest {
            trains: vec![route_station(1, "nonexistent")],
        };
        let err = plan_target_routes_with(&current, &target, &layout, &graph).unwrap_err();
        assert!(
            err.to_string().contains("nonexistent"),
            "should mention station name: {}",
            err
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
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 1)],
        };
        let target = RouteRequest {
            trains: vec![route_sensor(1, 2)],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        assert!(json["trains"].is_array());
        assert_eq!(json["trains"][0]["train"], 1);
        assert_eq!(json["trains"][0]["from_sensor"], 1);
        assert_eq!(json["trains"][0]["to_sensor"], 2);
        assert!(json["trains"][0]["steps"].is_array());
    }

    // -----------------------------------------------------------------------
    // Tests for real initial positions (operator-provided scenarios)
    // -----------------------------------------------------------------------

    #[test]
    fn route_train_at_sensor_21_to_waterloo() {
        // Train 1 at sensor 21 (sidings) routes to waterloo station
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 21)],
        };
        let target = RouteRequest {
            trains: vec![route_station(1, "waterloo")],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert_eq!(plan.trains.len(), 1);
        assert!(!plan.trains[0].already_there);
        let waterloo_sensors = [2u8, 7, 12, 17];
        assert!(
            waterloo_sensors.contains(&plan.trains[0].to_sensor),
            "train 1 from sensor 21 should route to a waterloo sensor, got {}",
            plan.trains[0].to_sensor
        );
        assert!(!plan.trains[0].steps.is_empty(), "should have route steps");
    }

    #[test]
    fn route_two_trains_from_sidings() {
        // Train 1 at sensor 21, train 2 at sensor 20 (both in sidings)
        // Route them to different stations
        let (layout, graph) = test_layout_and_graph();
        let current = InitialiseRequest {
            trains: vec![make_train(1, 21), make_train(2, 20)],
        };
        let target = RouteRequest {
            trains: vec![route_station(1, "waterloo"), route_station(2, "bridge")],
        };
        let plan = plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert_eq!(plan.trains.len(), 2);

        let waterloo_sensors = [2u8, 7, 12, 17];
        let bridge_sensors = [5u8, 9, 16];
        assert!(
            waterloo_sensors.contains(&plan.trains[0].to_sensor),
            "train 1 should route to waterloo, got sensor {}",
            plan.trains[0].to_sensor
        );
        assert!(
            bridge_sensors.contains(&plan.trains[1].to_sensor),
            "train 2 should route to bridge, got sensor {}",
            plan.trains[1].to_sensor
        );
    }

    #[test]
    fn sensor_22_does_not_exist_in_graph() {
        // Sensor 22 is not in the layout — verify the graph confirms this
        let graph = test_graph();
        assert!(
            graph.find_route(22, 1).is_none(),
            "there should be no route FROM sensor 22 (it doesn't exist)"
        );
        assert!(
            graph.find_route(1, 22).is_none(),
            "there should be no route TO sensor 22 (it doesn't exist)"
        );
    }

    #[test]
    fn route_every_station_pair() {
        // Verify that we can route between every pair of stations
        let (layout, graph) = test_layout_and_graph();
        let stations = &layout.stations;
        for from_station in stations {
            for to_station in stations {
                if from_station.name == to_station.name {
                    continue;
                }
                let from_sensor = from_station.sensor_ids[0];
                let current = InitialiseRequest {
                    trains: vec![make_train(1, from_sensor)],
                };
                let target = RouteRequest {
                    trains: vec![route_station(1, &to_station.name)],
                };
                let result = plan_target_routes_with(&current, &target, &layout, &graph);
                assert!(
                    result.is_ok(),
                    "should find route from {} (sensor {}) to {}: {:?}",
                    from_station.name,
                    from_sensor,
                    to_station.name,
                    result.err()
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Example route files (operator-provided scenarios from data/example_routes/)
    // -----------------------------------------------------------------------

    /// An example route scenario loaded from a JSON file.
    #[derive(Debug, Deserialize)]
    struct ExampleRoute {
        description: String,
        current: InitialiseRequest,
        target: RouteRequest,
        expect: String,
    }

    #[test]
    fn example_route_files() {
        let dir = format!("{}/data/example_routes", env!("CARGO_MANIFEST_DIR"));
        let dir_path = std::path::Path::new(&dir);
        if !dir_path.exists() {
            return; // No example routes directory — skip
        }

        let (layout, graph) = test_layout_and_graph();
        let valid_sensors = layout.sensor_ids();
        let mut tested = 0;

        for entry in std::fs::read_dir(dir_path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let contents = std::fs::read_to_string(&path).unwrap();
            let example: ExampleRoute = serde_json::from_str(&contents).unwrap_or_else(|e| {
                panic!("failed to parse example route {}: {}", path.display(), e)
            });

            match example.expect.as_str() {
                "ok" => {
                    // Current state must have valid sensors
                    let sensor_valid = example
                        .current
                        .validate()
                        .and_then(|_| example.current.validate_against_layout(&valid_sensors));
                    assert!(
                        sensor_valid.is_ok(),
                        "[{}] {}: current state has invalid sensors: {}",
                        path.display(),
                        example.description,
                        sensor_valid.err().unwrap()
                    );

                    let result =
                        plan_target_routes_with(&example.current, &example.target, &layout, &graph);
                    assert!(
                        result.is_ok(),
                        "[{}] {}: expected OK but got error: {:?}",
                        path.display(),
                        example.description,
                        result.err()
                    );
                    let plan = result.unwrap();
                    for tp in &plan.trains {
                        assert!(
                            !tp.steps.is_empty() || tp.already_there,
                            "[{}] {}: train {} has no steps and isn't already_there",
                            path.display(),
                            example.description,
                            tp.train
                        );
                    }
                }
                "error" => {
                    // Either the sensor validation or the route planning should fail
                    let sensor_valid = example
                        .current
                        .validate()
                        .and_then(|_| example.current.validate_against_layout(&valid_sensors));
                    if sensor_valid.is_err() {
                        // Expected: invalid sensor in current state
                    } else {
                        let result = plan_target_routes_with(
                            &example.current,
                            &example.target,
                            &layout,
                            &graph,
                        );
                        assert!(
                            result.is_err(),
                            "[{}] {}: expected error but route planning succeeded",
                            path.display(),
                            example.description,
                        );
                    }
                }
                other => panic!(
                    "[{}] {}: unknown expect value '{}' — use 'ok' or 'error'",
                    path.display(),
                    example.description,
                    other
                ),
            }
            tested += 1;
        }

        assert!(tested > 0, "no example route JSON files found in {}", dir);
        eprintln!("tested {} example route file(s)", tested);
    }
}
