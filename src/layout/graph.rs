//! Sensor-to-sensor adjacency graph built from [`TrackLayout`].
//!
//! The graph represents every possible **one-hop** movement between adjacent sensors on the
//! layout, annotated with the track segments traversed and the point settings required.
//! This is the foundation for pathfinding in Level 4.
//!
//! ## Terminology
//!
//! - **Hop**: movement from one sensor to an immediately adjacent sensor (no intermediate sensors).
//! - **PointSetting**: a point id + direction (Thru or Branch) needed for a hop.
//! - **Edge**: a hop plus metadata (tracks, points, whether a buffer terminates the path).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::layout::model::{RouteNode, TrackLayout, TrackSegment};
use crate::pi_client::PointDirection;

/// One edge in the sensor graph: a direct hop from one sensor to another (or to a dead end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorEdge {
    /// Source sensor id.
    pub from: u8,
    /// Destination sensor id (0 if dead end / buffer).
    pub to: u8,
    /// Track segment this edge lies on (the segment whose `along_fwd` contains both sensors,
    /// or the segment whose connection leads to the next segment).
    pub track_id: u8,
    /// Points that must be set for this hop to be valid.
    pub points: Vec<PointSetting>,
    /// True if this edge crosses a connection to a different track segment.
    pub crosses_connection: bool,
}

/// A point switch setting required for a hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PointSetting {
    pub point_id: u8,
    pub direction: PointDirection,
}

impl std::fmt::Display for PointSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "P{}={}", self.point_id, self.direction)
    }
}

/// The full sensor adjacency graph.
#[derive(Debug, Clone)]
pub struct TrackGraph {
    /// All sensor ids found in the layout.
    pub sensors: BTreeSet<u8>,
    /// Adjacency: sensor id → list of edges from that sensor.
    pub edges: BTreeMap<u8, Vec<SensorEdge>>,
    /// Sensor → track segment id it belongs to (primary).
    pub sensor_track: HashMap<u8, u8>,
}

/// A route from one sensor to another: sequence of hops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub from: u8,
    pub to: u8,
    /// Ordered hops (edges) from source to destination.
    pub hops: Vec<SensorEdge>,
}

impl Route {
    /// All point settings needed for this route (deduplicated).
    pub fn point_settings(&self) -> Vec<PointSetting> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for hop in &self.hops {
            for ps in &hop.points {
                if seen.insert((ps.point_id, ps.direction)) {
                    out.push(*ps);
                }
            }
        }
        out
    }

    /// All track segments involved in this route (deduplicated, ordered).
    pub fn track_ids(&self) -> Vec<u8> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for hop in &self.hops {
            if seen.insert(hop.track_id) {
                out.push(hop.track_id);
            }
        }
        out
    }
}

impl TrackGraph {
    /// Build a sensor graph from a validated [`TrackLayout`].
    ///
    /// Walks every track segment's `along_fwd` route tree, identifying sensors and the
    /// edges between them (including through points, across connections, and through couplers).
    pub fn from_layout(layout: &TrackLayout) -> Self {
        let mut sensors = BTreeSet::new();
        let mut edges: BTreeMap<u8, Vec<SensorEdge>> = BTreeMap::new();
        let mut sensor_track: HashMap<u8, u8> = HashMap::new();

        // First pass: collect all sensors and their owning tracks.
        for track in &layout.tracks {
            let track_sensors = collect_sensors_from_route(&track.along_fwd);
            for &s in &track_sensors {
                sensors.insert(s);
                sensor_track.insert(s, track.id);
            }
        }

        // Second pass: for each track, walk the route tree to find adjacent sensor pairs.
        for track in &layout.tracks {
            let track_edges = walk_track_edges(track, layout);
            for edge in track_edges {
                edges.entry(edge.from).or_default().push(edge);
            }
        }

        // Third pass: create cross-track edges through couplers.
        // A coupler appears on multiple tracks — find sensors adjacent to each coupler
        // occurrence and connect them.
        let coupler_edges = build_coupler_edges(layout);
        for edge in coupler_edges {
            edges.entry(edge.from).or_default().push(edge);
        }

        TrackGraph {
            sensors,
            edges,
            sensor_track,
        }
    }

