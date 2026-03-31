//! Boss-level train controller: continuous routing with collision avoidance and station stops.
//!
//! The [`TrainController`] manages the lifecycle of multiple trains on the layout:
//! 1. Load train positions from the initialise file
//! 2. Pick destinations for each train (cycling through stations)
//! 3. Plan routes using the graph (avoiding segments already reserved)
//! 4. Execute routes by sending commands to the Pi hardware
//! 5. Poll sensors to detect train arrival at destinations
//! 6. Dwell at stations for a configurable period
//! 7. Repeat
//!
//! Collision avoidance is segment-based: each train "reserves" the track segments
//! in its planned route. No other train can be routed through a reserved segment.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::layout::graph::TrackGraph;
use crate::layout::TrackLayout;
use crate::pi_client::{PiClient, PiError};
use crate::route_planner::{self, PlannedRoute, TrackCommand, DEFAULT_ROUTE_SPEED};
use crate::state::TrainPosition;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How long a train dwells at a station sensor (seconds).
pub const STATION_DWELL_SECS: u64 = 15;

/// How often the controller polls sensors (milliseconds).
pub const POLL_INTERVAL_MS: u64 = 500;

/// Speed for route execution (percentage).
pub const ROUTE_SPEED: u8 = DEFAULT_ROUTE_SPEED;

// ---------------------------------------------------------------------------
// Train state machine
// ---------------------------------------------------------------------------

/// The phase a single train is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrainPhase {
    /// Waiting for a route to be planned.
    Idle,
    /// Moving along a planned route toward `destination`.
    EnRoute {
        destination: u8,
        route: PlannedRoute,
    },
    /// Dwelling at a station sensor before picking a new destination.
    Dwelling {
        sensor: u8,
        station_name: String,
        until: Instant,
    },
}

