//! HTTP client for the Raspberry Pi train-control API.
//!
//! The Pi runs at a configurable base URL (default `http://192.168.1.80:5000`).
//! All methods are async and return strongly-typed Rust structs.
//!
//! ## Control methods
//!
//! | Method | Pi endpoint | Description |
//! |--------|-------------|-------------|
//! | [`PiClient::set_track_speed`] | `POST /api/tracks/{id}/speed` | Set direction + speed |
//! | [`PiClient::stop_track`] | `POST /api/tracks/{id}/stop` | Emergency stop one track |
//! | [`PiClient::all_stop`] | `POST /api/tracks/allstop` | Emergency stop ALL tracks |
//! | [`PiClient::set_point`] | `POST /api/points/{id}` | Switch a point |
//! | [`PiClient::set_sensor`] | `POST /api/sensors/{id}` | Force a sensor bit (testing) |
//! | [`PiClient::reset_sensors`] | `POST /api/sensors/reset` | Clear all sensors |

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Default base URL for the Pi API.
pub const DEFAULT_PI_URL: &str = "http://192.168.1.80:5000";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Track direction for `POST /api/tracks/{id}/speed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TrackDirection {
    Off,
    Fwd,
    #[serde(rename = "BCK")]
    Bck,
}

impl fmt::Display for TrackDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => write!(f, "OFF"),
            Self::Fwd => write!(f, "FWD"),
            Self::Bck => write!(f, "BCK"),
        }
    }
}

/// Point direction for `POST /api/points/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PointDirection {
    Thru,
    Branch,
}

impl fmt::Display for PointDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Thru => write!(f, "THRU"),
            Self::Branch => write!(f, "BRANCH"),
        }
    }
}

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

