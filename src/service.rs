//! HTTP service: `/evolve`, `/initialise`, `/program`, `/automatic`, `/stop`, `/health`,
//! `/pi/status`, `/pi/health`, `/pi/sensors`,
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
use crate::state::{self, InitialiseRequest, StateError};

/// Shared service state (API keys and evolution config come from environment at startup).
#[derive(Clone)]
pub struct AppState {
    pub evolve_lock: Arc<Mutex<()>>,
    pub evolution: EvolutionConfig,
    pub automation: Arc<AutomationController>,
    pub pi: Arc<PiClient>,
}

pub async fn serve(bind: SocketAddr, state: AppState) -> Result<(), std::io::Error> {
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/evolve", post(evolve_handler))
        .route("/initialise", post(initialise_handler))
        .route("/program", post(program_handler))
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
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let automatic = state.automation.is_running().await;
    Json(json!({
        "status": "ok",
        "automatic": automatic,
    }))
}

async fn evolve_handler(State(state): State<AppState>) -> Response {
    let _guard = state.evolve_lock.lock().await;
    match run_evolution(&state.evolution).await {
        Ok(out) => Json(json!({
            "status": "completed",
            "session": out.session,
            "transcript": out.transcript,
            "tokens": {
                "input": out.usage.input,
                "output": out.usage.output,
            },
            "warnings": out.warnings,
        }))
        .into_response(),
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
    body.validate()
        .map_err(|e: StateError| (StatusCode::BAD_REQUEST, e.to_string()))?;
    body.save(&state::trains_path())
        .map_err(|e: StateError| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "status": "ok",
        "trains": body.trains.len(),
    })))
}

async fn program_handler(
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state::save_program_placeholder(&payload)
        .map_err(|e: StateError| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "status": "accepted",
        "message": "reserved for future track program; payload stored under data/runtime/program.json",
    })))
}

async fn automatic_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .automation
        .start()
        .await
        .map_err(|e: AutomationError| automation_status(&e))?;
    Ok(Json(json!({
        "status": "running",
        "message": "boss-level automatic mode started (timetable loop is a placeholder until Pi/routing integration)",
    })))
}

async fn stop_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .automation
        .stop()
        .await
        .map_err(|e: AutomationError| automation_status(&e))?;
    Ok(Json(json!({
        "status": "stopped",
        "message": "automation stopped; train positions restored from snapshot taken at /automatic start",
    })))
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
    let status = state
        .pi
        .status()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(
        serde_json::to_value(status).unwrap_or_else(|e| json!({"error": e.to_string()})),
    ))
}

async fn pi_health_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let health = state
        .pi
        .health()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(
        serde_json::to_value(health).unwrap_or_else(|e| json!({"error": e.to_string()})),
    ))
}

async fn pi_sensors_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let sensors = state
        .pi
        .sensors()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(
        serde_json::to_value(sensors).unwrap_or_else(|e| json!({"error": e.to_string()})),
    ))
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
        .pi
        .set_track_speed(id, params.direction, params.speed)
        .await
        .map_err(pi_control_error)?;
    Ok(Json(json!({
        "status": "ok",
        "track": id,
        "direction": params.direction.to_string(),
        "speed": params.speed,
    })))
}

async fn pi_track_stop_handler(
    State(state): State<AppState>,
    Path(id): Path<u8>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi
        .stop_track(id)
        .await
        .map_err(pi_control_error)?;
    Ok(Json(json!({
        "status": "ok",
        "track": id,
        "action": "stopped",
    })))
}

async fn pi_allstop_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi
        .all_stop()
        .await
        .map_err(pi_control_error)?;
    Ok(Json(json!({
        "status": "ok",
        "action": "all_stop",
    })))
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
        .pi
        .set_point(id, params.direction)
        .await
        .map_err(pi_control_error)?;
    Ok(Json(json!({
        "status": "ok",
        "point": id,
        "direction": params.direction.to_string(),
    })))
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
        .pi
        .set_sensor(id, params.value)
        .await
        .map_err(pi_control_error)?;
    Ok(Json(json!({
        "status": "ok",
        "sensor": id,
        "value": params.value,
    })))
}

async fn pi_sensors_reset_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .pi
        .reset_sensors()
        .await
        .map_err(pi_control_error)?;
    Ok(Json(json!({
        "status": "ok",
        "action": "sensors_reset",
    })))
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
