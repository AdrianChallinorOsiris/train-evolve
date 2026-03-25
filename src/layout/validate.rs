//! Validation rules for [`TrackLayout`](crate::layout::model::TrackLayout).

use std::collections::HashSet;

use crate::layout::ids::{PointId, SensorId, TrackId};
use crate::layout::model::{
    ConnectionRef, PointDef, TrackElement, TrackEnd, TrackLayout, TrackSide,
};

/// Layout validation error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("unsupported layout version: {0} (expected 1)")]
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
    #[error("track {track} references unknown point id {point} in along_fwd")]
    UnknownPointOnTrack { track: u8, point: u8 },
    #[error("track {track} lists point id {point} more than once in along_fwd")]
    DuplicatePointOnTrack { track: u8, point: u8 },
    #[error("expected exactly one track with reverses_direction = true, found {0}")]
    ReverserCount(usize),
    #[error("duplicate point id: {0}")]
    DuplicatePointId(u8),
    #[error("invalid point id {0}: must be {}..={}", PointId::MIN, PointId::MAX)]
    InvalidPointId(u8),
    #[error("interconnect references unknown peer track {0}")]
    UnknownPeerTrack(u8),
    #[error("connection references unknown track id {0}")]
    UnknownTrackRef(u8),
    #[error(
        "interconnect mismatch: track {from} {from_side:?} connects to track {peer} {peer_side:?}, but that peer end does not connect back to track {from} {expected_back:?}"
    )]
    InterconnectMismatch {
        from: u8,
        from_side: TrackSide,
        peer: u8,
        peer_side: TrackSide,
        expected_back: TrackSide,
    },
    #[error("connection references unknown point id {0}")]
    UnknownPointRef(u8),
    #[error("station {name:?} references unknown sensor id {sensor}")]
    UnknownStationSensor { name: String, sensor: u8 },
}