    /// Find the shortest route (fewest sensor hops) from `from` to `to` using BFS.
    ///
    /// Returns `None` if no path exists (e.g. destination is behind a buffer with no way around).
    pub fn find_route(&self, from: u8, to: u8) -> Option<Route> {
        if from == to {
            return Some(Route {
                from,
                to,
                hops: vec![],
            });
        }
        if !self.sensors.contains(&from) || !self.sensors.contains(&to) {
            return None;
        }

        // BFS: state = (current sensor, path of edges taken)
        let mut visited: BTreeSet<u8> = BTreeSet::new();
        let mut queue: VecDeque<(u8, Vec<SensorEdge>)> = VecDeque::new();
        visited.insert(from);
        queue.push_back((from, vec![]));

        while let Some((current, path)) = queue.pop_front() {
            if let Some(neighbors) = self.edges.get(&current) {
                for edge in neighbors {
                    if edge.to == 0 {
                        continue; // dead end
                    }
                    if edge.to == to {
                        let mut hops = path.clone();
                        hops.push(edge.clone());
                        return Some(Route { from, to, hops });
                    }
                    if visited.insert(edge.to) {
                        let mut new_path = path.clone();
                        new_path.push(edge.clone());
                        queue.push_back((edge.to, new_path));
                    }
                }
            }
        }

        None // no path found
    }

