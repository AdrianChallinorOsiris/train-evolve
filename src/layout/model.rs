//! Serializable layout structures (match `data/track_layout.toml`).

use serde::{Deserialize, Serialize};

/// Which end of a track segment a connection refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackSide {
    Fwd,
    Bwd,
}

/// End of a track: buffer stop or connection to another track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackEnd {
    Buffer,
    Interconnect {
        peer_track: u8,
        peer_side: TrackSide,
    },
}

/// One element along a track in the **FWD** direction (from BWD end toward FWD end).
///
/// Points may appear between sensors, **before** the first sensor, or **after** the last sensor—
/// any order is allowed as long as it matches physical order along the segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackElement {
    Sensor { id: u8 },
    Point { id: u8 },
}

/// One segment of track with intrinsic FWD/BWD and an ordered sequence of sensors/points along FWD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackSegment {
    pub id: u8,
    /// Ordered sequence along **FWD** travel (BWD end → FWD end). Mix `sensor` and `point` in
    /// order; points may be before the first sensor, between sensors, or after the last sensor.
    pub along_fwd: Vec<TrackElement>,
    pub fwd_end: TrackEnd,
    pub bwd_end: TrackEnd,
    /// When true, this segment reverses travel direction (FWD in → BWD out to another track).
    pub reverses_direction: bool,
}

impl TrackSegment {
    /// Sensor ids on this segment in FWD order (subset of `along_fwd`).
    pub fn sensors_along_fwd(&self) -> impl Iterator<Item = u8> + '_ {
        self.along_fwd.iter().filter_map(|e| match e {
            TrackElement::Sensor { id } => Some(*id),
            TrackElement::Point { .. } => None,
        })
    }

    /// Point ids on this segment in FWD order (subset of `along_fwd`).
    pub fn points_along_fwd(&self) -> impl Iterator<Item = u8> + '_ {
        self.along_fwd.iter().filter_map(|e| match e {
            TrackElement::Point { id } => Some(*id),
            TrackElement::Sensor { .. } => None,
        })
    }
}

/// Role of a leg when connecting to another point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointLegRole {
    Entry,
    Thru,
    Branch,
}

/// Connection target for point legs: a track port or another point’s leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionRef {
    TrackPort { track: u8, side: TrackSide },
    PointLeg { point: u8, leg: PointLegRole },
}

/// A station groups one or more sensors (possibly on different tracks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    pub name: String,
    pub sensor_ids: Vec<u8>,
}

/// Point definition: independent switch or a coupled pair sharing one point number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "coupling", rename_all = "snake_case")]
pub enum PointDef {
    Independent {
        id: u8,
        entry: ConnectionRef,
        thru: ConnectionRef,
        branch: ConnectionRef,
    },
    Coupled {
        id: u8,
        entry_a: ConnectionRef,
        thru_a: ConnectionRef,
        branch_a: ConnectionRef,
        entry_b: ConnectionRef,
        thru_b: ConnectionRef,
        branch_b: ConnectionRef,
    },
}

impl PointDef {
    pub fn id(&self) -> u8 {
        match self {
            PointDef::Independent { id, .. } | PointDef::Coupled { id, .. } => *id,
        }
    }
}

/// Root document for `track_layout.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackLayout {
    pub version: u32,
    pub tracks: Vec<TrackSegment>,
    #[serde(default)]
    pub points: Vec<PointDef>,
    #[serde(default)]
    pub stations: Vec<Station>,
    /// Free-form notes for humans (optional).
    #[serde(default)]
    pub notes: Option<String>,
}