impl TrackLayout {
    /// Validate numeric ranges, uniqueness, interconnect reciprocity, and reference integrity.
    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.version != 1 {
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

        let mut sensors_seen = HashSet::new();
        for t in &self.tracks {
            let mut points_on_track = HashSet::new();
            for el in &t.along_fwd {
                match *el {
                    TrackElement::Sensor { id: s } => {
                        SensorId::try_new(s).ok_or(LayoutError::InvalidSensorId(s))?;
                        if !sensors_seen.insert(s) {
                            return Err(LayoutError::DuplicateSensorId(s));
                        }
                    }
                    TrackElement::Point { id: p } => {
                        PointId::try_new(p).ok_or(LayoutError::InvalidPointId(p))?;
                        if !points_on_track.insert(p) {
                            return Err(LayoutError::DuplicatePointOnTrack {
                                track: t.id,
                                point: p,
                            });
                        }
                        if !self.points.iter().any(|pd| pd.id() == p) {
                            return Err(LayoutError::UnknownPointOnTrack {
                                track: t.id,
                                point: p,
                            });
                        }
                    }
                }
            }
        }

        let reversers = self.tracks.iter().filter(|t| t.reverses_direction).count();
        if reversers != 1 {
            return Err(LayoutError::ReverserCount(reversers));
        }

        let mut point_ids = HashSet::new();
        for p in &self.points {
            let id = p.id();
            PointId::try_new(id).ok_or(LayoutError::InvalidPointId(id))?;
            if !point_ids.insert(id) {
                return Err(LayoutError::DuplicatePointId(id));
            }
        }

        for t in &self.tracks {
            validate_end(self, t.id, TrackSide::Fwd, &t.fwd_end)?;
            validate_end(self, t.id, TrackSide::Bwd, &t.bwd_end)?;
        }

        let point_id_set: HashSet<u8> = self.points.iter().map(|p| p.id()).collect();
        let track_id_set: HashSet<u8> = self.tracks.iter().map(|t| t.id).collect();
        for p in &self.points {
            for r in connection_refs(p) {
                validate_connection_ref(&point_id_set, &track_id_set, r)?;
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

fn validate_end(
    layout: &TrackLayout,
    from: u8,
    from_side: TrackSide,
    end: &TrackEnd,
) -> Result<(), LayoutError> {
    let TrackEnd::Interconnect {
        peer_track,
        peer_side,
    } = end
    else {
        return Ok(());
    };

    TrackId::try_new(*peer_track).ok_or(LayoutError::UnknownPeerTrack(*peer_track))?;

    let Some(peer) = layout.tracks.iter().find(|t| t.id == *peer_track) else {
        return Err(LayoutError::UnknownPeerTrack(*peer_track));
    };

    let peer_end = match peer_side {
        TrackSide::Fwd => &peer.fwd_end,
        TrackSide::Bwd => &peer.bwd_end,
    };

    match peer_end {
        TrackEnd::Interconnect {
            peer_track: back,
            peer_side: back_side,
        } => {
            if *back != from || *back_side != from_side {
                return Err(LayoutError::InterconnectMismatch {
                    from,
                    from_side,
                    peer: *peer_track,
                    peer_side: *peer_side,
                    expected_back: from_side,
                });
            }
            Ok(())
        }
        _ => Err(LayoutError::InterconnectMismatch {
            from,
            from_side,
            peer: *peer_track,
            peer_side: *peer_side,
            expected_back: from_side,
        }),
    }
}

fn connection_refs(p: &PointDef) -> Vec<&ConnectionRef> {
    match p {
        PointDef::Independent {
            entry,
            thru,
            branch,
            ..
        } => vec![entry, thru, branch],
        PointDef::Coupled {
            entry_a,
            thru_a,
            branch_a,
            entry_b,
            thru_b,
            branch_b,
            ..
        } => vec![entry_a, thru_a, branch_a, entry_b, thru_b, branch_b],
    }
}

fn validate_connection_ref(
    point_id_set: &HashSet<u8>,
    track_id_set: &HashSet<u8>,
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
        ConnectionRef::PointLeg { point, .. } => {
            PointId::try_new(*point).ok_or(LayoutError::InvalidPointId(*point))?;
            if !point_id_set.contains(point) {
                return Err(LayoutError::UnknownPointRef(*point));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::model::{Station, TrackElement, TrackSegment};

    const STUB: &str = include_str!("../../data/track_layout.toml");

    #[test]
    fn stub_parses_and_validates() {
        let layout = TrackLayout::from_toml_str(STUB).expect("parse");
        layout.validate().expect("valid stub");
    }

    #[test]
    fn duplicate_sensor_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        if let Some(t) = layout.tracks.first_mut() {
            t.along_fwd.push(TrackElement::Sensor { id: 1 });
            t.along_fwd.push(TrackElement::Sensor { id: 1 });
        }
        assert_eq!(layout.validate(), Err(LayoutError::DuplicateSensorId(1)));
    }

    #[test]
    fn zero_reversers_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        for t in &mut layout.tracks {
            t.reverses_direction = false;
        }
        assert_eq!(layout.validate(), Err(LayoutError::ReverserCount(0)));
    }

    #[test]
    fn invalid_track_id_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        layout.tracks.push(TrackSegment {
            id: 99,
            along_fwd: vec![],
            fwd_end: TrackEnd::Buffer,
            bwd_end: TrackEnd::Buffer,
            reverses_direction: false,
        });
        assert_eq!(layout.validate(), Err(LayoutError::InvalidTrackId(99)));
    }

    #[test]
    fn two_track_interconnect_reciprocal_ok() {
        let toml = r#"
version = 1

[[tracks]]
id = 1
along_fwd = []
reverses_direction = true
fwd_end = { kind = "interconnect", peer_track = 2, peer_side = "bwd" }
bwd_end = { kind = "buffer" }

[[tracks]]
id = 2
along_fwd = []
reverses_direction = false
fwd_end = { kind = "buffer" }
bwd_end = { kind = "interconnect", peer_track = 1, peer_side = "fwd" }
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        layout.validate().expect("reciprocal interconnect");
    }

    #[test]
    fn interconnect_not_reciprocal_fails() {
        let toml = r#"
version = 1

[[tracks]]
id = 1
along_fwd = []
reverses_direction = true
fwd_end = { kind = "interconnect", peer_track = 2, peer_side = "bwd" }
bwd_end = { kind = "buffer" }

[[tracks]]
id = 2
along_fwd = []
reverses_direction = false
fwd_end = { kind = "buffer" }
bwd_end = { kind = "buffer" }
"#;
        let layout = TrackLayout::from_toml_str(toml).expect("parse");
        assert!(matches!(
            layout.validate(),
            Err(LayoutError::InterconnectMismatch { .. })
        ));
    }

    #[test]
    fn duplicate_point_on_same_track_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        if let Some(t) = layout.tracks.iter_mut().find(|x| x.id == 2) {
            t.along_fwd.push(TrackElement::Point { id: 1 });
        }
        assert_eq!(
            layout.validate(),
            Err(LayoutError::DuplicatePointOnTrack { track: 2, point: 1 })
        );
    }

    #[test]
    fn unknown_station_sensor_fails() {
        let mut layout = TrackLayout::from_toml_str(STUB).unwrap();
        layout.stations.push(Station {
            name: "Ghost".into(),
            // Valid id range but no track lists this sensor in along_fwd.
            sensor_ids: vec![10],
        });
        assert!(matches!(
            layout.validate(),
            Err(LayoutError::UnknownStationSensor { .. })
        ));
    }
}
