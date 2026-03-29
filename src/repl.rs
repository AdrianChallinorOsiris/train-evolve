//! REPL command dispatch: handles `/health`, `/pi …`, `/initialise …`, etc.
//!
//! Extracted from `main.rs` to keep each module focused and readable.

use yoyo::pi_client::{PointDirection, TrackDirection};
use yoyo::service::{
    initialise_json, journal_response, program_json, roadmap_response, route_json, AppState,
    JOURNAL_FILE, ROADMAP_FILE,
};
use yoyo::state::{InitialiseRequest, RouteRequest};

// ANSI colors (shared with main.rs)
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";

// ---------------------------------------------------------------------------
// Pretty-print JSON
// ---------------------------------------------------------------------------

pub fn print_json_pretty(v: &serde_json::Value) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{}", v),
    }
}

// ---------------------------------------------------------------------------
// REPL /help text (single source of truth)
// ---------------------------------------------------------------------------

pub fn print_repl_help() {
    println!("\n{BOLD}  REPL:{RESET}");
    println!("  {GREEN}/help{RESET}               This list");
    println!("  {GREEN}/clear{RESET}              Clear conversation history");
    println!("  {GREEN}/model <name>{RESET}       Switch model (clears conversation)");
    println!("  {GREEN}/quit{RESET}               Exit");
    println!("\n{BOLD}  Service (same as HTTP --serve):{RESET}");
    println!("  {GREEN}/health{RESET}  {GREEN}/journal{RESET}  {GREEN}/roadmap{RESET}  {GREEN}/evolve{RESET}  {GREEN}/automatic{RESET}  {GREEN}/automatic/status{RESET}  {GREEN}/stop{RESET}");
    println!(
        "  {GREEN}/initialise <json>{RESET}   register trains — e.g. {{\"trains\":[{{\"train\":1,\"sensor\":21}},{{\"train\":2,\"sensor\":22,\"direction\":\"bwd\"}}]}}"
    );
    println!("  {DIM}                        train: id (≥1, unique)  sensor: position (1-24)  direction: fwd|bwd (default fwd)  max 6 trains{RESET}");
    println!("  {GREEN}/program <json>{RESET}      track program placeholder JSON");
    println!(
        "  {GREEN}/route <json>{RESET}        route to station — e.g. {{\"trains\":[{{\"train\":1,\"sensor\":1,\"station\":\"waterloo\",\"arrival\":\"FWD\"}}]}}"
    );
    println!("\n{BOLD}  Pi hardware:{RESET}");
    println!("  {GREEN}/pi status{RESET}  {GREEN}/pi health{RESET}  {GREEN}/pi sensors{RESET}  {GREEN}/pi sensors reset{RESET}");
    println!("  {GREEN}/pi track speed <id> OFF|FWD|BCK <0-100>{RESET}");
    println!("  {GREEN}/pi track stop <id>{RESET}");
    println!("  {GREEN}/pi allstop{RESET}");
    println!("  {GREEN}/pi point <id> THRU|BRANCH{RESET}");
    println!("  {GREEN}/pi sensor <id> true|false{RESET}");
    println!();
    println!("  {DIM}Ctrl+C during a response cancels the current turn.{RESET}");
    println!("  {DIM}Other input is sent to the agent.{RESET}");
    println!("  {DIM}Full list: run `yoyo --help`{RESET}\n");
}

// ---------------------------------------------------------------------------
// Parser helpers
// ---------------------------------------------------------------------------

fn parse_track_direction(s: &str) -> Result<TrackDirection, String> {
    match s.to_ascii_uppercase().as_str() {
        "OFF" => Ok(TrackDirection::Off),
        "FWD" => Ok(TrackDirection::Fwd),
        "BCK" => Ok(TrackDirection::Bck),
        _ => Err(format!("expected OFF|FWD|BCK, got {s:?}")),
    }
}

fn parse_point_direction(s: &str) -> Result<PointDirection, String> {
    match s.to_ascii_uppercase().as_str() {
        "THRU" => Ok(PointDirection::Thru),
        "BRANCH" => Ok(PointDirection::Branch),
        _ => Err(format!("expected THRU|BRANCH, got {s:?}")),
    }
}

fn parse_bool_word(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("expected true|false, got {s:?}")),
    }
}

// ---------------------------------------------------------------------------
// /pi … sub-dispatch
// ---------------------------------------------------------------------------

