//! HTTP service: `/evolve`, `/initialise`, `/program`, `/automatic`, `/stop`, `/health`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Mutex;

use crate::automation::AutomationController;
use crate::automation::AutomationError;
use crate::evolve_session::{run_evolution, EvolutionConfig, EvolutionError};
use crate::state::{self, InitialiseRequest, StateError};

/// Shared service state (API keys and evolution config come from environment at startup).
#[derive(Clone)]
pub struct AppState {
    pub evolve_lock: Arc<Mutex<()>>,
    pub evolution: EvolutionConfig,
    pub automation: Arc<AutomationController>,
}

pub async fn serve(bind: SocketAddr, state: AppState) -> Result<(), std::io::Error> {
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/evolve", post(evolve_handler))
        .route("/initialise", post(initialise_handler))
        .route("/program", post(program_handler))
        .route("/automatic", post(automatic_handler))
        .route("/stop", post(stop_handler))
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
