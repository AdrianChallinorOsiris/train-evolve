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
    #[error("unknown station \"{name}\" for train {train_id}")]
    UnknownStation { train_id: u8, name: String },
    #[error(
        "no sensor at station \"{station}\" reachable with arrival {arrival} for train {train_id}"
    )]
    NoSensorAtStation {
        train_id: u8,
        station: String,
        arrival: String,
    },
    #[error("station \"{station}\" fully occupied; no safe waiting position for train {train_id}")]
    StationFull { train_id: u8, station: String },
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
// Station-based route planning
// ---------------------------------------------------------------------------

use crate::layout::graph::TraverseDirection;
use crate::state::{ArrivalDirection, RouteRequest};
use std::collections::{BTreeSet, HashMap};

/// Result of planning a station route for one train.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationRouteResult {
    /// Train identifier from the request.
    pub train: u8,
    /// Starting sensor.
    pub from_sensor: u8,
    /// Station name.
    pub station: String,
    /// Requested arrival direction.
    pub arrival: String,
    /// The chosen destination sensor at the station (if routed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_sensor: Option<u8>,
    /// Whether the train is waiting (station full) vs routed.
    pub waiting: bool,
    /// If waiting, which sensor the train should wait at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_sensor: Option<u8>,
    /// Human-readable description.
    pub description: String,
    /// Planned route (if routed, not waiting).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<PlannedRoute>,
}

/// Plan station routes for all trains in a `RouteRequest`.
///
/// For each train:
/// 1. Look up the target station's sensors
/// 2. Pick a sensor that matches the arrival direction and isn't occupied
/// 3. If all sensors are occupied, find a safe waiting position
/// 4. Plan the route (or waiting move) avoiding conflicts with other trains
pub fn plan_station_routes(req: &RouteRequest) -> Result<Vec<StationRouteResult>, PlanError> {
    let layout = load_and_validate_layout()?;
    let graph = TrackGraph::from_layout(&layout);
    plan_station_routes_with(req, &layout, &graph)
}

/// Plan station routes using a pre-built layout and graph (testable without I/O).
pub fn plan_station_routes_with(
    req: &RouteRequest,
    layout: &TrackLayout,
    graph: &TrackGraph,
) -> Result<Vec<StationRouteResult>, PlanError> {
    // Build station name → sensor list lookup.
    let station_map: HashMap<String, Vec<u8>> = layout
        .stations
        .iter()
        .map(|s| (s.name.to_lowercase(), s.sensor_ids.clone()))
        .collect();

    // Track which sensors are occupied (by trains in this request).
    let mut occupied_sensors: BTreeSet<u8> = req.trains.iter().map(|t| t.sensor).collect();
    // Track which track segments are reserved by already-planned routes.
    let mut reserved_segments: BTreeSet<u8> = BTreeSet::new();

    let mut results = Vec::new();

    for train_req in &req.trains {
        let station_key = train_req.station.to_lowercase();
        let station_sensors =
            station_map
                .get(&station_key)
                .ok_or_else(|| PlanError::UnknownStation {
                    train_id: train_req.train,
                    name: train_req.station.clone(),
                })?;

        // Try to find an available sensor at the station matching the arrival direction.
        let result = pick_station_sensor_and_route(
            train_req,
            station_sensors,
            &occupied_sensors,
            &reserved_segments,
            graph,
            layout,
        );

        match result {
            StationPick::Routed { sensor, plan } => {
                // Reserve the track segments.
                for &tid in &plan.track_ids {
                    reserved_segments.insert(tid);
                }
                // Mark the destination sensor as occupied.
                occupied_sensors.insert(sensor);

                results.push(StationRouteResult {
                    train: train_req.train,
                    from_sensor: train_req.sensor,
                    station: train_req.station.clone(),
                    arrival: train_req.arrival.to_string(),
                    to_sensor: Some(sensor),
                    waiting: false,
                    wait_sensor: None,
                    description: plan.description.clone(),
                    route: Some(plan),
                });
                // If the train moved away from its starting sensor, free that sensor.
                if train_req.sensor != sensor {
                    occupied_sensors.remove(&train_req.sensor);
                }
            }
            StationPick::AlreadyAtStation { sensor } => {
                results.push(StationRouteResult {
                    train: train_req.train,
                    from_sensor: train_req.sensor,
                    station: train_req.station.clone(),
                    arrival: train_req.arrival.to_string(),
                    to_sensor: Some(sensor),
                    waiting: false,
                    wait_sensor: None,
                    description: format!(
                        "Train {} already at station {} (sensor {})",
                        train_req.train, train_req.station, sensor
                    ),
                    route: None,
                });
            }
            StationPick::Wait { wait_at, reason } => {
                results.push(StationRouteResult {
                    train: train_req.train,
                    from_sensor: train_req.sensor,
                    station: train_req.station.clone(),
                    arrival: train_req.arrival.to_string(),
                    to_sensor: None,
                    waiting: true,
                    wait_sensor: Some(wait_at),
                    description: reason,
                    route: None,
                });
            }
            StationPick::NoRoute => {
                return Err(PlanError::StationFull {
                    train_id: train_req.train,
                    station: train_req.station.clone(),
                });
            }
        }
    }

    Ok(results)
}

