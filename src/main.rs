//! yoyo — a coding agent that evolves itself.
//!
//! ## Usage
//!
//! **HTTP service** (REST control; no cron required):
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-... cargo run -- --serve
//! ANTHROPIC_API_KEY=sk-... cargo run -- --serve --bind 0.0.0.0:8080
//! ```
//!
//! **Interactive REPL**:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-... cargo run -- --repl
//! ANTHROPIC_API_KEY=sk-... cargo run -- --repl --model claude-opus-4-6 --skills ./skills
//! ```
//!
//! REPL commands: `/quit`, `/exit`, `/clear`, `/model <name>`, plus the same
//! control endpoints as `--serve` (`/health`, `/evolve`, `/initialise`, …).
//! CLI: optional `--system <file>` for a custom system prompt; token usage shows
//! per-turn and cumulative session totals.

mod repl;

use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use yoagent::agent::Agent;
use yoagent::provider::AnthropicProvider;
use yoagent::skills::SkillSet;
use yoagent::tools::default_tools;
use yoagent::*;

use yoyo::agent_runner::format_api_error_for_user;
use yoyo::automation::AutomationController;
use yoyo::evolve_session::EvolutionConfig;
use yoyo::pi_client::PiClient;
use yoyo::pi_client::DEFAULT_PI_URL;
use yoyo::prompts::SYSTEM_PROMPT;
use yoyo::service::{serve, AppState};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ANSI color helpers
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";

fn print_banner() {
    println!(
        "\n{BOLD}{CYAN}  yoyo{RESET} v{VERSION} {DIM}— a coding agent growing up in public{RESET}"
    );
    println!("{DIM}  Type /quit to exit, /clear to reset{RESET}\n");
}

fn print_help() {
    println!(
        r#"yoyo v{VERSION} — AI model railway control agent

USAGE:
  yoyo --repl [OPTIONS]    Interactive REPL mode
  yoyo --serve [OPTIONS]   HTTP service mode
  yoyo --help              Show this help
  yoyo --version           Show version

REPL OPTIONS:
  --model <name>       LLM model name (default: claude-opus-4-6)
  --skills <dir>       Skills directory (repeatable)
  --system <file>      Read system prompt from this file (UTF-8) instead of the default

SERVICE OPTIONS:
  --bind <host:port>   Bind address (default: 0.0.0.0:8080)
  --port <port>        Bind port (default: 8080; ignored if --bind given)

ENVIRONMENT:
  ANTHROPIC_API_KEY    API key (required; also accepts API_KEY)
  MODEL                Default model for --serve mode
  YOYO_SKILLS          Comma-separated skill dirs for --serve mode
  PI_URL               Pi base URL (default: http://192.168.1.80:5000)

REPL COMMANDS:
  /quit, /exit         Exit the REPL
  /clear               Reset conversation
  /model <name>        Switch model (clears conversation)
  /health              Same as GET /health
  /evolve              Same as POST /evolve
  /initialise <json>   Same as POST /initialise (JSON on one line)
  /program <json>      Same as POST /program
  /route <json>        Same as POST /route (compute routes for trains)
  /route/execute <json> Same as POST /route/execute (plan & execute on Pi)
  /automatic           Same as POST /automatic
  /automatic/status    Same as GET /automatic/status
  /stop                Same as POST /stop
  /pi status|health|sensors
  /pi sensors reset
  /pi track speed <id> OFF|FWD|BCK <0-100>
  /pi track stop <id>
  /pi allstop
  /pi point <id> THRU|BRANCH
  /pi sensor <id> true|false

HTTP (--serve) ENDPOINTS:
  GET  /health         Health check
  GET  /journal        Evolution journal (JOURNAL.md)
  GET  /roadmap        Planned curriculum (ROADMAP.md)
  POST /evolve         Trigger evolution session
  POST /initialise     Set train positions
  POST /route          Compute routes for trains with destinations
  POST /route/execute  Compute routes and execute on Pi hardware
  POST /program        Upload track program
  POST /automatic      Start automatic mode
  GET  /automatic/status  Train positions, phases, and track usage
  POST /stop           Stop automatic mode
  GET  /pi/status      Live track/point/sensor status from Pi
  GET  /pi/health      Pi hardware health
  GET  /pi/sensors     All sensor values
  POST /pi/track/:id/speed?direction=FWD&speed=50  Set track speed
  POST /pi/track/:id/stop     Stop one track
  POST /pi/allstop            Emergency stop all tracks
  POST /pi/point/:id?direction=THRU  Switch a point
  POST /pi/sensor/:id?value=true     Force a sensor (testing)
  POST /pi/sensors/reset             Clear all sensors"#,
        VERSION = VERSION,
    );
}

/// Prints per-turn token usage and running REPL session totals.
fn print_turn_usage(usage: &Usage, session_in: &mut u64, session_out: &mut u64) {
    *session_in += usage.input;
    *session_out += usage.output;
    if usage.input > 0 || usage.output > 0 {
        println!(
            "\n{DIM}  tokens (this turn): {} in / {} out{RESET}",
            usage.input, usage.output
        );
    }
    if *session_in > 0 || *session_out > 0 {
        println!(
            "{DIM}  session total: {} in / {} out{RESET}",
            *session_in, *session_out
        );
    }
}

/// Load system prompt: default [`SYSTEM_PROMPT`] or text from `--system <file>`.
fn load_repl_system_prompt(args: &[String]) -> Result<(String, Option<String>), String> {
    if let Some(i) = args.iter().position(|a| a == "--system") {
        let path = args
            .get(i + 1)
            .ok_or_else(|| "--system requires a path to a UTF-8 text file".to_string())?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read --system {path}: {e}"))?;
        Ok((text, Some(path.clone())))
    } else {
        Ok((SYSTEM_PROMPT.to_string(), None))
    }
}

fn evolution_config_from_env() -> Result<EvolutionConfig, String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("API_KEY"))
        .map_err(|_| "ANTHROPIC_API_KEY or API_KEY must be set".to_string())?;
    let model = std::env::var("MODEL").unwrap_or_else(|_| "claude-opus-4-6".into());
    let skill_dirs = std::env::var("YOYO_SKILLS")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_else(|_| vec!["./skills".to_string()]);
    Ok(EvolutionConfig {
        api_key,
        model,
        skill_dirs,
    })
}

