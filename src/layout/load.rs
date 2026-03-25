//! Parse layout from TOML.

use std::fs;
use std::path::Path;

use crate::layout::model::TrackLayout;

impl TrackLayout {
    /// Parse layout from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Read and parse a layout file from disk.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let s = fs::read_to_string(path.as_ref()).map_err(LoadError::Io)?;
        Self::from_toml_str(&s).map_err(LoadError::Toml)
    }
}

/// Error when loading from a path.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}
