//! Static model railway layout: tracks, sensors, points, stations, and validation.
//!
//! Load from [`TrackLayout::from_toml_str`] or [`TrackLayout::from_path`], then call
//! [`TrackLayout::validate`] before using the layout for routing.

mod ids;
mod load;
mod model;
mod validate;

pub use ids::{PointId, SensorId, TrackId};
pub use load::LoadError;
pub use model::{
    ConnectionRef, PointDef, PointLegRole, Station, TrackElement, TrackEnd, TrackLayout,
    TrackSegment, TrackSide,
};
pub use validate::LayoutError;