    /// List all sensors that are reachable from a given sensor.
    pub fn reachable_from(&self, sensor: u8) -> BTreeSet<u8> {
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();
        visited.insert(sensor);
        queue.push_back(sensor);
        while let Some(current) = queue.pop_front() {
            if let Some(neighbors) = self.edges.get(&current) {
                for edge in neighbors {
                    if edge.to != 0 && visited.insert(edge.to) {
                        queue.push_back(edge.to);
                    }
                }
            }
        }
        visited.remove(&sensor);
        visited
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Collect all sensor ids from a route node list (recursive, preorder).
fn collect_sensors_from_route(nodes: &[RouteNode]) -> Vec<u8> {
    let mut out = Vec::new();
    for node in nodes {
        collect_sensors_recursive(node, &mut out);
    }
    out
}

fn collect_sensors_recursive(node: &RouteNode, out: &mut Vec<u8>) {
    match node {
        RouteNode::Sensor { id } => out.push(*id),
        RouteNode::Point {
            entry,
            thru,
            branch,
            ..
        } => {
            for n in &entry.along_fwd {
                collect_sensors_recursive(n, out);
            }
            for n in &thru.along_fwd {
                collect_sensors_recursive(n, out);
            }
            for n in &branch.along_fwd {
                collect_sensors_recursive(n, out);
            }
        }
        _ => {}
    }
}

/// Walk a track's route tree and produce all sensor-to-sensor edges.
///
/// This is a linearisation: we walk the `along_fwd` list, tracking the "last sensor seen"
/// and accumulating point settings and connection crossings. When we encounter a new sensor,
/// we emit an edge from the previous sensor to the new one (and vice versa for bidirectional
/// travel). Points create branches: each branch path is walked independently.
fn walk_track_edges(track: &TrackSegment, layout: &TrackLayout) -> Vec<SensorEdge> {
    let mut edges = Vec::new();
    let mut ctx = WalkContext {
        track_id: track.id,
        points: Vec::new(),
        crossed_connection: false,
        layout,
    };
    // We need to find pairs of adjacent sensors in the along_fwd spine.
    // Walk linearly, remembering the last sensor.
    walk_nodes(&track.along_fwd, &mut ctx, &mut edges, None);
    edges
}

struct WalkContext<'a> {
    track_id: u8,
    points: Vec<PointSetting>,
    crossed_connection: bool,
    layout: &'a TrackLayout,
}

/// Walk a list of route nodes, emitting edges between consecutive sensors.
/// Returns the last sensor seen (if any) so callers can chain.
fn walk_nodes(
    nodes: &[RouteNode],
    ctx: &mut WalkContext<'_>,
    edges: &mut Vec<SensorEdge>,
    last_sensor: Option<u8>,
) -> Option<u8> {
    let mut current_sensor = last_sensor;
    for node in nodes {
        current_sensor = walk_one_node(node, ctx, edges, current_sensor);
    }
    current_sensor
}

fn walk_one_node(
    node: &RouteNode,
    ctx: &mut WalkContext<'_>,
    edges: &mut Vec<SensorEdge>,
    last_sensor: Option<u8>,
) -> Option<u8> {
    match node {
        RouteNode::Sensor { id } => {
            if let Some(prev) = last_sensor {
                // Edge from prev → this sensor (forward)
                edges.push(SensorEdge {
                    from: prev,
                    to: *id,
                    track_id: ctx.track_id,
                    points: ctx.points.clone(),
                    crosses_connection: ctx.crossed_connection,
                });
                // Edge from this sensor → prev (backward)
                edges.push(SensorEdge {
                    from: *id,
                    to: prev,
                    track_id: ctx.track_id,
                    points: ctx.points.clone(),
                    crosses_connection: ctx.crossed_connection,
                });
                ctx.crossed_connection = false;
            }
            Some(*id)
        }
        RouteNode::Connection {
            peer_track,
            peer_side,
            ..
        } => {
            // A connection leads to another track. We link to the nearest reachable
            // sensors on the peer track, respecting points that gate the path.
            //
            // Connections are bidirectional: we emit edges in both directions so that
            // BFS can traverse the connection from either side.
            ctx.crossed_connection = true;
            if let Some(peer) = ctx.layout.tracks.iter().find(|t| t.id == *peer_track) {
                if let Some(prev) = last_sensor {
                    let targets = match peer_side {
                        crate::layout::model::TrackSide::Bwd => {
                            first_reachable_sensors(&peer.along_fwd, &[])
                        }
                        crate::layout::model::TrackSide::Fwd => {
                            last_reachable_sensors(&peer.along_fwd, &[])
                        }
                    };
                    for (target_sensor, pt_settings) in targets {
                        let mut pts = ctx.points.clone();
                        pts.extend(pt_settings);
                        // Forward: this track's sensor → peer track's sensor
                        edges.push(SensorEdge {
                            from: prev,
                            to: target_sensor,
                            track_id: ctx.track_id,
                            points: pts.clone(),
                            crosses_connection: true,
                        });
                        // Reverse: peer track's sensor → this track's sensor
                        edges.push(SensorEdge {
                            from: target_sensor,
                            to: prev,
                            track_id: ctx.track_id,
                            points: pts,
                            crosses_connection: true,
                        });
                    }
                }
            }
            last_sensor
        }
        RouteNode::Point {
            id,
            entry,
            thru,
            branch,
        } => {
            // Walk the entry leg first (inherits current context)
            let after_entry = walk_nodes(&entry.along_fwd, ctx, edges, last_sensor);

            // Thru leg: add point setting Thru
            let mut thru_points = ctx.points.clone();
            thru_points.push(PointSetting {
                point_id: *id,
                direction: PointDirection::Thru,
            });
            let saved_points = std::mem::replace(&mut ctx.points, thru_points);
            let saved_crossed = ctx.crossed_connection;
            walk_nodes(&thru.along_fwd, ctx, edges, after_entry);
            ctx.points = saved_points;
            ctx.crossed_connection = saved_crossed;

            // Branch leg: add point setting Branch
            let mut branch_points = ctx.points.clone();
            branch_points.push(PointSetting {
                point_id: *id,
                direction: PointDirection::Branch,
            });
            let saved_points = std::mem::replace(&mut ctx.points, branch_points);
            let saved_crossed = ctx.crossed_connection;
            walk_nodes(&branch.along_fwd, ctx, edges, after_entry);
            ctx.points = saved_points;
            ctx.crossed_connection = saved_crossed;

            // Return the sensor from the entry leg (the point doesn't create a new sensor)
            after_entry
        }
        RouteNode::Buffer => {
            // Dead end — emit edge to sensor 0 (dead end marker)
            if let Some(prev) = last_sensor {
                edges.push(SensorEdge {
                    from: prev,
                    to: 0,
                    track_id: ctx.track_id,
                    points: ctx.points.clone(),
                    crosses_connection: false,
                });
            }
            last_sensor
        }
        RouteNode::Coupler { .. } | RouteNode::Inline => {
            // Couplers are handled separately in build_coupler_edges(); inline is a no-op.
            last_sensor
        }
    }
}

/// Find all reachable "first sensors" when entering a route from the BWD (start) side.
/// Returns `(sensor_id, point_settings_needed)` for each possible first sensor.
/// Points create branches — both thru and branch paths are explored.
fn first_reachable_sensors(nodes: &[RouteNode], points_so_far: &[PointSetting]) -> Vec<(u8, Vec<PointSetting>)> {
    let mut results = Vec::new();
    for node in nodes {
        match node {
            RouteNode::Sensor { id } => {
                results.push((*id, points_so_far.to_vec()));
                return results; // Found the first sensor; stop.
            }
            RouteNode::Point { id, entry, thru, branch } => {
                // Walk entry leg first
                let entry_results = first_reachable_sensors(&entry.along_fwd, points_so_far);
                if !entry_results.is_empty() {
                    results.extend(entry_results);
                    return results;
                }
                // No sensor in entry leg — walk thru and branch
                let mut thru_pts = points_so_far.to_vec();
                thru_pts.push(PointSetting { point_id: *id, direction: PointDirection::Thru });
                results.extend(first_reachable_sensors(&thru.along_fwd, &thru_pts));

                let mut branch_pts = points_so_far.to_vec();
                branch_pts.push(PointSetting { point_id: *id, direction: PointDirection::Branch });
                results.extend(first_reachable_sensors(&branch.along_fwd, &branch_pts));
                return results;
            }
            // Skip non-sensor nodes
            _ => continue,
        }
    }
    results
}

/// Find all reachable "last sensors" when entering a route from the FWD (end) side.
/// Walks the route in reverse.
fn last_reachable_sensors(nodes: &[RouteNode], points_so_far: &[PointSetting]) -> Vec<(u8, Vec<PointSetting>)> {
    let mut results = Vec::new();
    for node in nodes.iter().rev() {
        match node {
            RouteNode::Sensor { id } => {
                results.push((*id, points_so_far.to_vec()));
                return results;
            }
            RouteNode::Point { id, entry, thru, branch } => {
                // Walking backwards: entry leg is the "exit" from the FWD side perspective.
                // We need to check thru and branch legs first (they connect to the FWD side),
                // then the entry leg.
                let mut thru_pts = points_so_far.to_vec();
                thru_pts.push(PointSetting { point_id: *id, direction: PointDirection::Thru });
                results.extend(last_reachable_sensors(&thru.along_fwd, &thru_pts));

                let mut branch_pts = points_so_far.to_vec();
                branch_pts.push(PointSetting { point_id: *id, direction: PointDirection::Branch });
                results.extend(last_reachable_sensors(&branch.along_fwd, &branch_pts));

                // Also check entry leg
                let entry_results = last_reachable_sensors(&entry.along_fwd, points_so_far);
                if !entry_results.is_empty() {
                    results.extend(entry_results);
                }
                return results;
            }
            _ => continue,
        }
    }
    results
}

/// Build edges that cross through couplers between different tracks.
///
/// A coupler with id `C` may appear in multiple tracks' route trees. For each pair of
/// occurrences on different tracks, we create bidirectional edges between the sensors
/// adjacent to each occurrence.
fn build_coupler_edges(layout: &TrackLayout) -> Vec<SensorEdge> {
    // For each coupler id, collect (track_id, preceding_sensor, following_sensor, point_context).
    let mut coupler_locations: HashMap<u8, Vec<CouplerLocation>> = HashMap::new();
    for track in &layout.tracks {
        find_coupler_locations(
            &track.along_fwd,
            track.id,
            &[],
            &mut coupler_locations,
        );
    }

    let mut edges = Vec::new();
    for locations in coupler_locations.values() {
        // Create edges between all pairs of locations on different tracks
        for (i, loc_a) in locations.iter().enumerate() {
            for loc_b in &locations[i + 1..] {
                if loc_a.track_id == loc_b.track_id {
                    continue;
                }
                // Connect sensors around loc_a to sensors around loc_b
                for &sa in loc_a.adjacent_sensors().iter() {
                    for &sb in loc_b.adjacent_sensors().iter() {
                        if sa != 0 && sb != 0 {
                            let mut pts = loc_a.points.clone();
                            pts.extend(&loc_b.points);
                            edges.push(SensorEdge {
                                from: sa,
                                to: sb,
                                track_id: loc_a.track_id,
                                points: pts.clone(),
                                crosses_connection: true,
                            });
                            edges.push(SensorEdge {
                                from: sb,
                                to: sa,
                                track_id: loc_b.track_id,
                                points: pts,
                                crosses_connection: true,
                            });
                        }
                    }
                }
            }
        }
    }
    edges
}

#[derive(Debug)]
struct CouplerLocation {
    track_id: u8,
    /// Sensor immediately before this coupler in the route (0 if none).
    before: u8,
    /// Sensor immediately after this coupler in the route (0 if none).
    after: u8,
    /// Point settings needed to reach this coupler.
    points: Vec<PointSetting>,
}

impl CouplerLocation {
    fn adjacent_sensors(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.before != 0 {
            out.push(self.before);
        }
        if self.after != 0 {
            out.push(self.after);
        }
        out
    }
}

/// Walk a route to find coupler positions and their adjacent sensors.
fn find_coupler_locations(
    nodes: &[RouteNode],
    track_id: u8,
    point_ctx: &[PointSetting],
    out: &mut HashMap<u8, Vec<CouplerLocation>>,
) {
    // Linear scan to find sensors before/after each coupler
    let flat = flatten_for_coupler_scan(nodes, point_ctx);
    let mut last_sensor: u8 = 0;
    for item in &flat {
        match item {
            FlatItem::Sensor(id) => {
                last_sensor = *id;
            }
            FlatItem::Coupler(id, pts) => {
                // Find the next sensor after this coupler
                let after = find_next_sensor_after_coupler(&flat, item);
                out.entry(*id).or_default().push(CouplerLocation {
                    track_id,
                    before: last_sensor,
                    after,
                    points: pts.clone(),
                });
            }
        }
    }
}

#[derive(Debug, Clone)]
enum FlatItem {
    Sensor(u8),
    Coupler(u8, Vec<PointSetting>),
}

/// Flatten a route into a sequence of sensors and couplers, expanding points recursively.
/// For points, we flatten all three legs (with appropriate point settings).
fn flatten_for_coupler_scan(nodes: &[RouteNode], point_ctx: &[PointSetting]) -> Vec<FlatItem> {
    let mut out = Vec::new();
    for node in nodes {
        flatten_one(node, point_ctx, &mut out);
    }
    out
}

fn flatten_one(node: &RouteNode, point_ctx: &[PointSetting], out: &mut Vec<FlatItem>) {
    match node {
        RouteNode::Sensor { id } => {
            out.push(FlatItem::Sensor(*id));
        }
        RouteNode::Coupler { id } => {
            out.push(FlatItem::Coupler(*id, point_ctx.to_vec()));
        }
        RouteNode::Point {
            id,
            entry,
            thru,
            branch,
        } => {
            // Flatten entry
            for n in &entry.along_fwd {
                flatten_one(n, point_ctx, out);
            }
            // Flatten thru with point setting
            let mut thru_pts = point_ctx.to_vec();
            thru_pts.push(PointSetting {
                point_id: *id,
                direction: PointDirection::Thru,
            });
            for n in &thru.along_fwd {
                flatten_one(n, &thru_pts, out);
            }
            // Flatten branch with point setting
            let mut branch_pts = point_ctx.to_vec();
            branch_pts.push(PointSetting {
                point_id: *id,
                direction: PointDirection::Branch,
            });
            for n in &branch.along_fwd {
                flatten_one(n, &branch_pts, out);
            }
        }
        _ => {}
    }
}

fn find_next_sensor_after_coupler(flat: &[FlatItem], target: &FlatItem) -> u8 {
    let mut found = false;
    for item in flat {
        if std::ptr::eq(item, target) {
            found = true;
            continue;
        }
        if found {
            if let FlatItem::Sensor(id) = item {
                return *id;
            }
        }
    }
    0
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::model::TrackLayout;

    fn canonical_layout() -> TrackLayout {
        let path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
        TrackLayout::from_path(&path).expect("load canonical layout")
    }

    fn canonical_graph() -> TrackGraph {
        let layout = canonical_layout();
        layout.validate().expect("validate");
        TrackGraph::from_layout(&layout)
    }

    #[test]
    fn graph_finds_all_sensors() {
        let graph = canonical_graph();
        // Layout has sensors 1-24 (with sensor 22 possibly missing — let's check)
        assert!(
            graph.sensors.len() >= 22,
            "expected at least 22 sensors, got {}",
            graph.sensors.len()
        );
        // Known sensors from the layout
        for s in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24] {
            assert!(
                graph.sensors.contains(&s),
                "missing sensor {s}"
            );
        }
    }