async fn pi_dispatch(line: &str, state: &AppState) -> Result<(), String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.first().copied() != Some("/pi") {
        return Err("expected line to start with /pi".into());
    }
    if parts.len() < 2 {
        return Err("/pi: add a subcommand (status, health, sensors, track, …)".into());
    }
    match parts[1] {
        "status" => {
            let v = state.pi_status_json().await.map_err(|e| e.to_string())?;
            print_json_pretty(&v);
            Ok(())
        }
        "health" => {
            let v = state.pi_health_json().await.map_err(|e| e.to_string())?;
            print_json_pretty(&v);
            Ok(())
        }
        "sensors" => {
            if parts.len() == 3 && parts[2] == "reset" {
                let v = state
                    .pi_sensors_reset_json()
                    .await
                    .map_err(|e| e.to_string())?;
                print_json_pretty(&v);
            } else if parts.len() == 2 {
                let v = state
                    .pi_sensors_list_json()
                    .await
                    .map_err(|e| e.to_string())?;
                print_json_pretty(&v);
            } else {
                return Err(
                    "/pi sensors [reset] — use `/pi sensors` or `/pi sensors reset`".into(),
                );
            }
            Ok(())
        }
        "track" => pi_track_dispatch(&parts, state).await,
        "allstop" => {
            let v = state.pi_all_stop_json().await.map_err(|e| e.to_string())?;
            print_json_pretty(&v);
            Ok(())
        }
        "point" => {
            if parts.len() < 4 {
                return Err("/pi point <id> THRU|BRANCH".into());
            }
            let id = parts[2]
                .parse::<u8>()
                .map_err(|_| format!("invalid point id {:?}", parts[2]))?;
            let dir = parse_point_direction(parts[3])?;
            let v = state
                .pi_point_json(id, dir)
                .await
                .map_err(|e| e.to_string())?;
            print_json_pretty(&v);
            Ok(())
        }
        "sensor" => {
            if parts.len() < 4 {
                return Err("/pi sensor <id> true|false".into());
            }
            let id = parts[2]
                .parse::<u8>()
                .map_err(|_| format!("invalid sensor id {:?}", parts[2]))?;
            let value = parse_bool_word(parts[3])?;
            let v = state
                .pi_sensor_json(id, value)
                .await
                .map_err(|e| e.to_string())?;
            print_json_pretty(&v);
            Ok(())
        }
        other => Err(format!("unknown /pi subcommand {other:?}")),
    }
}

/// Handle `/pi track speed …` and `/pi track stop …`.
async fn pi_track_dispatch(parts: &[&str], state: &AppState) -> Result<(), String> {
    if parts.len() >= 3 && parts[2] == "speed" {
        if parts.len() < 6 {
            return Err("/pi track speed <id> OFF|FWD|BCK <speed> — not enough arguments".into());
        }
        let id = parts[3]
            .parse::<u8>()
            .map_err(|_| format!("invalid track id {:?}", parts[3]))?;
        let dir = parse_track_direction(parts[4])?;
        let speed = parts[5]
            .parse::<u8>()
            .map_err(|_| format!("invalid speed {:?}", parts[5]))?;
        let v = state
            .pi_track_speed_json(id, dir, speed)
            .await
            .map_err(|e| e.to_string())?;
        print_json_pretty(&v);
        Ok(())
    } else if parts.len() >= 3 && parts[2] == "stop" {
        if parts.len() < 4 {
            return Err("/pi track stop <id>".into());
        }
        let id = parts[3]
            .parse::<u8>()
            .map_err(|_| format!("invalid track id {:?}", parts[3]))?;
        let v = state
            .pi_track_stop_json(id)
            .await
            .map_err(|e| e.to_string())?;
        print_json_pretty(&v);
        Ok(())
    } else {
        Err("/pi track speed … or /pi track stop …".into())
    }
}

// ---------------------------------------------------------------------------
// Top-level service dispatch (all slash commands except /quit, /clear, /model, /help)
// ---------------------------------------------------------------------------