/// Runtime state for one train.
#[derive(Debug, Clone)]
pub struct TrainState {
    /// Train index (0-based).
    pub index: usize,
    /// Current known sensor position.
    pub current_sensor: u8,
    /// Track segments reserved by this train's current route.
    pub reserved_segments: BTreeSet<u8>,
    /// Current phase.
    pub phase: TrainPhase,
    /// History of visited stations (to avoid repeating the same one).
    pub visited_stations: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Manages multiple trains with collision avoidance and station stops.
pub struct TrainController {
    pub trains: Vec<TrainState>,
    pub graph: TrackGraph,
    pub layout: TrackLayout,
    /// Sensor ID → station name.
    pub station_sensors: HashMap<u8, String>,
    /// All station sensor IDs.
    pub all_station_sensors: BTreeSet<u8>,
    /// All known sensor IDs in the layout.
    pub all_sensors: BTreeSet<u8>,
    /// Track usage counter: track_id → number of times a train has been routed through it.
    pub track_usage: HashMap<u8, u32>,
}

/// Errors from the controller.
#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("layout error: {0}")]
    Layout(String),
    #[error("Pi error: {0}")]
    Pi(#[from] PiError),
    #[error("route error: {0}")]
    Route(String),
}

// ---------------------------------------------------------------------------
// Observable status (serializable snapshot for /automatic/status)
// ---------------------------------------------------------------------------

/// Serializable snapshot of one train's state.
#[derive(Debug, Clone, Serialize)]
pub struct TrainSnapshot {
    pub index: usize,
    pub current_sensor: u8,
    pub phase: String,
    /// For `en_route`: the destination sensor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<u8>,
    /// For `dwelling`: the station name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    /// For `dwelling`: seconds remaining.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dwell_remaining_secs: Option<u64>,
    pub reserved_segments: Vec<u8>,
    pub visited_stations: Vec<u8>,
}

/// Serializable snapshot of the entire automatic controller.
#[derive(Debug, Clone, Serialize)]
pub struct AutomaticStatus {
    pub running: bool,
    pub trains: Vec<TrainSnapshot>,
    pub track_usage: HashMap<u8, u32>,
    pub tick_count: u64,
}

impl TrainController {
    /// Create a serializable status snapshot.
    pub fn snapshot(&self, tick_count: u64) -> AutomaticStatus {
        let trains = self
            .trains
            .iter()
            .map(|t| {
                let (phase_str, destination, station, dwell_remaining) = match &t.phase {
                    TrainPhase::Idle => ("idle".to_string(), None, None, None),
                    TrainPhase::EnRoute { destination, .. } => {
                        ("en_route".to_string(), Some(*destination), None, None)
                    }
                    TrainPhase::Dwelling {
                        station_name,
                        until,
                        ..
                    } => {
                        let remaining = until.saturating_duration_since(Instant::now());
                        (
                            "dwelling".to_string(),
                            None,
                            Some(station_name.clone()),
                            Some(remaining.as_secs()),
                        )
                    }
                };
                TrainSnapshot {
                    index: t.index,
                    current_sensor: t.current_sensor,
                    phase: phase_str,
                    destination,
                    station,
                    dwell_remaining_secs: dwell_remaining,
                    reserved_segments: t.reserved_segments.iter().copied().collect(),
                    visited_stations: t.visited_stations.clone(),
                }
            })
            .collect();

        AutomaticStatus {
            running: true,
            trains,
            track_usage: self.track_usage.clone(),
            tick_count,
        }
    }
}

impl TrainController {
    /// Create a new controller from train positions.
    pub fn new(positions: &[TrainPosition]) -> Result<Self, ControllerError> {
        let layout = load_layout()?;
        let graph = TrackGraph::from_layout(&layout);

        let mut station_sensors: HashMap<u8, String> = HashMap::new();
        let mut all_station_sensors = BTreeSet::new();
        for station in &layout.stations {
            for &sid in &station.sensor_ids {
                station_sensors.insert(sid, station.name.clone());
                all_station_sensors.insert(sid);
            }
        }

        let trains: Vec<TrainState> = positions
            .iter()
            .enumerate()
            .map(|(i, p)| TrainState {
                index: i,
                current_sensor: p.sensor,
                reserved_segments: BTreeSet::new(),
                phase: TrainPhase::Idle,
                visited_stations: Vec::new(),
            })
            .collect();

        let all_sensors = graph.sensors.clone();

        // Initialize track usage counters for all track segments
        let mut track_usage: HashMap<u8, u32> = HashMap::new();
        for track in &layout.tracks {
            track_usage.insert(track.id, 0);
        }

        Ok(Self {
            trains,
            graph,
            layout,
            station_sensors,
            all_station_sensors,
            all_sensors,
            track_usage,
        })
    }

    /// Segments reserved by all trains (union).
    pub fn all_reserved_segments(&self) -> BTreeSet<u8> {
        let mut reserved = BTreeSet::new();
        for train in &self.trains {
            reserved.extend(&train.reserved_segments);
        }
        reserved
    }

    /// Segments reserved by all trains except the given one.
    pub fn segments_reserved_by_others(&self, except_index: usize) -> BTreeSet<u8> {
        let mut reserved = BTreeSet::new();
        for train in &self.trains {
            if train.index != except_index {
                reserved.extend(&train.reserved_segments);
            }
        }
        reserved
    }

