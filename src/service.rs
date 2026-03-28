//! HTTP service: `/evolve`, `/initialise`, `/program`, `/automatic`, `/stop`, `/health`,
//! `/journal`, `/roadmap`, `/pi/status`, `/pi/health`, `/pi/sensors`,
//! `/pi/track/:id/speed`, `/pi/track/:id/stop`, `/pi/allstop`,
//! `/pi/point/:id`, `/pi/sensor/:id`, `/pi/sensors/reset`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::automation::AutomationController;
use crate::automation::AutomationError;
use crate::evolve_session::{run_evolution, EvolutionConfig, EvolutionError};
use crate::pi_client::{PiClient, PointDirection, TrackDirection};
use crate::route_planner;
use crate::state::{self, InitialiseRequest, StateError};
use crate::train_controller;

/// Sender used by the evolve handler to tell the server loop to shut down for a restart.
pub type ShutdownSender = tokio::sync::watch::Sender<bool>;

/// Shared service state (API keys and evolution config come from environment at startup).
#[derive(Clone)]
pub struct AppState {
    pub evolve_lock: Arc<Mutex<()>>,
    pub evolution: EvolutionConfig,
    pub automation: Arc<AutomationController>,
    pub pi: Arc<PiClient>,
    /// Set to `true` after a successful `/evolve` to trigger a graceful shutdown + restart.
    pub shutdown_tx: Arc<ShutdownSender>,
}

pub async fn serve(bind: SocketAddr, state: AppState) -> Result<(), std::io::Error> {
    let shutdown_rx = state.shutdown_tx.subscribe();

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/journal", get(journal_handler))
        .route("/roadmap", get(roadmap_handler))
        .route("/evolve", post(evolve_handler))
        .route("/initialise", post(initialise_handler))
        .route("/program", post(program_handler))
        .route("/route", post(route_handler))
        .route("/route/execute", post(route_execute_handler))
        .route("/automatic", post(automatic_handler))
        .route("/stop", post(stop_handler))
        // Pi read-only proxies
        .route("/pi/status", get(pi_status_handler))
        .route("/pi/health", get(pi_health_handler))
        .route("/pi/sensors", get(pi_sensors_handler))
        // Pi control endpoints
        .route("/pi/track/{id}/speed", post(pi_track_speed_handler))
        .route("/pi/track/{id}/stop", post(pi_track_stop_handler))
        .route("/pi/allstop", post(pi_allstop_handler))
        .route("/pi/point/{id}", post(pi_point_handler))
        .route("/pi/sensor/{id}", post(pi_sensor_handler))
        .route("/pi/sensors/reset", post(pi_sensors_reset_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut rx = shutdown_rx;
            // Wait until the sender broadcasts `true`.
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
            eprintln!("yoyo: graceful shutdown requested (restart after evolve)");
        })
        .await?;
    Ok(())
}

impl AppState {
    /// Same payload as `GET /health`.
    pub async fn health_json(&self) -> serde_json::Value {
        let automatic = self.automation.is_running().await;
        json!({
            "status": "ok",
            "automatic": automatic,
        })
    }

    /// Same payload as successful `POST /evolve`.
    pub async fn evolve_json(&self) -> Result<serde_json::Value, EvolutionError> {
        let _guard = self.evolve_lock.lock().await;
        let out = run_evolution(&self.evolution).await?;

        let restart = out.restart_required;
        let json = json!({
            "status": "completed",
            "session": out.session,
            "transcript": out.transcript,
            "tokens": {
                "input": out.usage.input,
                "output": out.usage.output,
            },
            "warnings": out.warnings,
            "restart_required": restart,
        });

        // Signal shutdown after returning the response — the caller receives the
        // JSON before the server begins its graceful shutdown.
        if restart {
            let tx = self.shutdown_tx.clone();
            tokio::spawn(async move {
                // Small delay so the HTTP response finishes flushing.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let _ = tx.send(true);
            });
        }

        Ok(json)
    }

