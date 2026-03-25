//! Boss-level automatic operation (ROADMAP “Prove It”): timetable loop until stopped.
//!
//! Hardware integration (Pi API, routing, collision avoidance) is still TODO; the loop is a
//! cancellable placeholder that ticks until `/stop`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::state::{self, InitialiseRequest, StateError};
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
    pub async fn start(&self) -> Result<(), AutomationError> {
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

        let join = tokio::spawn(async move {
            automatic_loop(flag).await;
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

/// Placeholder: real implementation will read `data/track_layout.toml`, call the Pi API, run
/// collision-free routing, station dwell times, etc. (ROADMAP Boss Level).
async fn automatic_loop(cancel: Arc<AtomicBool>) {
    let mut tick: u64 = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        tick = tick.wrapping_add(1);
        // Short sleep so `/stop` remains responsive.
        tokio::time::sleep(Duration::from_millis(500)).await;
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        // Placeholder: one “timetable” step per second (log every 10 s to avoid noise).
        if tick != 0 && tick.is_multiple_of(20) {
            eprintln!(
                "yoyo: automatic mode tick {tick} (placeholder — integrate Pi + routing here)"
            );
        }
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
