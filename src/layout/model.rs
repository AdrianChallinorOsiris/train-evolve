//! Serializable layout structures (v2; see `data/track_layout.toml`).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Which port of a peer track a [`RouteNode::Connection`] attaches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackSide {
    Fwd,
    Bwd,
}

/// One node in an ordered route: sensors, couplers, connections, nested points, terminals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteNode {
    Sensor {
        id: u8,
    },
    Coupler {
        id: u8,
    },
    /// Global connection id; this endpoint sits on `peer_track`’s `peer_side` port.
    Connection {
        id: u8,
        peer_track: u8,
        peer_side: TrackSide,
    },
    Point {
        id: u8,
        entry: PointLeg,
        thru: PointLeg,
        branch: PointLeg,
    },
    /// End of a spur (buffer stop).
    Buffer,
    /// Continuation along the same leg with no additional hop (placeholder).
    Inline,
}

/// One leg of a [`RouteNode::Point`]: ordered nodes along that leg (same vocabulary as track `along_fwd`).
///
/// In TOML, **either** a bare array `[{ kind = "sensor", ... }, ...]` **or** `{ along_fwd = [ ... ] }`
/// deserializes here (nested inline tables cannot nest `{ along_fwd = ... }` inside another inline
/// `point`, so deep trees should use the bare-array form for each leg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointLeg {
    pub along_fwd: Vec<RouteNode>,
}

impl<'de> Deserialize<'de> for PointLeg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Short(Vec<RouteNode>),
            Long { along_fwd: Vec<RouteNode> },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Short(along_fwd) => Ok(PointLeg { along_fwd }),
            Wire::Long { along_fwd } => Ok(PointLeg { along_fwd }),
        }
    }
}

impl Serialize for PointLeg {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Wrap<'a> {
            along_fwd: &'a [RouteNode],
        }
        Wrap {
            along_fwd: &self.along_fwd,
        }
        .serialize(serializer)
    }
}

/// One segment of track: ordered spine from BWD toward FWD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackSegment {
    pub id: u8,
    pub along_fwd: Vec<RouteNode>,
}

impl TrackSegment {
    /// Sensor ids in preorder walk of `along_fwd` (not deduplicated).
    pub fn sensors_in_route(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for n in &self.along_fwd {
            collect_sensors(n, &mut out);
        }
        out
    }
}

fn collect_sensors(node: &RouteNode, out: &mut Vec<u8>) {
    match node {
        RouteNode::Sensor { id } => out.push(*id),
        RouteNode::Point {
            entry,
            thru,
            branch,
            ..
        } => {
            for n in &entry.along_fwd {
                collect_sensors(n, out);
            }
            for n in &thru.along_fwd {
                collect_sensors(n, out);
            }
            for n in &branch.along_fwd {
                collect_sensors(n, out);
            }
        }
        RouteNode::Coupler { .. }
        | RouteNode::Connection { .. }
        | RouteNode::Buffer
        | RouteNode::Inline => {}
    }
}

/// Role of a leg when connecting to an independent point in legacy `ConnectionRef` (couplers only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointLegRole {
    Entry,
    Thru,
    Branch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CouplerSide {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CouplerLegRole {
    Entry,
    Thru,
}

/// Used only by [`CouplerDef`] to name physical legs (track port or another coupler’s straight leg).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionRef {
    TrackPort {
        track: u8,
        side: TrackSide,
    },
    CouplerLeg {
        coupler: u8,
        side: CouplerSide,
        leg: CouplerLegRole,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    pub name: String,
    pub sensor_ids: Vec<u8>,
}

/// Two fused turnouts sharing one motor id (four straight legs; branches fused internally).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouplerDef {
    pub id: u8,
    pub entry_a: ConnectionRef,
    pub thru_a: ConnectionRef,
    pub entry_b: ConnectionRef,
    pub thru_b: ConnectionRef,
}

/// Root document for `track_layout.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackLayout {
    /// Must be `2` for the connection-based route model.
    pub version: u32,
    pub tracks: Vec<TrackSegment>,
    #[serde(default)]
    pub couplers: Vec<CouplerDef>,
    #[serde(default)]
    pub stations: Vec<Station>,
    #[serde(default)]
    pub notes: Option<String>,
}