    /// Same as `POST /automatic`.
    pub async fn automatic_start_json(&self) -> Result<serde_json::Value, AutomationError> {
        self.automation.start(self.pi.clone()).await?;
        Ok(json!({
            "status": "running",
            "message": "boss-level automatic mode started (timetable loop is a placeholder until Pi/routing integration)",
        }))
    }

    /// Same as `POST /stop`.
    pub async fn automatic_stop_json(&self) -> Result<serde_json::Value, AutomationError> {
        self.automation.stop().await?;
        Ok(json!({
            "status": "stopped",
            "message": "automation stopped; train positions restored from snapshot taken at /automatic start",
        }))
    }

    /// Same as `GET /pi/status`.
    pub async fn pi_status_json(&self) -> Result<serde_json::Value, crate::pi_client::PiError> {
        let status = self.pi.status().await?;
        Ok(serde_json::to_value(status).unwrap_or_else(|e| json!({"error": e.to_string()})))
    }

    /// Same as `GET /pi/health`.
    pub async fn pi_health_json(&self) -> Result<serde_json::Value, crate::pi_client::PiError> {
        let health = self.pi.health().await?;
        Ok(serde_json::to_value(health).unwrap_or_else(|e| json!({"error": e.to_string()})))
    }

    /// Same as `GET /pi/sensors`.
    pub async fn pi_sensors_list_json(
        &self,
    ) -> Result<serde_json::Value, crate::pi_client::PiError> {
        let sensors = self.pi.sensors().await?;
        Ok(serde_json::to_value(sensors).unwrap_or_else(|e| json!({"error": e.to_string()})))
    }

    pub async fn pi_track_speed_json(
        &self,
        id: u8,
        direction: TrackDirection,
        speed: u8,
    ) -> Result<serde_json::Value, crate::pi_client::PiError> {
        self.pi.set_track_speed(id, direction, speed).await?;
        Ok(json!({
            "status": "ok",
            "track": id,
            "direction": direction.to_string(),
            "speed": speed,
        }))
    }

    pub async fn pi_track_stop_json(
        &self,
        id: u8,
    ) -> Result<serde_json::Value, crate::pi_client::PiError> {
        self.pi.stop_track(id).await?;
        Ok(json!({
            "status": "ok",
            "track": id,
            "action": "stopped",
        }))
    }

    pub async fn pi_all_stop_json(&self) -> Result<serde_json::Value, crate::pi_client::PiError> {
        self.pi.all_stop().await?;
        Ok(json!({
            "status": "ok",
            "action": "all_stop",
        }))
    }

    pub async fn pi_point_json(
        &self,
        id: u8,
        direction: PointDirection,
    ) -> Result<serde_json::Value, crate::pi_client::PiError> {
        self.pi.set_point(id, direction).await?;
        Ok(json!({
            "status": "ok",
            "point": id,
            "direction": direction.to_string(),
        }))
    }

    pub async fn pi_sensor_json(
        &self,
        id: u8,
        value: bool,
    ) -> Result<serde_json::Value, crate::pi_client::PiError> {
        self.pi.set_sensor(id, value).await?;
        Ok(json!({
            "status": "ok",
            "sensor": id,
            "value": value,
        }))
    }

    pub async fn pi_sensors_reset_json(
        &self,
    ) -> Result<serde_json::Value, crate::pi_client::PiError> {
        self.pi.reset_sensors().await?;
        Ok(json!({
            "status": "ok",
            "action": "sensors_reset",
        }))
    }
}

/// Same as `POST /initialise` (no `AppState` required).
pub fn initialise_json(body: InitialiseRequest) -> Result<serde_json::Value, StateError> {
    body.validate()?;
    body.save(&state::trains_path())?;
    Ok(json!({
        "status": "ok",
        "trains": body.trains.len(),
    }))
}

/// Same as `POST /program`.
pub fn program_json(payload: serde_json::Value) -> Result<serde_json::Value, StateError> {
    state::save_program_placeholder(&payload)?;
    Ok(json!({
        "status": "accepted",
        "message": "reserved for future track program; payload stored under data/runtime/program.json",
    }))
}

