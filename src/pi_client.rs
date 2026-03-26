//! HTTP client for the Raspberry Pi train-control API.
//!
//! The Pi runs at a configurable base URL (default `http://192.168.1.80:5000`).
//! All methods are async and return strongly-typed Rust structs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default base URL for the Pi API.
pub const DEFAULT_PI_URL: &str = "http://192.168.1.80:5000";

/// Overall status snapshot from `GET /api/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiStatus {
    pub tracks: HashMap<String, TrackStatus>,
    pub points: HashMap<String, PointStatus>,
    pub sensors: u32,
    pub indicators: HashMap<String, String>,
}

/// One track segment's live state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackStatus {
    pub id: String,
    pub direction: String,
    pub speed: u8,
    pub held: bool,
}

/// One point's live state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointStatus {
    pub id: String,
    pub direction: String,
    pub thru: bool,
}

/// Health info from `GET /api/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiHealth {
    pub status: String,
    pub cpu_temperature_celsius: f64,
    pub fan_speed_percent: f64,
    pub memory_free_percent: f64,
    pub disk_free_percent: f64,
    pub protection_system_running: bool,
    pub version: String,
}

/// All sensor values from `GET /api/sensors`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorState {
    pub id: u8,
    pub value: bool,
}

/// Errors from Pi API calls.
#[derive(Debug, thiserror::Error)]
pub enum PiError {
    #[error("Pi unreachable: {0}")]
    Unreachable(String),
    #[error("Pi returned non-success: {0}")]
    ApiError(String),
    #[error("unexpected response shape: {0}")]
    BadResponse(String),
}

/// Thin client wrapping `curl` (no extra HTTP dependency).
///
/// Every method shells out to `curl` and parses the JSON response.  This is intentionally simple:
/// we avoid pulling in `reqwest` / `hyper-client` until the call frequency justifies it.
#[derive(Debug, Clone)]
pub struct PiClient {
    base_url: String,
}

impl Default for PiClient {
    fn default() -> Self {
        Self::new(DEFAULT_PI_URL)
    }
}

impl PiClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// `GET /api/status` — full track/point/sensor/indicator snapshot.
    pub async fn status(&self) -> Result<PiStatus, PiError> {
        let body = self.get("/api/status").await?;
        let root: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| PiError::BadResponse(e.to_string()))?;
        let data = root
            .get("data")
            .ok_or_else(|| PiError::BadResponse("missing 'data' key".into()))?;
        serde_json::from_value(data.clone()).map_err(|e| PiError::BadResponse(e.to_string()))
    }

    /// `GET /api/health` — Pi hardware health.
    pub async fn health(&self) -> Result<PiHealth, PiError> {
        let body = self.get("/api/health").await?;
        serde_json::from_str(&body).map_err(|e| PiError::BadResponse(e.to_string()))
    }

    /// `GET /api/sensors` — all sensor values.
    pub async fn sensors(&self) -> Result<HashMap<String, SensorState>, PiError> {
        let body = self.get("/api/sensors").await?;
        let root: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| PiError::BadResponse(e.to_string()))?;
        let data = root
            .get("data")
            .and_then(|d| d.get("sensors"))
            .ok_or_else(|| PiError::BadResponse("missing 'data.sensors' key".into()))?;
        serde_json::from_value(data.clone()).map_err(|e| PiError::BadResponse(e.to_string()))
    }

    /// Low-level GET via `curl` with a 5-second timeout.
    async fn get(&self, path: &str) -> Result<String, PiError> {
        let url = format!("{}{}", self.base_url, path);
        let output = tokio::process::Command::new("curl")
            .args(["-sf", "--connect-timeout", "5", "--max-time", "10", &url])
            .output()
            .await
            .map_err(|e| PiError::Unreachable(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PiError::Unreachable(format!(
                "curl exit {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(|e| PiError::BadResponse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_url() {
        let c = PiClient::default();
        assert_eq!(c.base_url, "http://192.168.1.80:5000");
    }

    #[test]
    fn parse_status_json() {
        let json = r#"{
            "data": {
                "tracks": {
                    "1": {"id": "T01", "direction": "OFF", "speed": 0, "held": false}
                },
                "points": {
                    "1": {"id": "P01", "direction": "THRU", "thru": true}
                },
                "sensors": 0,
                "indicators": {
                    "system_ready": "GREEN"
                }
            },
            "success": true
        }"#;
        let root: serde_json::Value = serde_json::from_str(json).unwrap();
        let data = root.get("data").unwrap();
        let status: PiStatus = serde_json::from_value(data.clone()).unwrap();
        assert_eq!(status.tracks.len(), 1);
        assert_eq!(status.tracks["1"].direction, "OFF");
        assert!(status.points["1"].thru);
        assert_eq!(status.sensors, 0);
    }

    #[test]
    fn parse_health_json() {
        let json = r#"{
            "status": "healthy",
            "cpu_temperature_celsius": 18.8,
            "fan_speed_percent": 49.0,
            "memory_free_percent": 94.87,
            "disk_free_percent": 74.91,
            "protection_system_running": true,
            "version": "1.0.22",
            "success": true
        }"#;
        let h: PiHealth = serde_json::from_str(json).unwrap();
        assert_eq!(h.status, "healthy");
        assert!(h.protection_system_running);
    }

    #[test]
    fn parse_sensors_json() {
        let json = r#"{
            "data": {
                "sensors": {
                    "1": {"id": 1, "value": false},
                    "2": {"id": 2, "value": true}
                }
            },
            "success": true
        }"#;
        let root: serde_json::Value = serde_json::from_str(json).unwrap();
        let data = root.get("data").and_then(|d| d.get("sensors")).unwrap();
        let sensors: HashMap<String, SensorState> = serde_json::from_value(data.clone()).unwrap();
        assert_eq!(sensors.len(), 2);
        assert!(!sensors["1"].value);
        assert!(sensors["2"].value);
    }
}
