//! Library crate for yoyo: track layout, HTTP service, and evolution session logic.
//!
//! The binary entry point is `src/main.rs` (REPL or `--serve`).

pub mod agent_runner;
pub mod automation;
pub mod evolve_session;
pub mod layout;
pub mod pi_client;
pub mod prompts;
pub mod route_planner;
pub mod service;
pub mod state;
