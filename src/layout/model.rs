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

/// One segment of track with intrinsic FWD/BWD and ordered sensors along FWD travel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackSegment {
    pub id: u8,
    /// Sensor ids in order along the FWD direction (first = toward BWD end, last = toward FWD end),
    /// or the reverse depending on convention — see `docs/TRACK_LAYOUT.md`.
    pub sensors_fwd: Vec<u8>,
    /// Point ids that sit on this segment (reference only; geometry lives under `points`).
    pub point_ids: Vec<u8>,
    pub fwd_end: TrackEnd,
    pub bwd_end: TrackEnd,
    /// When true, this segment reverses travel direction (FWD in → BWD out to another track).
    pub reverses_direction: bool,
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