    #[test]
    fn graph_has_edges() {
        let graph = canonical_graph();
        let total_edges: usize = graph.edges.values().map(|v| v.len()).sum();
        assert!(
            total_edges > 20,
            "expected at least 20 edges, got {total_edges}"
        );
    }

    #[test]
    fn adjacent_sensors_on_track_1() {
        let graph = canonical_graph();
        // Track 1: sensors 1, 2, 3 in order
        // Sensor 1 should have an edge to sensor 2
        let edges_from_1 = graph.edges.get(&1).expect("sensor 1 should have edges");
        assert!(
            edges_from_1.iter().any(|e| e.to == 2),
            "sensor 1 should connect to sensor 2, edges: {edges_from_1:?}"
        );
        // Sensor 2 should connect to 1 and 3
        let edges_from_2 = graph.edges.get(&2).expect("sensor 2 should have edges");
        assert!(
            edges_from_2.iter().any(|e| e.to == 1),
            "sensor 2 should connect to sensor 1"
        );
        assert!(
            edges_from_2.iter().any(|e| e.to == 3),
            "sensor 2 should connect to sensor 3"
        );
    }

    #[test]
    fn route_same_sensor_is_empty() {
        let graph = canonical_graph();
        let route = graph.find_route(1, 1).expect("same sensor route");
        assert!(route.hops.is_empty());
    }