/// Same as `POST /route`: compute routes for trains with destinations.
pub fn route_json(body: InitialiseRequest) -> Result<serde_json::Value, route_planner::PlanError> {
    body.validate()
        .map_err(|e| route_planner::PlanError::Layout(e.to_string()))?;
    let plans = route_planner::plan_routes(&body.trains)?;
    Ok(json!({
        "status": "ok",
        "routes": plans,
    }))
}

impl AppState {
    /// Execute a set of planned routes by sending commands to the Pi hardware.
    pub async fn execute_routes_json(
        &self,
        body: InitialiseRequest,
    ) -> Result<serde_json::Value, String> {
        body.validate().map_err(|e| e.to_string())?;
        let plans = route_planner::plan_routes(&body.trains).map_err(|e| e.to_string())?;

        let mut results = Vec::new();
        for plan in &plans {
            let mut cmd_results = Vec::new();
            for cmd in &plan.commands {
                let result = train_controller::execute_command(&self.pi, cmd).await;
                cmd_results.push(json!({
                    "command": cmd.to_string(),
                    "success": result.is_ok(),
                    "error": result.err().map(|e| e.to_string()),
                }));
            }
            results.push(json!({
                "train_index": plan.train_index,
                "from_sensor": plan.from_sensor,
                "to_sensor": plan.to_sensor,
                "description": plan.description,
                "commands_executed": cmd_results,
            }));
        }

        Ok(json!({
            "status": "executed",
            "routes": results,
        }))
    }
}

async fn health_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.health_json().await)
}

/// Relative path to the evolution journal (same file the agent appends to).
pub const JOURNAL_FILE: &str = "JOURNAL.md";

/// Relative path to the planned curriculum.
pub const ROADMAP_FILE: &str = "ROADMAP.md";

/// JSON body for [`GET /journal`](journal_handler) (kept pure for tests).
pub fn journal_response(text: &str) -> serde_json::Value {
    json!({
        "path": JOURNAL_FILE,
        "text": text,
    })
}

/// JSON body for [`GET /roadmap`](roadmap_handler) (kept pure for tests).
pub fn roadmap_response(text: &str) -> serde_json::Value {
    json!({
        "path": ROADMAP_FILE,
        "text": text,
    })
}

