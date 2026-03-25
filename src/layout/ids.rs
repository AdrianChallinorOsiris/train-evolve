//! Typed identifiers with documented numeric ranges (hardware numbering).

use std::fmt;

/// Track segment id (1–12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(pub u8);

/// Sensor id (1–24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SensorId(pub u8);

/// Point (switch) id (1–13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PointId(pub u8);

impl TrackId {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 12;

    /// Returns `Some(TrackId)` if `raw` is in range.
    pub fn try_new(raw: u8) -> Option<Self> {
        if (Self::MIN..=Self::MAX).contains(&raw) {
            Some(Self(raw))
        } else {
            None
        }
    }
}

impl SensorId {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 24;

    pub fn try_new(raw: u8) -> Option<Self> {
        if (Self::MIN..=Self::MAX).contains(&raw) {
            Some(Self(raw))
        } else {
            None
        }
    }
}

impl PointId {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 13;

    pub fn try_new(raw: u8) -> Option<Self> {
        if (Self::MIN..=Self::MAX).contains(&raw) {
            Some(Self(raw))
        } else {
            None
        }
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for SensorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for PointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
