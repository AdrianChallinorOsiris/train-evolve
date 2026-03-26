//! Validation rules for [`TrackLayout`](crate::layout::model::TrackLayout).

use std::collections::{HashMap, HashSet};

use crate::layout::ids::{PointId, SensorId, TrackId};
use crate::layout::model::{ConnectionRef, CouplerDef, RouteNode, TrackLayout, TrackSide};

/// Layout validation error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("unsupported layout version: {0} (expected 2)")]
    UnsupportedVersion(u32),
    #[error("no track segments defined")]
    NoTracks,
    #[error("duplicate track id: {0}")]
    DuplicateTrackId(u8),
    #[error("invalid track id {0}: must be {}..={}", TrackId::MIN, TrackId::MAX)]
    InvalidTrackId(u8),
    #[error("duplicate sensor id {0} across tracks")]
    DuplicateSensorId(u8),
    #[error("invalid sensor id {0}: must be {}..={}", SensorId::MIN, SensorId::MAX)]
    InvalidSensorId(u8),
    #[error("track {track} lists point id {point} more than once in its route tree")]
    DuplicatePointOnTrack { track: u8, point: u8 },
    #[error("track {track} lists coupler id {coupler} more than once in its route tree")]
    DuplicateCouplerOnTrack { track: u8, coupler: u8 },
    #[error("track {track} references unknown coupler id {coupler}")]
    UnknownCouplerOnTrack { track: u8, coupler: u8 },
    #[error("duplicate coupler id in couplers list: {0}")]
    DuplicateCouplerId(u8),
    #[error("invalid point id {0}: must be {}..={}", PointId::MIN, PointId::MAX)]
    InvalidPointId(u8),
    #[error("invalid coupler id {0}: must be {}..={}", PointId::MIN, PointId::MAX)]
    InvalidCouplerId(u8),
    #[error("id {0} is used as both a point and a coupler on the same layout")]
    IdPointAndCoupler(u8),
    #[error("connection references unknown peer track {0}")]
    UnknownPeerTrack(u8),
    #[error("connection references unknown track id {0}")]
    UnknownTrackRef(u8),
    #[error("connection id {id}: expected exactly 2 endpoints, found {count}")]
    ConnectionEndpointCount { id: u8, count: usize },
    #[error("connection id {id}: endpoints are not reciprocal between the two tracks")]
    ConnectionNotReciprocal { id: u8 },
    #[error("connection references unknown coupler id {0}")]
    UnknownCouplerRef(u8),
    #[error("station {name:?} references unknown sensor id {sensor}")]
    UnknownStationSensor { name: String, sensor: u8 },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ConnEnd {
    this_track: u8,
    peer_track: u8,
    #[allow(dead_code)]
    peer_side: TrackSide,
}

impl TrackLayout {
    /// Validate numeric ranges, uniqueness, connection pairing, and reference integrity.
    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.version != 2 {
            return Err(LayoutError::UnsupportedVersion(self.version));
        }
        if self.tracks.is_empty() {
            return Err(LayoutError::NoTracks);
        }

        let mut track_ids = HashSet::new();
        for t in &self.tracks {
            if !track_ids.insert(t.id) {
                return Err(LayoutError::DuplicateTrackId(t.id));
            }
            TrackId::try_new(t.id).ok_or(LayoutError::InvalidTrackId(t.id))?;
        }

        let mut coupler_ids = HashSet::new();
        for c in &self.couplers {
            PointId::try_new(c.id).ok_or(LayoutError::InvalidCouplerId(c.id))?;
            if !coupler_ids.insert(c.id) {
                return Err(LayoutError::DuplicateCouplerId(c.id));
            }
        }

        let mut sensors_seen = HashSet::new();
        let mut point_ids_layout = HashSet::new();
        let mut coupler_ids_layout = HashSet::new();