    /// Pick a destination for a train, avoiding sensors occupied by other trains
    /// and preferring routes through underused track segments and unvisited stations.
    pub fn pick_destination(&self, train_index: usize) -> Option<u8> {
        let train = &self.trains[train_index];
        let other_sensors: HashSet<u8> = self
            .trains
            .iter()
            .filter(|t| t.index != train_index)
            .map(|t| t.current_sensor)
            .collect();

        let reserved = self.segments_reserved_by_others(train_index);

        // Collect all candidate destinations (stations + non-stations for coverage)
        let candidates: Vec<u8> = self
            .all_sensors
            .iter()
            .copied()
            .filter(|&s| s != train.current_sensor)
            .filter(|s| !other_sensors.contains(s))
            .collect();

        // Score each candidate: lower = better
        // Priority: (1) station bonus, (2) least-recently-visited, (3) route uses underused tracks
        let mut best: Option<(u8, i64)> = None;
        for &candidate in &candidates {
            if let Some(route) = self.graph.find_route(train.current_sensor, candidate) {
                let route_segments: BTreeSet<u8> = route.hops.iter().map(|h| h.track_id).collect();
                // Skip if route conflicts with reserved segments
                if !route_segments.is_disjoint(&reserved) {
                    continue;
                }

                // Score: lower is better
                let mut score: i64 = 0;

                // Prefer station sensors (bonus -1000)
                if self.all_station_sensors.contains(&candidate) {
                    score -= 1000;
                }

                // Prefer sensors we haven't visited recently (bonus -500 for never visited)
                let visit_penalty = train
                    .visited_stations
                    .iter()
                    .rposition(|&v| v == candidate)
                    .map(|pos| (train.visited_stations.len() - pos) as i64)
                    .unwrap_or(-500);
                score += visit_penalty;

                // Prefer routes through underused track segments
                let route_usage: u32 = route_segments
                    .iter()
                    .map(|&tid| self.track_usage.get(&tid).copied().unwrap_or(0))
                    .sum();
                score += route_usage as i64;

                if best.is_none() || score < best.unwrap().1 {
                    best = Some((candidate, score));
                }
            }
        }

        best.map(|(s, _)| s)
    }

    /// Plan a route for a train to a destination, reserving segments.
    pub fn plan_route(
        &mut self,
        train_index: usize,
        destination: u8,
    ) -> Result<PlannedRoute, ControllerError> {
        let train = &self.trains[train_index];
        let trains_for_planner = vec![TrainPosition {
            train: (train_index + 1) as u8,
            sensor: train.current_sensor,
            direction: crate::state::TrainDirection::default(),
            destination: Some(destination),
        }];
        let plans = route_planner::plan_routes_with_graph(&trains_for_planner, &self.graph)
            .map_err(|e| ControllerError::Route(e.to_string()))?;
        let plan = plans
            .into_iter()
            .next()
            .ok_or_else(|| ControllerError::Route("no route planned".to_string()))?;

        // Reserve segments and track usage
        let segments: BTreeSet<u8> = plan.track_ids.iter().copied().collect();
        for &tid in &segments {
            *self.track_usage.entry(tid).or_insert(0) += 1;
        }
        self.trains[train_index].reserved_segments = segments;
        self.trains[train_index].phase = TrainPhase::EnRoute {
            destination,
            route: plan.clone(),
        };

        Ok(plan)
    }

    /// Execute a planned route's commands on the Pi hardware.
    pub async fn execute_route(pi: &PiClient, route: &PlannedRoute) -> Result<(), ControllerError> {
        for cmd in &route.commands {
            execute_command(pi, cmd).await?;
        }
        Ok(())
    }

    /// Stop all track segments used by a train's route.
    pub async fn stop_train_tracks(
        pi: &PiClient,
        route: &PlannedRoute,
    ) -> Result<(), ControllerError> {
        for &tid in &route.track_ids {
            pi.stop_track(tid).await?;
        }
        Ok(())
    }

    /// Handle a train arriving at its destination sensor.
    pub fn arrive(&mut self, train_index: usize, sensor: u8) {
        let is_station = self.station_sensors.contains_key(&sensor);
        let train = &mut self.trains[train_index];
        train.current_sensor = sensor;
        train.reserved_segments.clear();

        if is_station {
            let station_name = self.station_sensors[&sensor].clone();
            train.visited_stations.push(sensor);
            train.phase = TrainPhase::Dwelling {
                sensor,
                station_name,
                until: Instant::now() + Duration::from_secs(STATION_DWELL_SECS),
            };
        } else {
            train.phase = TrainPhase::Idle;
        }
    }