/// Internal result of trying to route a train to a station.
enum StationPick {
    /// Successfully routed to a specific sensor at the station.
    Routed { sensor: u8, plan: PlannedRoute },
    /// Train is already at the station.
    AlreadyAtStation { sensor: u8 },
    /// Station is full; train should wait at this sensor.
    Wait { wait_at: u8, reason: String },
    /// No route or waiting position could be found.
    NoRoute,
}

/// Try to pick a sensor at the station and plan a route.
///
/// Strategy:
/// 1. If the train is already on a station sensor, done.
/// 2. Find station sensors reachable with the correct arrival direction and not occupied.
/// 3. If none free, find a safe waiting position.
fn pick_station_sensor_and_route(
    train_req: &crate::state::RouteTrainRequest,
    station_sensors: &[u8],
    occupied: &BTreeSet<u8>,
    reserved: &BTreeSet<u8>,
    graph: &TrackGraph,
    layout: &TrackLayout,
) -> StationPick {
    // 1. Already at the station?
    if station_sensors.contains(&train_req.sensor) {
        return StationPick::AlreadyAtStation {
            sensor: train_req.sensor,
        };
    }

    // 2. Find available station sensors matching the arrival direction.
    let mut candidates: Vec<(u8, Route, PlannedRoute)> = Vec::new();
    for &ss in station_sensors {
        if occupied.contains(&ss) {
            continue;
        }
        if let Some(route) = graph.find_route(train_req.sensor, ss) {
            // Check arrival direction: the last hop's traverse_direction
            // tells us how the train arrives on the destination track.
            if !route.hops.is_empty() {
                let last_hop = route.hops.last().unwrap();
                let arrives_fwd = last_hop.traverse_direction == TraverseDirection::Fwd;
                let wanted_fwd = train_req.arrival == ArrivalDirection::Fwd;
                if arrives_fwd != wanted_fwd {
                    continue;
                }
            }
            // Check that route segments don't conflict with reserved.
            let route_segments: BTreeSet<u8> = route.hops.iter().map(|h| h.track_id).collect();
            if !route_segments.is_disjoint(reserved) {
                continue;
            }
            let plan = route_to_plan(0, &route, graph);
            candidates.push((ss, route, plan));
        }
    }

    // Prefer the shortest route.
    candidates.sort_by_key(|(_, route, _)| route.hops.len());

    if let Some((sensor, _route, plan)) = candidates.into_iter().next() {
        return StationPick::Routed { sensor, plan };
    }

    // 3. Station is full or no matching arrival direction available.
    //    Find a safe waiting position: a sensor on the route toward the station
    //    that doesn't block any existing train's exit path.
    find_safe_waiting_position(
        train_req,
        station_sensors,
        occupied,
        reserved,
        graph,
        layout,
    )
}

/// Find a safe sensor to wait at when the station is full.
///
/// We look for a sensor on the path toward the station that:
/// - Is not occupied
/// - Doesn't block the exit of any train currently at the station
///
/// For constrained stations like "industrial" (track 12, sensor 23),
/// the only exit is BWD from T12 → T2. We must not block track 2 or
/// place the waiting train on a sensor that prevents the occupant from leaving.
fn find_safe_waiting_position(
    train_req: &crate::state::RouteTrainRequest,
    station_sensors: &[u8],
    occupied: &BTreeSet<u8>,
    reserved: &BTreeSet<u8>,
    graph: &TrackGraph,
    _layout: &TrackLayout,
) -> StationPick {
    // Find exit tracks from each occupied station sensor.
    // An exit track is any track segment used by edges leaving the station sensor.
    let mut blocked_segments: BTreeSet<u8> = BTreeSet::new();
    for &ss in station_sensors {
        if !occupied.contains(&ss) {
            continue;
        }
        // All edges from this sensor = possible exit routes.
        if let Some(edges) = graph.edges.get(&ss) {
            for edge in edges {
                if edge.to != 0 {
                    blocked_segments.insert(edge.track_id);
                }
            }
        }
    }

    // Try to find a route to any station sensor (even occupied) and pick
    // an intermediate sensor that's safe to wait at.
    for &ss in station_sensors {
        if let Some(route) = graph.find_route(train_req.sensor, ss) {
            // Walk backward from the destination to find a safe waiting sensor.
            for hop in route.hops.iter().rev() {
                let wait_sensor = hop.from;
                if occupied.contains(&wait_sensor) {
                    continue;
                }
                // Don't wait on a segment that blocks an occupied train's exit.
                if blocked_segments.contains(&hop.track_id) {
                    continue;
                }
                // Don't conflict with already-reserved segments.
                if reserved.contains(&hop.track_id) {
                    continue;
                }
                return StationPick::Wait {
                    wait_at: wait_sensor,
                    reason: format!(
                        "Train {} waiting at sensor {} — station {} is full; \
                         will proceed when a platform clears",
                        train_req.train, wait_sensor, train_req.station
                    ),
                };
            }
        }
    }

    // Last resort: stay where you are if that doesn't block anything.
    if !blocked_segments.contains(graph.sensor_track.get(&train_req.sensor).unwrap_or(&0)) {
        return StationPick::Wait {
            wait_at: train_req.sensor,
            reason: format!(
                "Train {} holding at sensor {} — station {} is full",
                train_req.train, train_req.sensor, train_req.station
            ),
        };
    }

    StationPick::NoRoute
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
}