        for t in &self.tracks {
            let mut points_here = HashSet::new();
            let mut couplers_here = HashSet::new();
            for n in &t.along_fwd {
                collect_route_ids(
                    n,
                    t.id,
                    &mut sensors_seen,
                    &mut points_here,
                    &mut couplers_here,
                    &mut point_ids_layout,
                    &mut coupler_ids_layout,
                    &coupler_ids,
                )?;
                validate_connection_peers(n, &track_ids)?;
            }
        }

        if let Some(id) = point_ids_layout.intersection(&coupler_ids_layout).next() {
            return Err(LayoutError::IdPointAndCoupler(*id));
        }

        let mut by_conn: HashMap<u8, Vec<ConnEnd>> = HashMap::new();
        for t in &self.tracks {
            for n in &t.along_fwd {
                collect_connections(t.id, n, &mut by_conn);
            }
        }

        validate_connection_groups(&by_conn, &track_ids)?;

        let track_id_set: HashSet<u8> = self.tracks.iter().map(|t| t.id).collect();
        let coupler_id_set: HashSet<u8> = self.couplers.iter().map(|c| c.id).collect();
        for c in &self.couplers {
            for r in coupler_connection_refs(c) {
                validate_coupler_connection_ref(&track_id_set, &coupler_id_set, r)?;
            }
        }