/// Generic success response from Pi control endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiAck {
    pub success: bool,
    #[serde(default)]
    pub message: Option<String>,
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
    #[error("invalid parameter: {0}")]
    InvalidParam(String),
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

    // -------------------------------------------------------------------
    // Control methods (POST)
    // -------------------------------------------------------------------

    /// `POST /api/tracks/{track_id}/speed?speed={speed}&direction={direction}`
    ///
    /// Set the speed (0–100) and direction of a track segment.
    pub async fn set_track_speed(
        &self,
        track_id: u8,
        direction: TrackDirection,
        speed: u8,
    ) -> Result<PiAck, PiError> {
        if !(1..=12).contains(&track_id) {
            return Err(PiError::InvalidParam(format!(
                "track_id {track_id}: must be 1..=12"
            )));
        }
        if speed > 100 {
            return Err(PiError::InvalidParam(format!(
                "speed {speed}: must be 0..=100"
            )));
        }
        let path = format!("/api/tracks/{track_id}/speed?speed={speed}&direction={direction}");
        let body = self.post(&path).await?;
        parse_ack(&body)
    }

    /// `POST /api/tracks/{track_id}/stop` — emergency stop one track.
    pub async fn stop_track(&self, track_id: u8) -> Result<PiAck, PiError> {
        if !(1..=12).contains(&track_id) {
            return Err(PiError::InvalidParam(format!(
                "track_id {track_id}: must be 1..=12"
            )));
        }
        let path = format!("/api/tracks/{track_id}/stop");
        let body = self.post(&path).await?;
        parse_ack(&body)
    }

    /// `POST /api/tracks/allstop` — emergency stop ALL tracks.
    pub async fn all_stop(&self) -> Result<PiAck, PiError> {
        let body = self.post("/api/tracks/allstop").await?;
        parse_ack(&body)
    }

    /// `POST /api/points/{point_id}?direction={direction}` — switch a point.
    pub async fn set_point(
        &self,
        point_id: u8,
        direction: PointDirection,
    ) -> Result<PiAck, PiError> {
        if !(1..=13).contains(&point_id) {
            return Err(PiError::InvalidParam(format!(
                "point_id {point_id}: must be 1..=13"
            )));
        }
        let path = format!("/api/points/{point_id}?direction={direction}");
        let body = self.post(&path).await?;
        parse_ack(&body)
    }

    /// `POST /api/sensors/{sensor_id}?value={value}` — force a sensor bit (for testing).
    pub async fn set_sensor(&self, sensor_id: u8, value: bool) -> Result<PiAck, PiError> {
        if !(1..=24).contains(&sensor_id) {
            return Err(PiError::InvalidParam(format!(
                "sensor_id {sensor_id}: must be 1..=24"
            )));
        }
        let val = if value { "true" } else { "false" };
        let path = format!("/api/sensors/{sensor_id}?value={val}");
        let body = self.post(&path).await?;
        parse_ack(&body)
    }

    /// `POST /api/sensors/reset` — clear all sensors.
    pub async fn reset_sensors(&self) -> Result<PiAck, PiError> {
        let body = self.post("/api/sensors/reset").await?;
        parse_ack(&body)
    }

    // -------------------------------------------------------------------
    // Low-level HTTP helpers
    // -------------------------------------------------------------------

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

    /// Low-level POST via `curl` (empty body, query params in path) with a 5-second timeout.
    async fn post(&self, path: &str) -> Result<String, PiError> {
        let url = format!("{}{}", self.base_url, path);
        let output = tokio::process::Command::new("curl")
            .args([
                "-sf",
                "-X",
                "POST",
                "--connect-timeout",
                "5",
                "--max-time",
                "10",
                &url,
            ])
            .output()
            .await
            .map_err(|e| PiError::Unreachable(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PiError::ApiError(format!(
                "curl exit {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(|e| PiError::BadResponse(e.to_string()))
    }
}

/// Parse a generic `{ "success": true, ... }` Pi response.
fn parse_ack(body: &str) -> Result<PiAck, PiError> {
    let ack: PiAck = serde_json::from_str(body).map_err(|e| PiError::BadResponse(e.to_string()))?;
    if !ack.success {
        return Err(PiError::ApiError(
            ack.message
                .unwrap_or_else(|| "Pi returned success=false".into()),
        ));
    }
    Ok(ack)
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
    fn track_direction_display() {
        assert_eq!(TrackDirection::Off.to_string(), "OFF");
        assert_eq!(TrackDirection::Fwd.to_string(), "FWD");
        assert_eq!(TrackDirection::Bck.to_string(), "BCK");
    }

    #[test]
    fn point_direction_display() {
        assert_eq!(PointDirection::Thru.to_string(), "THRU");
        assert_eq!(PointDirection::Branch.to_string(), "BRANCH");
    }

    #[test]
    fn track_direction_serde_roundtrip() {
        let json = serde_json::to_string(&TrackDirection::Fwd).unwrap();
        assert_eq!(json, "\"FWD\"");
        let parsed: TrackDirection = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, TrackDirection::Fwd);
    }

    #[test]
    fn point_direction_serde_roundtrip() {
        let json = serde_json::to_string(&PointDirection::Branch).unwrap();
        assert_eq!(json, "\"BRANCH\"");
        let parsed: PointDirection = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, PointDirection::Branch);
    }

    #[test]
    fn parse_ack_success() {
        let json = r#"{"success": true, "message": "done"}"#;
        let ack = parse_ack(json).unwrap();
        assert!(ack.success);
        assert_eq!(ack.message.as_deref(), Some("done"));
    }

    #[test]
    fn parse_ack_failure() {
        let json = r#"{"success": false, "message": "track held"}"#;
        let err = parse_ack(json).unwrap_err();
        assert!(err.to_string().contains("track held"));
    }

    #[test]
    fn parse_ack_no_message() {
        let json = r#"{"success": true}"#;
        let ack = parse_ack(json).unwrap();
        assert!(ack.success);
        assert!(ack.message.is_none());
    }

    #[tokio::test]
    async fn set_track_speed_rejects_bad_track_id() {
        let c = PiClient::new("http://localhost:1");
        let err = c.set_track_speed(0, TrackDirection::Fwd, 50).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
        let err = c.set_track_speed(13, TrackDirection::Fwd, 50).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
    }

    #[tokio::test]
    async fn set_track_speed_rejects_bad_speed() {
        let c = PiClient::new("http://localhost:1");
        let err = c.set_track_speed(1, TrackDirection::Fwd, 101).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
    }

    #[tokio::test]
    async fn stop_track_rejects_bad_track_id() {
        let c = PiClient::new("http://localhost:1");
        let err = c.stop_track(0).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
    }

    #[tokio::test]
    async fn set_point_rejects_bad_point_id() {
        let c = PiClient::new("http://localhost:1");
        let err = c.set_point(0, PointDirection::Thru).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
        let err = c.set_point(14, PointDirection::Thru).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
    }

    #[tokio::test]
    async fn set_sensor_rejects_bad_sensor_id() {
        let c = PiClient::new("http://localhost:1");
        let err = c.set_sensor(0, true).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
        let err = c.set_sensor(25, true).await;
        assert!(matches!(err, Err(PiError::InvalidParam(_))));
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