    /// Check if a dwelling train's dwell time has expired.
    pub fn check_dwell_complete(&mut self, train_index: usize) -> bool {
        let train = &self.trains[train_index];
        if let TrainPhase::Dwelling { until, .. } = &train.phase {
            if Instant::now() >= *until {
                self.trains[train_index].phase = TrainPhase::Idle;
                return true;
            }
        }
        false
    }

    /// Check if a second train needs to wait for an adjacent platform.
    /// Returns true if this train should keep dwelling because it's at a station
    /// that has an adjacent platform where another train is expected.
    pub fn should_wait_for_adjacent(&self, train_index: usize) -> bool {
        let train = &self.trains[train_index];
        let sensor = train.current_sensor;

        // Find which station this sensor belongs to
        if let Some(station_name) = self.station_sensors.get(&sensor) {
            // Find the station definition
            if let Some(station) = self
                .layout
                .stations
                .iter()
                .find(|s| &s.name == station_name)
            {
                // Check if any other train is en route to a sensor in this same station
                for other in &self.trains {
                    if other.index == train_index {
                        continue;
                    }
                    if let TrainPhase::EnRoute { destination, .. } = &other.phase {
                        if station.sensor_ids.contains(destination) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

/// Execute one track command on the Pi.
pub async fn execute_command(pi: &PiClient, cmd: &TrackCommand) -> Result<(), PiError> {
    match cmd {
        TrackCommand::SetPoint {
            point_id,
            direction,
        } => {
            pi.set_point(*point_id, *direction).await?;
        }
        TrackCommand::SetTrackSpeed {
            track_id,
            direction,
            speed,
        } => {
            pi.set_track_speed(*track_id, *direction, *speed).await?;
        }
        TrackCommand::StopTrack { track_id } => {
            pi.stop_track(*track_id).await?;
        }
    }
    Ok(())
}

/// Load and validate the canonical track layout.
fn load_layout() -> Result<TrackLayout, ControllerError> {
    let path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
    let layout =
        TrackLayout::from_path(&path).map_err(|e| ControllerError::Layout(e.to_string()))?;
    layout
        .validate()
        .map_err(|e| ControllerError::Layout(e.to_string()))?;
    Ok(layout)
}

// ---------------------------------------------------------------------------
// The main automatic control loop
// ---------------------------------------------------------------------------

/// Shared slot for observable status — updated by the automatic loop, read by the status endpoint.
pub type StatusSlot = Arc<tokio::sync::Mutex<Option<AutomaticStatus>>>;

/// Run the boss-level automatic control loop.
///
/// This continuously routes trains around the layout, avoiding collisions,
/// stopping at stations, and waiting for adjacent platform arrivals.
/// The `status_slot` is updated on every tick so the `/automatic/status` endpoint
/// can report current train positions.
pub async fn run_automatic(
    pi: Arc<PiClient>,
    initial_trains: Vec<TrainPosition>,
    cancel: Arc<AtomicBool>,
    status_slot: StatusSlot,
) -> Result<(), ControllerError> {
    let mut controller = TrainController::new(&initial_trains)?;
    let mut tick_count: u64 = 0;

    eprintln!(
        "yoyo: automatic mode started with {} train(s), {} station sensors",
        controller.trains.len(),
        controller.all_station_sensors.len()
    );

    loop {
        if cancel.load(Ordering::SeqCst) {
            // Emergency stop all tracks before exiting
            if let Err(e) = pi.all_stop().await {
                eprintln!("yoyo: all_stop on cancel failed: {e}");
            }
            break;
        }

        // Poll sensors once per tick (shared across all trains).
        // Only needed when at least one train is en route.
        let has_en_route = controller
            .trains
            .iter()
            .any(|t| matches!(t.phase, TrainPhase::EnRoute { .. }));
        let sensor_snapshot = if has_en_route {
            match pi.sensors().await {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("yoyo: sensor poll error: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Process each train
        for i in 0..controller.trains.len() {
            if cancel.load(Ordering::SeqCst) {
                break;
            }

            let phase = controller.trains[i].phase.clone();
            match phase {
                TrainPhase::Idle => {
                    // Pick a destination and start moving
                    if let Some(dest) = controller.pick_destination(i) {
                        match controller.plan_route(i, dest) {
                            Ok(route) => {
                                let sensor = controller.trains[i].current_sensor;
                                let station = controller
                                    .station_sensors
                                    .get(&dest)
                                    .map(|s| format!(" ({})", s))
                                    .unwrap_or_default();
                                eprintln!(
                                    "yoyo: train {} departing S{} → S{}{}: {}",
                                    i, sensor, dest, station, route.description
                                );
                                if let Err(e) = TrainController::execute_route(&pi, &route).await {
                                    eprintln!("yoyo: train {} execute error: {e}", i);
                                    // Revert to idle
                                    controller.trains[i].reserved_segments.clear();
                                    controller.trains[i].phase = TrainPhase::Idle;
                                }
                            }
                            Err(e) => {
                                eprintln!("yoyo: train {} route planning error: {e}", i);
                            }
                        }
                    } else if tick_count.is_multiple_of(20) {
                        // Log periodically so the operator sees why a train is stuck
                        let sensor = controller.trains[i].current_sensor;
                        let reserved = controller.segments_reserved_by_others(i);
                        eprintln!(
                            "yoyo: train {} at S{} idle — no reachable destination (reserved by others: {:?})",
                            i, sensor, reserved
                        );
                    }
                }
                TrainPhase::EnRoute { destination, route } => {
                    // Check the pre-fetched sensor snapshot for arrival
                    let arrived = sensor_snapshot
                        .as_ref()
                        .and_then(|snap| snap.get(&destination.to_string()))
                        .map(|s| s.value)
                        .unwrap_or(false);
                    if arrived {
                        eprintln!("yoyo: train {} arrived at S{}", i, destination);
                        // Stop the tracks used by this route
                        if let Err(e) = TrainController::stop_train_tracks(&pi, &route).await {
                            eprintln!("yoyo: train {} stop tracks error: {e}", i);
                        }
                        controller.arrive(i, destination);
                    }
                }
                TrainPhase::Dwelling {
                    sensor,
                    station_name,
                    until,
                } => {
                    // Check if we should keep waiting for an adjacent train
                    if controller.should_wait_for_adjacent(i) {
                        // Extend dwell — don't transition yet
                        continue;
                    }
                    if controller.check_dwell_complete(i) {
                        eprintln!(
                            "yoyo: train {} finished dwelling at S{} ({})",
                            i, sensor, station_name
                        );
                    } else {
                        let remaining = until.saturating_duration_since(Instant::now());
                        if remaining.as_secs() > 0 && remaining.as_secs() % 5 == 0 {
                            // Log every ~5 seconds
                            eprintln!(
                                "yoyo: train {} dwelling at {} (S{}) — {}s remaining",
                                i,
                                station_name,
                                sensor,
                                remaining.as_secs()
                            );
                        }
                    }
                }
            }
        }

        // Update the observable status snapshot
        tick_count += 1;
        {
            let snapshot = controller.snapshot(tick_count);
            *status_slot.lock().await = Some(snapshot);
        }

        // Sleep before next poll cycle
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }

    // Clear status on exit
    *status_slot.lock().await = None;
    eprintln!("yoyo: automatic mode stopped");
    Ok(())
}



#[cfg(test)]
mod tests {
    use super::*;

    fn test_positions() -> Vec<TrainPosition> {
        vec![
            TrainPosition {
                train: 1,
                sensor: 1,
                direction: crate::state::TrainDirection::default(),
                destination: None,
            },
            TrainPosition {
                train: 2,
                sensor: 10,
                direction: crate::state::TrainDirection::default(),
                destination: None,
            },
        ]
    }

    #[test]
    fn controller_creates_from_positions() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        assert_eq!(ctrl.trains.len(), 2);
        assert_eq!(ctrl.trains[0].current_sensor, 1);
        assert_eq!(ctrl.trains[1].current_sensor, 10);
        assert!(matches!(ctrl.trains[0].phase, TrainPhase::Idle));
    }

    #[test]
    fn station_sensors_loaded() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        // waterloo has sensors 2, 7, 12, 17
        assert_eq!(ctrl.station_sensors.get(&2), Some(&"waterloo".to_string()));
        assert_eq!(ctrl.station_sensors.get(&7), Some(&"waterloo".to_string()));
        // sidings has sensors 18, 19, 20, 21
        assert_eq!(ctrl.station_sensors.get(&18), Some(&"sidings".to_string()));
        // blackheath has sensor 24
        assert_eq!(
            ctrl.station_sensors.get(&24),
            Some(&"blackheath".to_string())
        );
    }

    #[test]
    fn pick_destination_avoids_other_trains() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        // Train 0 at sensor 1, train 1 at sensor 10
        // Train 0's destination should not be sensor 10
        let dest = ctrl.pick_destination(0);
        assert!(dest.is_some());
        assert_ne!(dest.unwrap(), 10);
        assert_ne!(dest.unwrap(), 1); // not its own position
    }

    #[test]
    fn pick_destination_prefers_stations() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        let dest = ctrl.pick_destination(0);
        assert!(dest.is_some());
        // Should be a station sensor
        assert!(
            ctrl.all_station_sensors.contains(&dest.unwrap()),
            "destination {} should be a station sensor",
            dest.unwrap()
        );
    }

    #[test]
    fn plan_route_reserves_segments() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        let dest = ctrl.pick_destination(0).unwrap();
        let route = ctrl.plan_route(0, dest).unwrap();
        assert!(!route.track_ids.is_empty() || route.hop_count == 0);
        // Segments should be reserved
        if !route.track_ids.is_empty() {
            assert!(!ctrl.trains[0].reserved_segments.is_empty());
        }
    }

    #[test]
    fn collision_avoidance_reserves_disjoint() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        // Plan route for train 0
        let dest0 = ctrl.pick_destination(0).unwrap();
        let _route0 = ctrl.plan_route(0, dest0).unwrap();
        let reserved0 = ctrl.trains[0].reserved_segments.clone();

        // Plan route for train 1
        let dest1 = ctrl.pick_destination(1).unwrap();
        let _route1 = ctrl.plan_route(1, dest1).unwrap();
        let reserved1 = ctrl.trains[1].reserved_segments.clone();

        // The reserved segments should be disjoint (collision avoidance)
        assert!(
            reserved0.is_disjoint(&reserved1),
            "reserved segments should not overlap: train 0 = {:?}, train 1 = {:?}",
            reserved0,
            reserved1
        );
    }

    #[test]
    fn arrive_at_station_starts_dwelling() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        // Simulate train 0 arriving at sensor 2 (waterloo station)
        ctrl.arrive(0, 2);
        assert_eq!(ctrl.trains[0].current_sensor, 2);
        assert!(matches!(ctrl.trains[0].phase, TrainPhase::Dwelling { .. }));
        assert!(ctrl.trains[0].visited_stations.contains(&2));
    }

    #[test]
    fn arrive_at_non_station_goes_idle() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        // Sensor 3 is not a station
        ctrl.arrive(0, 3);
        assert_eq!(ctrl.trains[0].current_sensor, 3);
        assert!(matches!(ctrl.trains[0].phase, TrainPhase::Idle));
    }

    #[test]
    fn dwell_check_not_expired() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        ctrl.arrive(0, 2); // waterloo
                           // Should not be complete immediately
        assert!(!ctrl.check_dwell_complete(0));
    }

