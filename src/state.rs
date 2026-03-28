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
    /// Target sensor for this train (optional; used by route planner).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<u8>,
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
            if let Some(d) = t.destination {
                if !(1..=24).contains(&d) {
                    return Err(StateError::InvalidSensor(d));
                }
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

// ---------------------------------------------------------------------------
// Cumulative evolution statistics
// ---------------------------------------------------------------------------

/// Default path for persistent evolution statistics (gitignored).
pub fn stats_path() -> std::path::PathBuf {
    Path::new("data/runtime/stats.json").to_path_buf()
}

/// Persistent cumulative statistics across all evolution sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EvolutionStats {
    /// Total number of completed evolution sessions.
    pub sessions_completed: u32,
    /// Cumulative input tokens across all sessions.
    pub total_tokens_in: u64,
    /// Cumulative output tokens across all sessions.
    pub total_tokens_out: u64,
    /// ISO 8601 timestamp of the last completed session.
    pub last_session_at: String,
    /// Version at the last completed session.
    pub last_version: String,
}

impl EvolutionStats {
    /// Load from disk, returning defaults if the file doesn't exist.
    pub fn load() -> Result<Self, StateError> {
        let path = stats_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&s)?)
    }

    /// Persist to disk.
    pub fn save(&self) -> Result<(), StateError> {
        let path = stats_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Record a completed evolution session.
    pub fn record_session(&mut self, tokens_in: u64, tokens_out: u64, version: &str, date: &str) {
        self.sessions_completed += 1;
        self.total_tokens_in += tokens_in;
        self.total_tokens_out += tokens_out;
        self.last_session_at = date.to_string();
        self.last_version = version.to_string();
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

    fn tp(sensor: u8) -> TrainPosition {
        TrainPosition {
            sensor,
            destination: None,
        }
    }

    #[test]
    fn validate_max_trains() {
        let mut trains = vec![];
        for _ in 0..7 {
            trains.push(tp(1));
        }
        let req = InitialiseRequest { trains };
        assert!(req.validate().is_err());
    }

    #[test]
    fn validate_ok_trains() {
        let req = InitialiseRequest {
            trains: vec![tp(1), tp(12), tp(24)],
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn validate_invalid_sensor_zero() {
        let req = InitialiseRequest {
            trains: vec![tp(0)],
        };
        assert!(matches!(req.validate(), Err(StateError::InvalidSensor(0))));
    }

    #[test]
    fn validate_invalid_sensor_high() {
        let req = InitialiseRequest {
            trains: vec![tp(25)],
        };
        assert!(matches!(req.validate(), Err(StateError::InvalidSensor(25))));
    }

    #[test]
    fn validate_invalid_destination() {
        let req = InitialiseRequest {
            trains: vec![TrainPosition {
                sensor: 1,
                destination: Some(99),
            }],
        };
        assert!(matches!(req.validate(), Err(StateError::InvalidSensor(99))));
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trains.json");
        let req = InitialiseRequest {
            trains: vec![tp(3), tp(18)],
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

    #[test]
    fn evolution_stats_default() {
        let stats = EvolutionStats::default();
        assert_eq!(stats.sessions_completed, 0);
        assert_eq!(stats.total_tokens_in, 0);
        assert_eq!(stats.total_tokens_out, 0);
        assert!(stats.last_session_at.is_empty());
        assert!(stats.last_version.is_empty());
    }

    #[test]
    fn evolution_stats_record_session() {
        let mut stats = EvolutionStats::default();
        stats.record_session(1000, 500, "1.0.7", "2026-03-28");
        assert_eq!(stats.sessions_completed, 1);
        assert_eq!(stats.total_tokens_in, 1000);
        assert_eq!(stats.total_tokens_out, 500);
        assert_eq!(stats.last_session_at, "2026-03-28");
        assert_eq!(stats.last_version, "1.0.7");

        stats.record_session(2000, 800, "1.0.8", "2026-03-29");
        assert_eq!(stats.sessions_completed, 2);
        assert_eq!(stats.total_tokens_in, 3000);
        assert_eq!(stats.total_tokens_out, 1300);
        assert_eq!(stats.last_version, "1.0.8");
    }

    #[test]
    fn evolution_stats_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime").join("stats.json");

        let mut stats = EvolutionStats::default();
        stats.record_session(500, 200, "1.0.5", "2026-03-25");

        // Save to custom path
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let json = serde_json::to_string_pretty(&stats).unwrap();
        fs::write(&path, json).unwrap();

        // Load back
        let s = fs::read_to_string(&path).unwrap();
        let loaded: EvolutionStats = serde_json::from_str(&s).unwrap();
        assert_eq!(loaded, stats);
    }

    #[test]
    fn evolution_stats_serializes_to_json() {
        let mut stats = EvolutionStats::default();
        stats.record_session(1000, 500, "1.0.7", "2026-03-28");
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["sessions_completed"], 1);
        assert_eq!(json["total_tokens_in"], 1000);
        assert_eq!(json["total_tokens_out"], 500);
        assert_eq!(json["last_session_at"], "2026-03-28");
        assert_eq!(json["last_version"], "1.0.7");
    }
}