/// Returns `true` if `line` was a service command (including invalid ones we reported).
pub async fn service_dispatch(line: &str, state: &AppState) -> bool {
    let line = line.trim();
    match line {
        "/health" => {
            let v = state.health_json().await;
            print_json_pretty(&v);
            true
        }
        "/journal" => {
            match tokio::fs::read_to_string(JOURNAL_FILE).await {
                Ok(t) => print_json_pretty(&journal_response(&t)),
                Err(e) => eprintln!("{RED}  {JOURNAL_FILE}: {e}{RESET}"),
            }
            true
        }
        "/roadmap" => {
            match tokio::fs::read_to_string(ROADMAP_FILE).await {
                Ok(t) => print_json_pretty(&roadmap_response(&t)),
                Err(e) => eprintln!("{RED}  {ROADMAP_FILE}: {e}{RESET}"),
            }
            true
        }
        "/evolve" => {
            match state.evolve_json().await {
                Ok(v) => print_json_pretty(&v),
                Err(e) => eprintln!("{RED}  evolve failed: {e}{RESET}"),
            }
            true
        }
        "/automatic" => {
            match state.automatic_start_json().await {
                Ok(v) => print_json_pretty(&v),
                Err(e) => eprintln!("{RED}  automatic: {e}{RESET}"),
            }
            true
        }
        "/automatic/status" => {
            let v = state.automatic_status_json().await;
            print_json_pretty(&v);
            true
        }
        "/stop" => {
            match state.automatic_stop_json().await {
                Ok(v) => print_json_pretty(&v),
                Err(e) => eprintln!("{RED}  stop: {e}{RESET}"),
            }
            true
        }
        s if s.starts_with("/initialise") => {
            dispatch_json_command(s, "/initialise", |rest| {
                match serde_json::from_str::<InitialiseRequest>(rest) {
                    Ok(body) => match initialise_json(body) {
                        Ok(v) => print_json_pretty(&v),
                        Err(e) => eprintln!("{RED}  initialise: {e}{RESET}"),
                    },
                    Err(e) => eprintln!("{RED}  invalid JSON: {e}{RESET}"),
                }
            });
            true
        }
        s if s.starts_with("/program") => {
            dispatch_json_command(s, "/program", |rest| {
                match serde_json::from_str::<serde_json::Value>(rest) {
                    Ok(payload) => match program_json(payload) {
                        Ok(v) => print_json_pretty(&v),
                        Err(e) => eprintln!("{RED}  program: {e}{RESET}"),
                    },
                    Err(e) => eprintln!("{RED}  invalid JSON: {e}{RESET}"),
                }
            });
            true
        }
        s if s.starts_with("/route") => {
            dispatch_json_command(s, "/route", |rest| {
                match serde_json::from_str::<RouteRequest>(rest) {
                    Ok(body) => match route_json(body) {
                        Ok(v) => print_json_pretty(&v),
                        Err(e) => eprintln!("{RED}  route: {e}{RESET}"),
                    },
                    Err(e) => eprintln!("{RED}  invalid JSON: {e}{RESET}"),
                }
            });
            true
        }
        s if s.starts_with("/pi") => {
            match pi_dispatch(s, state).await {
                Ok(()) => {}
                Err(e) => eprintln!("{RED}  {e}{RESET}"),
            }
            true
        }
        _ => false,
    }
}

/// Helper: strip a command prefix, check for non-empty JSON body, call handler.
fn dispatch_json_command<F>(line: &str, prefix: &str, handler: F)
where
    F: FnOnce(&str),
{
    let rest = line[prefix.len()..].trim();
    if rest.is_empty() {
        eprintln!("{RED}  usage: {prefix} <JSON>{RESET}");
        return;
    }
    handler(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_track_direction_fwd() {
        assert_eq!(parse_track_direction("fwd").unwrap(), TrackDirection::Fwd);
        assert_eq!(parse_track_direction("FWD").unwrap(), TrackDirection::Fwd);
    }

    #[test]
    fn parse_track_direction_bad() {
        assert!(parse_track_direction("SIDEWAYS").is_err());
    }

    #[test]
    fn parse_point_direction_thru() {
        assert_eq!(parse_point_direction("thru").unwrap(), PointDirection::Thru);
        assert_eq!(
            parse_point_direction("BRANCH").unwrap(),
            PointDirection::Branch
        );
    }

    #[test]
    fn parse_point_direction_bad() {
        assert!(parse_point_direction("LEFT").is_err());
    }

    #[test]
    fn parse_bool_word_true() {
        assert!(parse_bool_word("true").unwrap());
        assert!(parse_bool_word("1").unwrap());
        assert!(parse_bool_word("yes").unwrap());
    }

    #[test]
    fn parse_bool_word_false() {
        assert!(!parse_bool_word("false").unwrap());
        assert!(!parse_bool_word("0").unwrap());
        assert!(!parse_bool_word("no").unwrap());
    }

    #[test]
    fn parse_bool_word_bad() {
        assert!(parse_bool_word("maybe").is_err());
    }
}