    #[test]
    fn route_adjacent_sensors() {
        let graph = canonical_graph();
        let route = graph.find_route(1, 2).expect("route 1→2 should exist");
        assert_eq!(route.hops.len(), 1, "adjacent sensors = 1 hop");
        assert_eq!(route.hops[0].from, 1);
        assert_eq!(route.hops[0].to, 2);
    }

    #[test]
    fn route_across_track_boundary() {
        let graph = canonical_graph();
        // Sensor 3 is the last sensor on track 1, sensor 4 is the first on track 2.
        // Connection 2 joins track 1 FWD end to track 2 BWD end, so 3→4 crosses it.
        let route = graph.find_route(3, 4);
        assert!(
            route.is_some(),
            "should find route from sensor 3 (track 1) to sensor 4 (track 2)"
        );
    }

    #[test]
    fn route_to_siding() {
        let graph = canonical_graph();
        // Sensor 18 is in sidings (track 11, behind points 8, 10, 11)
        let route = graph.find_route(10, 18);
        assert!(
            route.is_some(),
            "should find route from sensor 10 to siding sensor 18"
        );
        let route = route.unwrap();
        // This route should require some point settings
        let point_settings = route.point_settings();
        assert!(
            !point_settings.is_empty(),
            "route to siding should require point settings"
        );
    }