    #[test]
    fn dwell_check_expired() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        // Manually set dwelling with past expiry
        ctrl.trains[0].phase = TrainPhase::Dwelling {
            sensor: 2,
            station_name: "waterloo".into(),
            until: Instant::now() - Duration::from_secs(1),
        };
        assert!(ctrl.check_dwell_complete(0));
        assert!(matches!(ctrl.trains[0].phase, TrainPhase::Idle));
    }

    #[test]
    fn should_wait_for_adjacent_when_en_route() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        // Train 0 dwelling at waterloo (sensor 2)
        ctrl.arrive(0, 2);

        // Train 1 en route to waterloo (sensor 7 — same station)
        ctrl.trains[1].phase = TrainPhase::EnRoute {
            destination: 7,
            route: PlannedRoute {
                train_index: 1,
                from_sensor: 10,
                to_sensor: 7,
                track_ids: vec![5],
                hop_count: 1,
                description: "test".into(),
                commands: vec![],
            },
        };

        // Train 0 should wait for train 1
        assert!(ctrl.should_wait_for_adjacent(0));
    }

    #[test]
    fn all_reserved_segments_union() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        ctrl.trains[0].reserved_segments.insert(1);
        ctrl.trains[0].reserved_segments.insert(2);
        ctrl.trains[1].reserved_segments.insert(3);
        let all = ctrl.all_reserved_segments();
        assert_eq!(all, BTreeSet::from([1, 2, 3]));
    }

    #[test]
    fn track_usage_increments_on_plan() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        // All track usage should start at 0
        for &count in ctrl.track_usage.values() {
            assert_eq!(count, 0);
        }
        let dest = ctrl.pick_destination(0).unwrap();
        let route = ctrl.plan_route(0, dest).unwrap();
        // Track usage should be incremented for tracks in the route
        for &tid in &route.track_ids {
            assert!(
                *ctrl.track_usage.get(&tid).unwrap_or(&0) > 0,
                "track {tid} should have usage > 0 after planning"
            );
        }
    }

    #[test]
    fn track_usage_initialized_for_all_tracks() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        // Should have entries for all 12 tracks
        assert!(ctrl.track_usage.len() >= 12);
    }

    #[test]
    fn snapshot_idle_trains() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        let snap = ctrl.snapshot(0);
        assert!(snap.running);
        assert_eq!(snap.trains.len(), 2);
        assert_eq!(snap.trains[0].phase, "idle");
        assert_eq!(snap.trains[0].current_sensor, 1);
        assert_eq!(snap.trains[1].current_sensor, 10);
        assert_eq!(snap.tick_count, 0);
    }

    #[test]
    fn snapshot_dwelling_train() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        ctrl.arrive(0, 2); // waterloo
        let snap = ctrl.snapshot(42);
        assert_eq!(snap.trains[0].phase, "dwelling");
        assert_eq!(snap.trains[0].station.as_deref(), Some("waterloo"));
        assert!(snap.trains[0].dwell_remaining_secs.is_some());
        assert_eq!(snap.tick_count, 42);
    }

    #[test]
    fn snapshot_en_route_train() {
        let mut ctrl = TrainController::new(&test_positions()).unwrap();
        let dest = ctrl.pick_destination(0).unwrap();
        let _route = ctrl.plan_route(0, dest).unwrap();
        let snap = ctrl.snapshot(1);
        assert_eq!(snap.trains[0].phase, "en_route");
        assert_eq!(snap.trains[0].destination, Some(dest));
        assert!(!snap.trains[0].reserved_segments.is_empty());
    }

    #[test]
    fn snapshot_serializes_to_json() {
        let ctrl = TrainController::new(&test_positions()).unwrap();
        let snap = ctrl.snapshot(5);
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["running"], true);
        assert!(json["trains"].is_array());
        assert_eq!(json["tick_count"], 5);
    }
}
