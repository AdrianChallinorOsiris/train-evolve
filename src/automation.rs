//! Boss-level automatic operation (ROADMAP "Prove It"): timetable loop until stopped.
//!
//! When `/automatic` is posted, the controller loads train positions from the INITIALISE file,
//! builds a [`TrainController`](crate::train_controller::TrainController), and runs a continuous
//! routing loop: pick destinations, plan routes (avoiding collisions), execute on the Pi,
//! poll sensors, dwell at stations. `/stop` cancels the loop and restores train positions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::pi_client::PiClient;
use crate::state::{self, InitialiseRequest, StateError};
use crate::train_controller;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error("automatic mode is already running")]
    AlreadyRunning,
    #[error("automatic mode is not running")]
    NotRunning,
    #[error("no train state file; POST /initialise before /automatic")]
    MissingInitialiseFile,
    #[error(transparent)]
    State(#[from] StateError),
}

struct AutomationInner {
    /// Background task for the timetable loop.
    join: Option<tokio::task::JoinHandle<()>>,
    /// Set to `true` to exit the loop (from `/stop`).
    cancel: Option<Arc<AtomicBool>>,
}

/// Shared controller for `/automatic` and `/stop`.
pub struct AutomationController {
    inner: Mutex<AutomationInner>,
}

impl Default for AutomationController {
    fn default() -> Self {
        Self::new()
    }
}

impl AutomationController {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(AutomationInner {
                join: None,
                cancel: None,
            }),
        }
    }

    /// Start boss-level automation: requires INITIALISE data; saves a snapshot for `/stop`.
    pub async fn start(&self, pi: Arc<PiClient>) -> Result<(), AutomationError> {
        let mut inner = self.inner.lock().await;
        if let Some(ref h) = inner.join {
            if !h.is_finished() {
                return Err(AutomationError::AlreadyRunning);
            }
        }

        let init = InitialiseRequest::load(&state::trains_path())?
            .ok_or(AutomationError::MissingInitialiseFile)?;
        init.validate()?;

        init.save(&state::automatic_start_path())?;

        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let trains = init.trains.clone();

        let join = tokio::spawn(async move {
            if let Err(e) = train_controller::run_automatic(pi, trains, flag).await {
                eprintln!("yoyo: automatic mode error: {e}");
            }
        });

        inner.join = Some(join);
        inner.cancel = Some(cancel);
        Ok(())
    }

    /// Stop automation and restore train positions from the snapshot taken at `/automatic` start.
    pub async fn stop(&self) -> Result<(), AutomationError> {
        let mut inner = self.inner.lock().await;

        let cancel = inner.cancel.take().ok_or(AutomationError::NotRunning)?;
        let join = inner.join.take().ok_or(AutomationError::NotRunning)?;

        cancel.store(true, Ordering::SeqCst);
        let _ = join.await;

        if let Some(snapshot) = InitialiseRequest::load(&state::automatic_start_path())? {
            snapshot.save(&state::trains_path())?;
        }

        Ok(())
    }

    pub async fn is_running(&self) -> bool {
        let inner = self.inner.lock().await;
        inner
            .join
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_without_start_errors() {
        let c = AutomationController::new();
        assert!(matches!(c.stop().await, Err(AutomationError::NotRunning)));
    }
}