    #[test]
    fn reachable_from_sensor_1() {
        let graph = canonical_graph();
        let reachable = graph.reachable_from(1);
        // Sensor 1 is on the outer loop — should reach all other sensors
        assert!(
            reachable.len() >= 10,
            "sensor 1 should reach at least 10 other sensors, got {}",
            reachable.len()
        );
    }

    #[test]
    fn nonexistent_sensor_returns_none() {
        let graph = canonical_graph();
        assert!(graph.find_route(99, 1).is_none());
        assert!(graph.find_route(1, 99).is_none());
    }

    #[test]
    fn sensor_track_mapping() {
        let graph = canonical_graph();
        // Sensors 1, 2, 3 should be on track 1
        assert_eq!(graph.sensor_track.get(&1), Some(&1));
        assert_eq!(graph.sensor_track.get(&2), Some(&1));
        assert_eq!(graph.sensor_track.get(&3), Some(&1));
        // Sensors 4, 5 should be on track 2
        assert_eq!(graph.sensor_track.get(&4), Some(&2));
        assert_eq!(graph.sensor_track.get(&5), Some(&2));
    }

    #[test]
    fn point_setting_display() {
        let ps = PointSetting {
            point_id: 5,
            direction: PointDirection::Thru,
        };
        assert_eq!(ps.to_string(), "P5=THRU");
    }

