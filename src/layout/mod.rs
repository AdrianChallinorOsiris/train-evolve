//! Static model railway layout (v2): route graphs with connections, nested points, validation.
//!
//! Load from [`TrackLayout::from_toml_str`] or [`TrackLayout::from_path`], then call
//! [`TrackLayout::validate`] before using the layout for routing.

pub mod graph;
mod ids;
mod load;
mod model;
mod validate;

pub use graph::{PointSetting, Route, SensorEdge, TrackGraph};
pub use ids::{PointId, SensorId, TrackId};
pub use load::LoadError;
pub use model::{
    ConnectionRef, CouplerDef, CouplerLegRole, CouplerSide, PointLeg, PointLegRole, RouteNode,
    Station, TrackLayout, TrackSegment, TrackSide,
};
pub use validate::LayoutError;