fn parse_bind(args: &[String]) -> Result<SocketAddr, String> {
    if let Some(i) = args.iter().position(|a| a == "--bind") {
        if let Some(addr) = args.get(i + 1) {
            return addr
                .parse()
                .map_err(|_| format!("invalid --bind {addr} (use host:port, e.g. 0.0.0.0:8080)"));
        }
    }
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    Ok(SocketAddr::from(([0, 0, 0, 0], port)))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("yoyo {}", VERSION);
        return;
    }

    if args.iter().any(|a| a == "--serve") {
        run_serve(&args).await;
        return;
    }

    // Default to REPL mode (also handles explicit --repl)
    run_repl(&args).await;
}

// ---------------------------------------------------------------------------
// --serve mode
// ---------------------------------------------------------------------------

async fn run_serve(args: &[String]) {
    let bind = match parse_bind(args) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("yoyo: {e}");
            std::process::exit(1);
        }
    };
    let evolution = match evolution_config_from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("yoyo: {e}");
            std::process::exit(1);
        }
    };
    let pi_url =
        std::env::var("PI_URL").unwrap_or_else(|_| yoyo::pi_client::DEFAULT_PI_URL.to_string());
    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let state = AppState {
        evolve_lock: Arc::new(Mutex::new(())),
        evolution,
        automation: Arc::new(AutomationController::new()),
        pi: Arc::new(PiClient::new(&pi_url)),
        shutdown_tx: Arc::new(shutdown_tx),
    };
    eprintln!("yoyo v{}: HTTP service on http://{bind}", VERSION);
    eprintln!("yoyo: Pi hardware at {pi_url}");
    eprintln!("yoyo: POST /evolve /initialise /route /program /automatic /stop");
    eprintln!(
        "yoyo: GET  /health /journal /roadmap /automatic/status /pi/status /pi/health /pi/sensors"
    );
    eprintln!("yoyo: POST /pi/track/:id/speed /pi/track/:id/stop /pi/allstop /pi/point/:id /pi/sensor/:id /pi/sensors/reset");
    if let Err(e) = serve(bind, state).await {
        eprintln!("yoyo: server error: {e}");
        std::process::exit(1);
    }
    eprintln!("yoyo: server stopped — restart the process to pick up new code");
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// --repl mode
// ---------------------------------------------------------------------------