async fn markdown_file_handler<F>(
    path: &'static str,
    to_json: F,
) -> Result<Json<serde_json::Value>, (StatusCode, String)>
where
    F: Fn(&str) -> serde_json::Value,
{
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Json(to_json(&text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err((StatusCode::NOT_FOUND, format!("{path} not found")))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn journal_handler() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    markdown_file_handler(JOURNAL_FILE, journal_response).await
}

async fn roadmap_handler() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    markdown_file_handler(ROADMAP_FILE, roadmap_response).await
}

async fn evolve_handler(State(state): State<AppState>) -> Response {
    match state.evolve_json().await {
        Ok(j) => Json(j).into_response(),
        Err(e) => evolve_error_response(e),
    }
}

fn evolve_error_response(e: EvolutionError) -> Response {
    let status = match &e {
        EvolutionError::PreflightFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        EvolutionError::PostCheckFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        EvolutionError::Agent(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, e.to_string()).into_response()
}

async fn initialise_handler(
    State(_state): State<AppState>,
    Json(body): Json<InitialiseRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    initialise_json(body)
        .map_err(|e: StateError| {
            let code = match &e {
                StateError::TooManyTrains(_) | StateError::InvalidSensor(_) => {
                    StatusCode::BAD_REQUEST
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (code, e.to_string())
        })
        .map(Json)
}

async fn program_handler(
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    program_json(payload)
        .map_err(|e: StateError| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        .map(Json)
}

async fn route_handler(
    Json(body): Json<InitialiseRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    route_json(body)
        .map_err(|e| {
            let code = match &e {
                route_planner::PlanError::NoDestination { .. } => StatusCode::BAD_REQUEST,
                route_planner::PlanError::NoRoute { .. } => StatusCode::NOT_FOUND,
                route_planner::PlanError::Layout(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (code, e.to_string())
        })
        .map(Json)
}

async fn route_execute_handler(
    State(state): State<AppState>,
    Json(body): Json<InitialiseRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .execute_routes_json(body)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
        .map(Json)
}

async fn automatic_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .automatic_start_json()
        .await
        .map_err(|e: AutomationError| automation_status(&e))
        .map(Json)
}

async fn stop_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .automatic_stop_json()
        .await
        .map_err(|e: AutomationError| automation_status(&e))
        .map(Json)
}

fn automation_status(e: &AutomationError) -> (StatusCode, String) {
    let code = match e {
        AutomationError::AlreadyRunning => StatusCode::CONFLICT,
        AutomationError::NotRunning => StatusCode::CONFLICT,
        AutomationError::MissingInitialiseFile => StatusCode::BAD_REQUEST,
        AutomationError::State(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string())
}

// --- Pi proxy endpoints ---------------------------------------------------

async fn pi_status_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_status_json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        .map(Json)
}

async fn pi_health_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_health_json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        .map(Json)
}

async fn pi_sensors_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_sensors_list_json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        .map(Json)
}

// --- Pi control endpoints --------------------------------------------------

/// Query params for `POST /pi/track/:id/speed`.
#[derive(Debug, Deserialize)]
struct TrackSpeedParams {
    direction: TrackDirection,
    speed: u8,
}

async fn pi_track_speed_handler(
    State(state): State<AppState>,
    Path(id): Path<u8>,
    Query(params): Query<TrackSpeedParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_track_speed_json(id, params.direction, params.speed)
        .await
        .map_err(pi_control_error)
        .map(Json)
}

async fn pi_track_stop_handler(
    State(state): State<AppState>,
    Path(id): Path<u8>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_track_stop_json(id)
        .await
        .map_err(pi_control_error)
        .map(Json)
}

async fn pi_allstop_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_all_stop_json()
        .await
        .map_err(pi_control_error)
        .map(Json)
}

/// Query params for `POST /pi/point/:id`.
#[derive(Debug, Deserialize)]
struct PointParams {
    direction: PointDirection,
}

async fn pi_point_handler(
    State(state): State<AppState>,
    Path(id): Path<u8>,
    Query(params): Query<PointParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_point_json(id, params.direction)
        .await
        .map_err(pi_control_error)
        .map(Json)
}

/// Query params for `POST /pi/sensor/:id`.
#[derive(Debug, Deserialize)]
struct SensorParams {
    value: bool,
}

async fn pi_sensor_handler(
    State(state): State<AppState>,
    Path(id): Path<u8>,
    Query(params): Query<SensorParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_sensor_json(id, params.value)
        .await
        .map_err(pi_control_error)
        .map(Json)
}

async fn pi_sensors_reset_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi_sensors_reset_json()
        .await
        .map_err(pi_control_error)
        .map(Json)
}

/// Map PiError to HTTP status codes.
fn pi_control_error(e: crate::pi_client::PiError) -> (StatusCode, String) {
    use crate::pi_client::PiError;
    let code = match &e {
        PiError::InvalidParam(_) => StatusCode::BAD_REQUEST,
        PiError::Unreachable(_) => StatusCode::BAD_GATEWAY,
        PiError::ApiError(_) => StatusCode::BAD_GATEWAY,
        PiError::BadResponse(_) => StatusCode::BAD_GATEWAY,
    };
    (code, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_response_includes_path_and_text() {
        let v = journal_response("Day 0\n");
        assert_eq!(v["path"], JOURNAL_FILE);
        assert_eq!(v["text"], "Day 0\n");
    }

    #[test]
    fn roadmap_response_includes_path_and_text() {
        let v = roadmap_response("# Roadmap\n");
        assert_eq!(v["path"], ROADMAP_FILE);
        assert_eq!(v["text"], "# Roadmap\n");
    }
}
