//! Runtime train state (INITIALISE) and program placeholder (PROGRAM).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_TRAINS: usize = 6;

/// Default path for INITIALISE payload (gitignored).
pub fn trains_path() -> std::path::PathBuf {
    Path::new("data/runtime/trains.json").to_path_buf()
}

/// Default path for PROGRAM placeholder payload (gitignored).
pub fn program_path() -> std::path::PathBuf {
    Path::new("data/runtime/program.json").to_path_buf()
}

/// Snapshot of INITIALISE taken when `/automatic` starts (used by `/stop` to restore).
pub fn automatic_start_path() -> std::path::PathBuf {
    Path::new("data/runtime/automatic_start.json").to_path_buf()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialiseRequest {
    pub trains: Vec<TrainPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainPosition {
    /// Which sensor currently detects this train (1–24).
    pub sensor: u8,
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("at most {MAX_TRAINS} trains allowed, got {0}")]
    TooManyTrains(usize),
    #[error("invalid sensor id {0}: must be 1..=24")]
    InvalidSensor(u8),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl InitialiseRequest {
    pub fn validate(&self) -> Result<(), StateError> {
        if self.trains.len() > MAX_TRAINS {
            return Err(StateError::TooManyTrains(self.trains.len()));
        }
        for t in &self.trains {
            if !(1..=24).contains(&t.sensor) {
                return Err(StateError::InvalidSensor(t.sensor));
            }
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Option<Self>, StateError> {
        if !path.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&s)?))
    }
}

/// PROGRAM endpoint stores the raw JSON until the track program format exists.
pub fn save_program_placeholder(value: &serde_json::Value) -> Result<(), StateError> {
    let path = program_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_max_trains() {
        let mut trains = vec![];
        for _ in 0..7 {
            trains.push(TrainPosition { sensor: 1 });
        }
        let req = InitialiseRequest { trains };
        assert!(req.validate().is_err());
    }

    #[test]
    fn validate_ok_trains() {
        let req = InitialiseRequest {
            trains: vec![
                TrainPosition { sensor: 1 },
                TrainPosition { sensor: 12 },
                TrainPosition { sensor: 24 },
            ],
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn validate_invalid_sensor_zero() {
        let req = InitialiseRequest {
            trains: vec![TrainPosition { sensor: 0 }],
        };
        assert!(matches!(req.validate(), Err(StateError::InvalidSensor(0))));
    }

    #[test]
    fn validate_invalid_sensor_high() {
        let req = InitialiseRequest {
            trains: vec![TrainPosition { sensor: 25 }],
        };
        assert!(matches!(req.validate(), Err(StateError::InvalidSensor(25))));
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trains.json");
        let req = InitialiseRequest {
            trains: vec![TrainPosition { sensor: 3 }, TrainPosition { sensor: 18 }],
        };
        req.save(&path).unwrap();
        let loaded = InitialiseRequest::load(&path).unwrap().unwrap();
        assert_eq!(loaded.trains.len(), 2);
        assert_eq!(loaded.trains[0].sensor, 3);
        assert_eq!(loaded.trains[1].sensor, 18);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(InitialiseRequest::load(&path).unwrap().is_none());
    }
}
