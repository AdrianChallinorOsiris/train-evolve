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
//! REPL commands: `/quit`, `/exit`, `/clear`, `/model <name>`

use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use yoagent::agent::Agent;
use yoagent::provider::AnthropicProvider;
use yoagent::skills::SkillSet;
use yoagent::tools::default_tools;
use yoagent::*;

use yoyo::automation::AutomationController;
use yoyo::evolve_session::EvolutionConfig;
use yoyo::pi_client::PiClient;
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
    println!("\n{BOLD}{CYAN}  yoyo{RESET} v{VERSION} {DIM}— a coding agent growing up in public{RESET}");
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

SERVICE ENDPOINTS:
  GET  /health         Health check
  POST /evolve         Trigger evolution session
  POST /initialise     Set train positions
  POST /program        Upload track program
  POST /automatic      Start automatic mode
  POST /stop           Stop automatic mode
  GET  /pi/status      Live track/point/sensor status from Pi
  GET  /pi/health      Pi hardware health
  GET  /pi/sensors     All sensor values"#,
        VERSION = VERSION,
    );
}

fn print_usage(usage: &Usage) {
    if usage.input > 0 || usage.output > 0 {
        println!(
            "\n{DIM}  tokens: {} in / {} out{RESET}",
            usage.input, usage.output
        );
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

    // --help: print usage and exit
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    // --version: print version and exit
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("yoyo {}", VERSION);
        return;
    }

    if args.iter().any(|a| a == "--serve") {
        let bind = match parse_bind(&args) {
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
        let pi_url = std::env::var("PI_URL")
            .unwrap_or_else(|_| yoyo::pi_client::DEFAULT_PI_URL.to_string());
        let state = AppState {
            evolve_lock: Arc::new(Mutex::new(())),
            evolution,
            automation: Arc::new(AutomationController::new()),
            pi: Arc::new(PiClient::new(&pi_url)),
        };
        eprintln!("yoyo: HTTP service on http://{bind}");
        eprintln!("yoyo: POST /evolve /initialise /program /automatic /stop  GET /health /pi/status /pi/health /pi/sensors");
        if let Err(e) = serve(bind, state).await {
            eprintln!("yoyo: server error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Default to REPL mode (also handles explicit --repl)
    let api_key = match std::env::var("ANTHROPIC_API_KEY").or_else(|_| std::env::var("API_KEY")) {
        Ok(k) => k,
        Err(_) => {
            eprintln!("yoyo: ANTHROPIC_API_KEY or API_KEY must be set");
            std::process::exit(1);
        }
    };

    // Early validation: warn (don't block) if key looks obviously wrong.
    if api_key.trim().is_empty() {
        eprintln!("yoyo: warning: API key is empty — API calls will fail");
    } else if api_key.len() < 10 {
        eprintln!("yoyo: warning: API key looks too short ({} chars) — API calls may fail", api_key.len());
    }

    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "claude-opus-4-6".into());

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
        .with_system_prompt(SYSTEM_PROMPT)
        .with_model(&model)
        .with_api_key(&api_key)
        .with_skills(skills.clone())
        .with_tools(default_tools());

    print_banner();
    println!("{DIM}  model: {model}{RESET}");
    if !skills.is_empty() {
        println!("{DIM}  skills: {} loaded{RESET}", skills.len());
    }
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".into());
    println!("{DIM}  cwd:   {cwd}{RESET}\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
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
            "/clear" => {
                agent = Agent::new(AnthropicProvider)
                    .with_system_prompt(SYSTEM_PROMPT)
                    .with_model(&model)
                    .with_api_key(&api_key)
                    .with_skills(skills.clone())
                    .with_tools(default_tools());
                println!("{DIM}  (conversation cleared){RESET}\n");
                continue;
            }
            s if s.starts_with("/model ") => {
                let new_model = s.trim_start_matches("/model ").trim();
                agent = Agent::new(AnthropicProvider)
                    .with_system_prompt(SYSTEM_PROMPT)
                    .with_model(new_model)
                    .with_api_key(&api_key)
                    .with_skills(skills.clone())
                    .with_tools(default_tools());
                println!("{DIM}  (switched to {new_model}, conversation cleared){RESET}\n");
                continue;
            }
            _ => {}
        }

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
                    // drain remaining events
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
                    let summary = match tool_name.as_str() {
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
                        _ => tool_name.clone(),
                    };
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
                                let err = error_message
                                    .as_deref()
                                    .unwrap_or("unknown API error");
                                eprintln!(
                                    "\n{RED}  ⚠ API error: {err}{RESET}"
                                );
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
        if cancelled {
            println!("\n{YELLOW}  ⚠ interrupted{RESET}");
        }
        print_usage(&last_usage);
        println!();
    }

    println!("\n{DIM}  bye 👋{RESET}\n");
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
        let args: Vec<String> = vec!["yoyo".into(), "--serve".into(), "--port".into(), "9090".into()];
        let addr = parse_bind(&args).unwrap();
        assert_eq!(addr, SocketAddr::from(([0, 0, 0, 0], 9090)));
    }

    #[test]
    fn test_parse_bind_custom_address() {
        let args: Vec<String> = vec!["yoyo".into(), "--serve".into(), "--bind".into(), "127.0.0.1:3000".into()];
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
        // VERSION should match Cargo.toml; ensure it parses as semver
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "VERSION should be semver (x.y.z)");
        for part in &parts {
            assert!(part.parse::<u32>().is_ok(), "each semver component should be a number");
        }
    }
}