async fn run_repl(args: &[String]) {
    let api_key = match std::env::var("ANTHROPIC_API_KEY").or_else(|_| std::env::var("API_KEY")) {
        Ok(k) => k,
        Err(_) => {
            eprintln!("yoyo: ANTHROPIC_API_KEY or API_KEY must be set");
            std::process::exit(1);
        }
    };

    if api_key.trim().is_empty() {
        eprintln!("yoyo: warning: API key is empty — API calls will fail");
    } else if api_key.len() < 10 {
        eprintln!(
            "yoyo: warning: API key looks too short ({} chars) — API calls may fail",
            api_key.len()
        );
    }

    let mut model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "claude-opus-4-6".into());

    let (system_prompt, system_prompt_path) = match load_repl_system_prompt(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("yoyo: {e}");
            std::process::exit(1);
        }
    };

    let skill_dirs: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--skills")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();

    let skills = if skill_dirs.is_empty() {
        SkillSet::empty()
    } else {
        match SkillSet::load(&skill_dirs) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("yoyo: failed to load skills: {e}");
                std::process::exit(1);
            }
        }
    };

    let mut agent = Agent::new(AnthropicProvider)
        .with_system_prompt(&system_prompt)
        .with_model(&model)
        .with_api_key(&api_key)
        .with_skills(skills.clone())
        .with_tools(default_tools());

    let skill_dirs_for_evo: Vec<String> = if skill_dirs.is_empty() {
        vec!["./skills".to_string()]
    } else {
        skill_dirs.clone()
    };
    let pi_url = std::env::var("PI_URL").unwrap_or_else(|_| DEFAULT_PI_URL.to_string());
    let (repl_shutdown_tx, _repl_shutdown_rx) = tokio::sync::watch::channel(false);
    let repl_state = AppState {
        evolve_lock: Arc::new(Mutex::new(())),
        evolution: EvolutionConfig {
            api_key: api_key.clone(),
            model: model.clone(),
            skill_dirs: skill_dirs_for_evo,
        },
        automation: Arc::new(AutomationController::new()),
        pi: Arc::new(PiClient::new(&pi_url)),
        shutdown_tx: Arc::new(repl_shutdown_tx),
    };

    print_banner();
    println!("{DIM}  model: {model}{RESET}");
    if let Some(ref p) = system_prompt_path {
        println!("{DIM}  system: {p} (custom file){RESET}");
    }
    if !skills.is_empty() {
        println!("{DIM}  skills: {} loaded{RESET}", skills.len());
    }
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".into());
    let git_branch = git_branch_for_prompt();
    println!("{DIM}  cwd:   {cwd}{RESET}");
    if let Some(ref b) = &git_branch {
        println!("{DIM}  git:   {b}{RESET}");
    }
    println!("{DIM}  pi:    {pi_url}{RESET}");
    println!("{DIM}  type /help for commands{RESET}\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut session_tokens_in: u64 = 0;
    let mut session_tokens_out: u64 = 0;

    loop {
        if let Some(ref b) = &git_branch {
            print!("{BOLD}{GREEN}[{b}] {RESET}");
        }
        print!("{BOLD}{GREEN}> {RESET}");
        io::stdout().flush().ok();

        let line = match lines.next() {
            Some(Ok(l)) => l,
            _ => break,
        };

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        match input {
            "/quit" | "/exit" => break,
            "/help" => {
                repl::print_repl_help();
                continue;
            }
            "/clear" => {
                agent = Agent::new(AnthropicProvider)
                    .with_system_prompt(&system_prompt)
                    .with_model(&model)
                    .with_api_key(&api_key)
                    .with_skills(skills.clone())
                    .with_tools(default_tools());
                println!("{DIM}  (conversation cleared){RESET}\n");
                continue;
            }
            s if s.starts_with("/model ") => {
                let new_model = s.trim_start_matches("/model ").trim();
                model = new_model.to_string();
                agent = Agent::new(AnthropicProvider)
                    .with_system_prompt(&system_prompt)
                    .with_model(new_model)
                    .with_api_key(&api_key)
                    .with_skills(skills.clone())
                    .with_tools(default_tools());
                println!("{DIM}  (switched to {new_model}, conversation cleared){RESET}\n");
                continue;
            }
            s if s.starts_with('/') => {
                if repl::service_dispatch(s, &repl_state).await {
                    println!();
                    continue;
                }
                eprintln!(
                    "{RED}  unknown command (try /help). Messages to the agent must not start with /{RESET}\n"
                );
                continue;
            }
            _ => {}
        }

        let (last_usage, cancelled) = run_agent_turn(&mut agent, input).await;

        print_turn_usage(&last_usage, &mut session_tokens_in, &mut session_tokens_out);
        if cancelled {
            println!("\n{YELLOW}  ⚠ interrupted{RESET}");
        }
        println!();
    }

    println!("\n{DIM}  bye 👋{RESET}\n");
}

// ---------------------------------------------------------------------------
// Agent turn (streaming + tool feedback)
// ---------------------------------------------------------------------------