    /// Minimal layout for isolated graph tests (no file dependency).
    #[test]
    fn minimal_two_track_graph() {
        let toml = r#"
version = 2

[[tracks]]
id = 1
along_fwd = [
  { kind = "sensor", id = 1 },
  { kind = "sensor", id = 2 },
  { kind = "connection", id = 1, peer_track = 2, peer_side = "bwd" },
]

[[tracks]]
id = 2
along_fwd = [
  { kind = "connection", id = 1, peer_track = 1, peer_side = "fwd" },
  { kind = "sensor", id = 3 },
  { kind = "sensor", id = 4 },
]
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        layout.validate().expect("validate");
        let graph = TrackGraph::from_layout(&layout);

        assert_eq!(graph.sensors.len(), 4);
        // 1→2 should be direct
        let route = graph.find_route(1, 2).expect("1→2");
        assert_eq!(route.hops.len(), 1);
        // 1→3 should cross the connection (2 hops: 1→2, 2→3)
        let route = graph.find_route(1, 3).expect("1→3");
        assert_eq!(route.hops.len(), 2);
        // 1→4 should be 3 hops: 1→2, 2→3, 3→4
        let route = graph.find_route(1, 4).expect("1→4");
        assert_eq!(route.hops.len(), 3);
    }

    /// Test point branching in graph.
    #[test]
    fn graph_with_point() {
        let toml = r#"
version = 2

[[tracks]]
id = 1
along_fwd = [
  { kind = "sensor", id = 1 },
  { kind = "point", id = 1, entry = [{ kind = "inline" }], thru = [{ kind = "sensor", id = 2 }], branch = [{ kind = "sensor", id = 3 }] },
]
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        layout.validate().expect("validate");
        let graph = TrackGraph::from_layout(&layout);

        assert_eq!(graph.sensors.len(), 3);
        // 1→2 requires point 1 = Thru
        let route = graph.find_route(1, 2).expect("1→2");
        assert_eq!(route.hops.len(), 1);
        assert!(route.point_settings().iter().any(|ps| ps.point_id == 1 && ps.direction == PointDirection::Thru));
        // 1→3 requires point 1 = Branch
        let route = graph.find_route(1, 3).expect("1→3");
        assert_eq!(route.hops.len(), 1);
        assert!(route.point_settings().iter().any(|ps| ps.point_id == 1 && ps.direction == PointDirection::Branch));
    }
}