        for station in &self.stations {
            for &s in &station.sensor_ids {
                SensorId::try_new(s).ok_or(LayoutError::InvalidSensorId(s))?;
                if !sensors_seen.contains(&s) {
                    return Err(LayoutError::UnknownStationSensor {
                        name: station.name.clone(),
                        sensor: s,
                    });
                }
            }
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_route_ids(
    node: &RouteNode,
    track_id: u8,
    sensors_seen: &mut HashSet<u8>,
    points_here: &mut HashSet<u8>,
    couplers_here: &mut HashSet<u8>,
    point_ids_layout: &mut HashSet<u8>,
    coupler_ids_layout: &mut HashSet<u8>,
    coupler_table: &HashSet<u8>,
) -> Result<(), LayoutError> {
    match node {
        RouteNode::Sensor { id } => {
            SensorId::try_new(*id).ok_or(LayoutError::InvalidSensorId(*id))?;
            if !sensors_seen.insert(*id) {
                return Err(LayoutError::DuplicateSensorId(*id));
            }
        }
        RouteNode::Coupler { id } => {
            PointId::try_new(*id).ok_or(LayoutError::InvalidCouplerId(*id))?;
            if !coupler_table.contains(id) {
                return Err(LayoutError::UnknownCouplerOnTrack {
                    track: track_id,
                    coupler: *id,
                });
            }
            if !couplers_here.insert(*id) {
                return Err(LayoutError::DuplicateCouplerOnTrack {
                    track: track_id,
                    coupler: *id,
                });
            }
            coupler_ids_layout.insert(*id);
        }
        RouteNode::Point {
            id,
            entry,
            thru,
            branch,
        } => {
            PointId::try_new(*id).ok_or(LayoutError::InvalidPointId(*id))?;
            if !points_here.insert(*id) {
                return Err(LayoutError::DuplicatePointOnTrack {
                    track: track_id,
                    point: *id,
                });
            }
            point_ids_layout.insert(*id);
            for leg in [&entry.along_fwd, &thru.along_fwd, &branch.along_fwd] {
                for n in leg {
                    collect_route_ids(
                        n,
                        track_id,
                        sensors_seen,
                        points_here,
                        couplers_here,
                        point_ids_layout,
                        coupler_ids_layout,
                        coupler_table,
                    )?;
                }
            }
        }
        RouteNode::Connection { peer_track, .. } => {
            TrackId::try_new(*peer_track).ok_or(LayoutError::UnknownPeerTrack(*peer_track))?;
        }
        RouteNode::Buffer | RouteNode::Inline => {}
    }
    Ok(())
}

fn validate_connection_peers(node: &RouteNode, track_ids: &HashSet<u8>) -> Result<(), LayoutError> {
    match node {
        RouteNode::Connection { peer_track, .. } => {
            if !track_ids.contains(peer_track) {
                return Err(LayoutError::UnknownPeerTrack(*peer_track));
            }
        }
        RouteNode::Point {
            entry,
            thru,
            branch,
            ..
        } => {
            for n in entry
                .along_fwd
                .iter()
                .chain(thru.along_fwd.iter())
                .chain(branch.along_fwd.iter())
            {
                validate_connection_peers(n, track_ids)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_connections(this_track: u8, node: &RouteNode, by_conn: &mut HashMap<u8, Vec<ConnEnd>>) {
    match node {
        RouteNode::Connection {
            id,
            peer_track,
            peer_side,
        } => {
            by_conn.entry(*id).or_default().push(ConnEnd {
                this_track,
                peer_track: *peer_track,
                peer_side: *peer_side,
            });
        }
        RouteNode::Point {
            entry,
            thru,
            branch,
            ..
        } => {
            for n in &entry.along_fwd {
                collect_connections(this_track, n, by_conn);
            }
            for n in &thru.along_fwd {
                collect_connections(this_track, n, by_conn);
            }
            for n in &branch.along_fwd {
                collect_connections(this_track, n, by_conn);
            }
        }
        _ => {}
    }
}

/// One logical endpoint of a `connection` hop: which track owns the node and which peer port it uses.
///
/// The same `connection` id may appear **more than twice** in the route forest if the file repeats
/// the same hop (e.g. duplicate `point` subtrees). Identical `(this_track, peer_track, peer_side)`
/// entries are **deduplicated** before checking the usual pair rule: exactly **two** distinct
/// endpoints, on two different tracks, with reciprocal `peer_track` references.
fn dedupe_connection_ends(ends: &[ConnEnd]) -> Vec<ConnEnd> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for &e in ends {
        if seen.insert(e) {
            out.push(e);
        }
    }
    out
}

fn validate_connection_groups(
    by_conn: &HashMap<u8, Vec<ConnEnd>>,
    track_ids: &HashSet<u8>,
) -> Result<(), LayoutError> {
    for (&id, ends) in by_conn {
        let ends = dedupe_connection_ends(ends);
        let count = ends.len();
        if count != 2 {
            return Err(LayoutError::ConnectionEndpointCount { id, count });
        }
        let a = ends[0];
        let b = ends[1];
        if !track_ids.contains(&a.peer_track) || !track_ids.contains(&b.peer_track) {
            return Err(LayoutError::UnknownPeerTrack(a.peer_track));
        }
        if a.this_track != b.peer_track || a.peer_track != b.this_track {
            return Err(LayoutError::ConnectionNotReciprocal { id });
        }
    }
    Ok(())
}

fn coupler_connection_refs(c: &CouplerDef) -> Vec<&ConnectionRef> {
    vec![&c.entry_a, &c.thru_a, &c.entry_b, &c.thru_b]
}

fn validate_coupler_connection_ref(
    track_id_set: &HashSet<u8>,
    coupler_id_set: &HashSet<u8>,
    r: &ConnectionRef,
) -> Result<(), LayoutError> {
    match r {
        ConnectionRef::TrackPort { track, .. } => {
            TrackId::try_new(*track).ok_or(LayoutError::InvalidTrackId(*track))?;
            if !track_id_set.contains(track) {
                return Err(LayoutError::UnknownTrackRef(*track));
            }
            Ok(())
        }
        ConnectionRef::CouplerLeg { coupler, .. } => {
            PointId::try_new(*coupler).ok_or(LayoutError::InvalidCouplerId(*coupler))?;
            if !coupler_id_set.contains(coupler) {
                return Err(LayoutError::UnknownCouplerRef(*coupler));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::model::{PointLeg, RouteNode, Station, TrackSegment};

    /// Minimal valid layout for unit tests (does not read `data/track_layout.toml` so local edits
    /// to the canonical file cannot break the test suite).
    const STUB: &str = r#"
version = 2

[[tracks]]
id = 1
along_fwd = [
  { kind = "sensor", id = 1 },
]

[[tracks]]
id = 3
along_fwd = [
  { kind = "point", id = 5, entry = [{ kind = "inline" }], thru = [{ kind = "buffer" }], branch = [{ kind = "buffer" }] },
]

[[stations]]
name = "s"
sensor_ids = [1]
"#;

    #[test]
    fn stub_parses_and_validates() {
        let layout = TrackLayout::from_toml_str(STUB).expect("parse");
        layout.validate().expect("valid stub");
    }

    /// Canonical `data/track_layout.toml` must parse and deserialize. (Call [`TrackLayout::validate`]
    /// separately when checking connection pairing and other graph rules.)
    #[test]
    fn data_track_layout_toml_loads() {
        let path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
        let _layout = TrackLayout::from_path(&path).expect("data/track_layout.toml should load");
    }

    #[test]
    fn duplicate_sensor_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        if let Some(t) = layout.tracks.first_mut() {
            t.along_fwd.push(RouteNode::Sensor { id: 1 });
        }
        assert_eq!(layout.validate(), Err(LayoutError::DuplicateSensorId(1)));
    }

    #[test]
    fn invalid_track_id_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        layout.tracks.push(TrackSegment {
            id: 99,
            along_fwd: vec![],
        });
        assert_eq!(layout.validate(), Err(LayoutError::InvalidTrackId(99)));
    }

    #[test]
    fn two_track_connection_reciprocal_ok() {
        let toml = r#"
version = 2

[[tracks]]
id = 1
along_fwd = [
  { kind = "connection", id = 1, peer_track = 2, peer_side = "bwd" },
]

[[tracks]]
id = 2
along_fwd = [
  { kind = "connection", id = 1, peer_track = 1, peer_side = "fwd" },
]
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        layout.validate().expect("reciprocal connection");
    }

    /// Same `connection` id may appear multiple times in the tree if the same hop is repeated
    /// (e.g. duplicate subtrees); identical endpoints merge to one before pairing checks.
    #[test]
    fn duplicate_identical_connection_endpoints_ok() {
        let toml = r#"
version = 2

[[tracks]]
id = 1
along_fwd = [
  { kind = "connection", id = 1, peer_track = 2, peer_side = "bwd" },
  { kind = "connection", id = 1, peer_track = 2, peer_side = "bwd" },
]

[[tracks]]
id = 2
along_fwd = [
  { kind = "connection", id = 1, peer_track = 1, peer_side = "fwd" },
]
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        layout.validate().expect("deduped reciprocal connection");
    }

    #[test]
    fn connection_not_reciprocal_fails() {
        let toml = r#"
version = 2

[[tracks]]
id = 1
along_fwd = [
  { kind = "connection", id = 1, peer_track = 2, peer_side = "bwd" },
]

[[tracks]]
id = 2
along_fwd = []
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        assert!(matches!(
            layout.validate(),
            Err(LayoutError::ConnectionEndpointCount { .. })
                | Err(LayoutError::ConnectionNotReciprocal { .. })
        ));
    }

    #[test]
    fn duplicate_point_on_same_track_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        if let Some(t) = layout.tracks.iter_mut().find(|x| x.id == 3) {
            t.along_fwd.push(RouteNode::Point {
                id: 5,
                entry: PointLeg { along_fwd: vec![] },
                thru: PointLeg { along_fwd: vec![] },
                branch: PointLeg { along_fwd: vec![] },
            });
        }
        assert_eq!(
            layout.validate(),
            Err(LayoutError::DuplicatePointOnTrack { track: 3, point: 5 })
        );
    }

    #[test]
    fn unknown_station_sensor_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        layout.stations.push(Station {
            name: "Ghost".into(),
            sensor_ids: vec![22],
        });
        assert!(matches!(
            layout.validate(),
            Err(LayoutError::UnknownStationSensor { .. })
        ));
    }
}