/// Run one agent turn: stream text, show tool calls, handle Ctrl+C.
/// Returns the usage stats and whether the turn was cancelled.
async fn run_agent_turn(agent: &mut Agent, input: &str) -> (Usage, bool) {
    let mut rx = agent.prompt(input).await;
    let mut last_usage = Usage::default();
    let mut in_text = false;
    let mut cancelled = false;

    loop {
        let event = tokio::select! {
            ev = rx.recv() => match ev {
                Some(e) => e,
                None => break,
            },
            _ = tokio::signal::ctrl_c() => {
                agent.abort();
                cancelled = true;
                while rx.recv().await.is_some() {}
                break;
            }
        };
        match event {
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => {
                if in_text {
                    println!();
                    in_text = false;
                }
                let summary = tool_summary(&tool_name, &args);
                print!("{YELLOW}  ▶ {summary}{RESET}");
                io::stdout().flush().ok();
            }
            AgentEvent::ToolExecutionEnd { is_error, .. } => {
                if is_error {
                    println!(" {RED}✗{RESET}");
                } else {
                    println!(" {GREEN}✓{RESET}");
                }
            }
            AgentEvent::MessageUpdate {
                delta: StreamDelta::Text { delta },
                ..
            } => {
                if !in_text {
                    println!();
                    in_text = true;
                }
                print!("{}", delta);
                io::stdout().flush().ok();
            }
            AgentEvent::AgentEnd { messages } => {
                for msg in messages.iter().rev() {
                    if let AgentMessage::Llm(Message::Assistant {
                        usage,
                        stop_reason,
                        error_message,
                        ..
                    }) = msg
                    {
                        last_usage = usage.clone();
                        if *stop_reason == StopReason::Error {
                            if in_text {
                                println!();
                                in_text = false;
                            }
                            let err = error_message.as_deref().unwrap_or("unknown API error");
                            let err = format_api_error_for_user(err);
                            eprintln!("\n{RED}  ⚠ API error: {err}{RESET}");
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    if in_text {
        println!();
    }

    (last_usage, cancelled)
}

/// One-line summary of a tool call for the REPL.
fn tool_summary(tool_name: &str, args: &serde_json::Value) -> String {
    match tool_name {
        "bash" => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("...");
            format!("$ {}", truncate(cmd, 80))
        }
        "read_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("read {}", path)
        }
        "write_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("write {}", path)
        }
        "edit_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("edit {}", path)
        }
        "list_files" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("ls {}", path)
        }
        "search" => {
            let pat = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            format!("search '{}'", truncate(pat, 60))
        }
        _ => tool_name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// If `git rev-parse` works, return the current branch name for the REPL prompt.
fn git_branch_for_prompt() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn load_repl_system_prompt_default_matches_embedded() {
        let args = vec!["yoyo".into()];
        let (s, p) = load_repl_system_prompt(&args).unwrap();
        assert!(p.is_none());
        assert_eq!(s, SYSTEM_PROMPT);
    }

    #[test]
    fn load_repl_system_prompt_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("sys.txt");
        std::fs::write(&path, "custom brain").unwrap();
        let args = vec![
            "yoyo".into(),
            "--system".into(),
            path.to_str().unwrap().to_string(),
        ];
        let (s, p) = load_repl_system_prompt(&args).unwrap();
        assert_eq!(s, "custom brain");
        assert_eq!(p.as_deref(), path.to_str());
    }

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_unicode() {
        assert_eq!(truncate("héllo wörld", 5), "héllo");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn test_parse_bind_default() {
        let args: Vec<String> = vec!["yoyo".into(), "--serve".into()];
        let addr = parse_bind(&args).unwrap();
        assert_eq!(addr, SocketAddr::from(([0, 0, 0, 0], 8080)));
    }

    #[test]
    fn test_parse_bind_custom_port() {
        let args: Vec<String> = vec![
            "yoyo".into(),
            "--serve".into(),
            "--port".into(),
            "9090".into(),
        ];
        let addr = parse_bind(&args).unwrap();
        assert_eq!(addr, SocketAddr::from(([0, 0, 0, 0], 9090)));
    }

    #[test]
    fn test_parse_bind_custom_address() {
        let args: Vec<String> = vec![
            "yoyo".into(),
            "--serve".into(),
            "--bind".into(),
            "127.0.0.1:3000".into(),
        ];
        let addr = parse_bind(&args).unwrap();
        assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], 3000)));
    }

    #[test]
    fn test_parse_bind_invalid_address() {
        let args: Vec<String> = vec!["yoyo".into(), "--bind".into(), "not-an-addr".into()];
        assert!(parse_bind(&args).is_err());
    }

    #[test]
    fn test_version_constant() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "VERSION should be semver (x.y.z)");
        for part in &parts {
            assert!(
                part.parse::<u32>().is_ok(),
                "each semver component should be a number"
            );
        }
    }

    #[test]
    fn tool_summary_bash() {
        let args = serde_json::json!({"command": "ls -la"});
        assert_eq!(tool_summary("bash", &args), "$ ls -la");
    }

    #[test]
    fn tool_summary_read_file() {
        let args = serde_json::json!({"path": "src/main.rs"});
        assert_eq!(tool_summary("read_file", &args), "read src/main.rs");
    }

    #[test]
    fn tool_summary_unknown_tool() {
        let args = serde_json::json!({});
        assert_eq!(tool_summary("custom_tool", &args), "custom_tool");
    }
}
