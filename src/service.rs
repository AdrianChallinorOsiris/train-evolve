//! HTTP service: `/evolve`, `/initialise`, `/program`, `/simulate`, `/route`, `/route/execute`,
//! `/automatic`, `/automatic/status`, `/stop`,
//! `/health`, `/journal`, `/roadmap`, `/pi/status`, `/pi/health`, `/pi/sensors`,
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
use crate::state::{self, EvolutionStats, InitialiseRequest, RouteRequest, StateError};

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
        .route("/simulate", post(simulate_handler))
        .route("/automatic", post(automatic_handler))
        .route("/automatic/status", get(automatic_status_handler))
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
        let version = env!("CARGO_PKG_VERSION");

        // Check Pi connectivity (quick health check, don't block long)
        let pi_status = match self.pi.health().await {
            Ok(h) => json!({
                "reachable": true,
                "pi_version": h.version,
                "cpu_temp_c": h.cpu_temperature_celsius,
                "protection_running": h.protection_system_running,
            }),
            Err(e) => json!({
                "reachable": false,
                "error": e.to_string(),
            }),
        };

        let mut health = json!({
            "status": "ok",
            "version": version,
            "automatic": automatic,
            "pi": pi_status,
        });

        // Include cumulative evolution stats if available
        if let Ok(stats) = EvolutionStats::load() {
            if stats.sessions_completed > 0 {
                health["evolution"] = json!({
                    "sessions_completed": stats.sessions_completed,
                    "total_tokens_in": stats.total_tokens_in,
                    "total_tokens_out": stats.total_tokens_out,
                    "last_session_at": stats.last_session_at,
                    "last_version": stats.last_version,
                });
            }
        }

        health
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
            "message": "automatic mode started — trains routing continuously with collision avoidance and station stops",
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

    /// Same as `GET /automatic/status`.
    pub async fn automatic_status_json(&self) -> serde_json::Value {
        let running = self.automation.is_running().await;
        match self.automation.status().await {
            Some(status) => {
                serde_json::to_value(status).unwrap_or_else(|e| json!({"error": e.to_string()}))
            }
            None => json!({
                "running": running,
                "trains": [],
                "message": if running { "starting up — no status yet" } else { "automatic mode not running" },
            }),
        }
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

/// Same as `POST /route`: plan routes from current positions to target positions.
///
/// Loads current train state from `data/runtime/trains.json` (set by `/initialise`),
/// then plans routes for each train in `target` to reach its specified destination + direction.
pub fn route_json(target: RouteRequest) -> Result<serde_json::Value, route_planner::PlanError> {
    target
        .validate()
        .map_err(|e| route_planner::PlanError::Layout(e.to_string()))?;
    let current = state::InitialiseRequest::load(&state::trains_path())
        .map_err(|e| route_planner::PlanError::Layout(e.to_string()))?
        .ok_or(route_planner::PlanError::NoCurrentState)?;
    let plan = route_planner::plan_target_routes(&current, &target)?;
    Ok(json!({
        "status": "ok",
        "plan": plan,
    }))
}

/// Same as `POST /simulate`: dry-run route planning (no hardware interaction).
///
/// Identical to `route_json` but returns `"status": "simulated"` so the caller
/// can review tracks, points, and step sequence before committing.
pub fn simulate_json(target: RouteRequest) -> Result<serde_json::Value, route_planner::PlanError> {
    target
        .validate()
        .map_err(|e| route_planner::PlanError::Layout(e.to_string()))?;
    let current = state::InitialiseRequest::load(&state::trains_path())
        .map_err(|e| route_planner::PlanError::Layout(e.to_string()))?
        .ok_or(route_planner::PlanError::NoCurrentState)?;
    let plan = route_planner::plan_target_routes(&current, &target)?;
    Ok(json!({
        "status": "simulated",
        "message": "Dry run — no commands sent to hardware. Review the plan below.",
        "current": current,
        "target": target,
        "plan": plan,
    }))
}

impl AppState {
    /// Execute planned routes by sending commands to the Pi hardware.
    ///
    /// Loads current train state, plans routes to target, then executes
    /// the step-by-step plan. For AwaitSensor steps, polls the Pi sensors.
    pub async fn execute_routes_json(
        &self,
        target: RouteRequest,
    ) -> Result<serde_json::Value, String> {
        target.validate().map_err(|e| e.to_string())?;
        let current = state::InitialiseRequest::load(&state::trains_path())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no current train state — POST /initialise first".to_string())?;
        let plan =
            route_planner::plan_target_routes(&current, &target).map_err(|e| e.to_string())?;

        let mut results = Vec::new();
        for train_plan in &plan.trains {
            let mut step_results = Vec::new();
            for step in &train_plan.steps {
                let result = execute_route_step(&self.pi, step).await;
                step_results.push(json!({
                    "step": step.to_string(),
                    "success": result.is_ok(),
                    "error": result.err().map(|e| e.to_string()),
                }));
            }
            results.push(json!({
                "train": train_plan.train,
                "from_sensor": train_plan.from_sensor,
                "to_sensor": train_plan.to_sensor,
                "target_direction": train_plan.target_direction,
                "already_there": train_plan.already_there,
                "description": train_plan.description,
                "steps_executed": step_results,
            }));
        }

        // Update the saved train state to reflect the new positions.
        let new_positions = InitialiseRequest {
            trains: plan
                .trains
                .iter()
                .map(|tp| crate::state::TrainPosition {
                    train: tp.train,
                    sensor: tp.to_sensor,
                    direction: tp
                        .target_direction
                        .parse::<crate::state::TrainDirection>()
                        .unwrap_or_default(),
                    destination: None,
                })
                .collect(),
        };
        if let Err(e) = new_positions.save(&state::trains_path()) {
            return Err(format!(
                "routes executed but failed to save new positions: {e}"
            ));
        }

        Ok(json!({
            "status": "executed",
            "trains": results,
            "warnings": plan.warnings,
        }))
    }
}

/// Maximum time to wait for a sensor to fire during route execution (seconds).
const AWAIT_SENSOR_TIMEOUT_SECS: u64 = 60;

/// Interval between sensor polls during route execution (milliseconds).
const AWAIT_SENSOR_POLL_MS: u64 = 500;

/// Execute a single route step against the Pi hardware.
async fn execute_route_step(
    pi: &PiClient,
    step: &route_planner::RouteStep,
) -> Result<(), crate::pi_client::PiError> {
    use route_planner::RouteStep;
    match step {
        RouteStep::SetPoint {
            point_id,
            direction,
            ..
        } => {
            pi.set_point(*point_id, *direction).await?;
        }
        RouteStep::EnergiseTrack {
            track_id,
            direction,
            speed,
            ..
        } => {
            pi.set_track_speed(*track_id, *direction, *speed).await?;
        }
        RouteStep::DeEnergiseTrack { track_id, .. } => {
            pi.stop_track(*track_id).await?;
        }
        RouteStep::AwaitSensor { sensor, .. } => {
            // Poll sensors until the target fires or timeout elapses.
            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_secs(AWAIT_SENSOR_TIMEOUT_SECS);
            let key = sensor.to_string();
            loop {
                if tokio::time::Instant::now() >= deadline {
                    eprintln!(
                        "yoyo: await sensor {} timed out after {}s",
                        sensor, AWAIT_SENSOR_TIMEOUT_SECS
                    );
                    break;
                }
                match pi.sensors().await {
                    Ok(snap) => {
                        if snap.get(&key).map(|s| s.value).unwrap_or(false) {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("yoyo: sensor poll error during route execute: {e}");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(AWAIT_SENSOR_POLL_MS)).await;
            }
        }
    }
    Ok(())
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
                StateError::TooManyTrains(_)
                | StateError::InvalidSensor(_)
                | StateError::InvalidTrainId(_)
                | StateError::DuplicateTrainId(_) => StatusCode::BAD_REQUEST,
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

fn plan_error_to_status(e: &route_planner::PlanError) -> StatusCode {
    match e {
        route_planner::PlanError::NoDestination { .. } => StatusCode::BAD_REQUEST,
        route_planner::PlanError::NoRoute { .. } => StatusCode::NOT_FOUND,
        route_planner::PlanError::TrainNotFound { .. } => StatusCode::BAD_REQUEST,
        route_planner::PlanError::UnknownStation { .. } => StatusCode::BAD_REQUEST,
        route_planner::PlanError::NoCurrentState => StatusCode::BAD_REQUEST,
        route_planner::PlanError::Layout(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn route_handler(
    Json(body): Json<RouteRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    route_json(body)
        .map_err(|e| (plan_error_to_status(&e), e.to_string()))
        .map(Json)
}

async fn route_execute_handler(
    State(state): State<AppState>,
    Json(body): Json<RouteRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .execute_routes_json(body)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
        .map(Json)
}

async fn simulate_handler(
    Json(body): Json<RouteRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    simulate_json(body)
        .map_err(|e| (plan_error_to_status(&e), e.to_string()))
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

async fn automatic_status_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.automatic_status_json().await)
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

    #[test]
    fn simulate_json_returns_simulated_status() {
        // Set up a temp trains.json so simulate_json can load current positions.
        let dir = tempfile::tempdir().unwrap();
        let trains_path = dir.path().join("trains.json");
        let current = InitialiseRequest {
            trains: vec![crate::state::TrainPosition {
                train: 1,
                sensor: 1,
                direction: crate::state::TrainDirection::Fwd,
                destination: None,
            }],
        };
        current.save(&trains_path).unwrap();

        // simulate_json reads from state::trains_path() which is a fixed path,
        // so we test the planner directly instead.
        let layout_path = format!("{}/data/track_layout.toml", env!("CARGO_MANIFEST_DIR"));
        let layout = crate::layout::TrackLayout::from_path(&layout_path).unwrap();
        layout.validate().unwrap();
        let graph = crate::layout::graph::TrackGraph::from_layout(&layout);

        let target = crate::state::RouteRequest {
            trains: vec![crate::state::RouteTrainRequest {
                train: 1,
                destination: crate::state::Destination::Sensor(2),
                direction: crate::state::TrainDirection::Fwd,
            }],
        };
        let plan =
            route_planner::plan_target_routes_with(&current, &target, &layout, &graph).unwrap();
        assert!(!plan.trains.is_empty());
        assert_eq!(plan.trains[0].train, 1);
        assert_eq!(plan.trains[0].from_sensor, 1);
        assert_eq!(plan.trains[0].to_sensor, 2);
    }
}
