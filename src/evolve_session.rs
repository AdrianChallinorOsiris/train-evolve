//! One evolution iteration (aligned with `scripts/evolve.sh`, callable from the HTTP service).

use std::fs;
use std::path::Path;
use std::process::Command;

use yoagent::agent::Agent;
use yoagent::provider::AnthropicProvider;
use yoagent::skills::SkillSet;
use yoagent::tools::default_tools;
use yoagent::Usage;

use crate::agent_runner::run_prompt;
use crate::prompts::SYSTEM_PROMPT;
use thiserror::Error;

/// Configuration for a single evolution run.
#[derive(Clone)]
pub struct EvolutionConfig {
    pub api_key: String,
    pub model: String,
    pub skill_dirs: Vec<String>,
}

/// Result of one evolution iteration.
#[derive(Debug)]
pub struct EvolutionOutcome {
    /// Session number from `DAY_COUNT` at the start of this run (evolution counter, not calendar).
    pub session: u32,
    pub transcript: String,
    pub usage: Usage,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum EvolutionError {
    #[error("pre-flight cargo build/test failed: {0}")]
    PreflightFailed(String),
    #[error("post-evolution cargo build/test failed; reverted src/ with: {0}")]
    PostCheckFailed(String),
    #[error("agent error: {0}")]
    Agent(String),
}

fn read_day_count() -> u32 {
    fs::read_to_string("DAY_COUNT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1)
}

fn write_day_count(day: u32) -> std::io::Result<()> {
    fs::write("DAY_COUNT", format!("{day}\n"))
}

fn iso_date() -> String {
    std::env::var("DATE_OVERRIDE").unwrap_or_else(|_| {
        Command::new("date")
            .arg("+%Y-%m-%d")
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown-date".into())
    })
}

fn cargo_build_test() -> Result<(), String> {
    let build = Command::new("cargo")
        .args(["build", "--quiet"])
        .status()
        .map_err(|e| e.to_string())?;
    if !build.success() {
        return Err("cargo build failed".into());
    }
    let test = Command::new("cargo")
        .args(["test", "--quiet"])
        .status()
        .map_err(|e| e.to_string())?;
    if !test.success() {
        return Err("cargo test failed".into());
    }
    Ok(())
}

fn git_checkout_src() -> Result<(), String> {
    let s = Command::new("git")
        .args(["checkout", "--", "src/"])
        .status()
        .map_err(|e| e.to_string())?;
    if !s.success() {
        return Err("git checkout -- src/ failed".into());
    }
    Ok(())
}

fn git_status_dirty() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Prepends the full assistant transcript to `JOURNAL.md` (newest entry at top).
///
/// The prompt asks the model to edit the journal, but that is not reliable; this
/// guarantees every successful HTTP evolution session is recorded.
fn prepend_journal_transcript(
    journal_path: &Path,
    session: u32,
    transcript: &str,
) -> std::io::Result<()> {
    let existing = if journal_path.exists() {
        fs::read_to_string(journal_path)?
    } else {
        String::new()
    };
    let trimmed = transcript.trim_end();
    let body = if trimmed.is_empty() {
        "_No assistant text captured for this session._\n\n".to_string()
    } else {
        format!("{trimmed}\n\n")
    };
    let mut out = format!("## Session {session} — Evolution transcript\n\n{body}{existing}");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    fs::write(journal_path, out)
}

fn git_wrap_up(session: u32) -> Result<(), String> {
    if !git_status_dirty() {
        return Ok(());
    }
    let add = Command::new("git")
        .args(["add", "-A"])
        .status()
        .map_err(|e| e.to_string())?;
    if !add.success() {
        return Err("git add -A failed".into());
    }
    let msg = format!("Session {session}: wrap-up");
    let commit = Command::new("git")
        .args(["commit", "-m", &msg])
        .status()
        .map_err(|e| e.to_string())?;
    if !commit.success() {
        return Err("git commit failed (nothing to commit?)".into());
    }
    Ok(())
}

fn build_evolution_prompt(session: u32, date: &str) -> String {
    let recent_journal: String = fs::read_to_string("JOURNAL.md")
        .map(|s| s.chars().take(2000).collect::<String>())
        .unwrap_or_else(|_| "No journal yet.".into());

    let issues = fs::read_to_string("ISSUES_TODAY.md").unwrap_or_else(|_| {
        "No issues file (run gh fetch or create empty ISSUES_TODAY.md).".into()
    });

    format!(
        r#"This is evolution session {session} ({date}). The operator triggered you (e.g. POST /evolve); you are not on a daily schedule.

Read these files in this order:
1. IDENTITY.md (who you are and your rules)
2. src/ (your source code — this is YOU)
3. ROADMAP.md (your evolution path)
4. JOURNAL.md (your recent history — last 10 entries)
5. ISSUES_TODAY.md (community requests)

=== Recent journal (excerpt) ===
{recent_journal}

=== ISSUES_TODAY ===
{issues}

=== PHASE 1: Self-Assessment ===

Read your own source code carefully. Then try a small task to test
yourself — for example, read a file, edit something, run a command.
Note any friction, bugs, crashes, or missing capabilities.

=== PHASE 2: Review Community Issues ===

Read ISSUES_TODAY.md. These are real people asking you to improve.
Issues with more 👍 reactions should be prioritized higher.

=== PHASE 3: Decide ===

Make as many improvements as you can this session. Prioritize:
1. Self-discovered crash or data loss bug
2. Community issue with most 👍 (if actionable this session)
3. Self-discovered UX friction or missing error handling
4. Planned roadmap item at your current level

=== PHASE 4: Implement ===

For each improvement, follow the evolve skill rules:
- Write a test first if possible
- Use edit_file for surgical changes
- Run cargo build && cargo test after changes
- If build fails, try to fix it. If you can't, revert with: bash git checkout -- src/
- After each successful change, commit: git add -A && git commit -m "Session {session}: <short description>"
- Then move on to the next improvement

=== VERSION (Cargo.toml) ===

Before your final commits, update the `version` field in Cargo.toml using semantic intent:
- **Small change** (fix, tests, docs, refactor): bump the **minor** (second) component (e.g. `0.1.0` → `0.2.0`).
- **New feature** (new capability, new module, user-visible behavior): bump the **major** (first) component and set the **minor** to **0** (e.g. `0.2.0` → `1.0.0`).

Include the version bump in the same commit or an immediate follow-up commit with a clear message.

=== PHASE 5: Journal ===

Write this session's entry at the TOP of JOURNAL.md. Format:
## Session {session} — [title]
[2-4 sentences: what you tried, what worked, what didn't, what's next]

=== PHASE 6: Update Roadmap ===

If you completed a roadmap item, check it off in ROADMAP.md:
- [x] Item description (session {session})

If you discovered a new issue, add it to the appropriate level.

=== PHASE 7: Issue Response ===

If you worked on a community GitHub issue, write to ISSUE_RESPONSE.md:
issue_number: [N]
status: fixed|partial|wontfix
comment: [your 2-3 sentence response to the person]

Now begin. Read IDENTITY.md first."#
    )
}

/// Run one full evolution iteration (build → agent → verify → session counter → optional git wrap-up).
pub async fn run_evolution(cfg: &EvolutionConfig) -> Result<EvolutionOutcome, EvolutionError> {
    cargo_build_test().map_err(EvolutionError::PreflightFailed)?;

    let session = read_day_count();
    let date = iso_date();
    let prompt = build_evolution_prompt(session, &date);

    let skills = if cfg.skill_dirs.is_empty() {
        SkillSet::empty()
    } else {
        SkillSet::load(&cfg.skill_dirs).map_err(|e| EvolutionError::Agent(e.to_string()))?
    };

    let mut agent = Agent::new(AnthropicProvider)
        .with_system_prompt(SYSTEM_PROMPT)
        .with_model(&cfg.model)
        .with_api_key(&cfg.api_key)
        .with_skills(skills)
        .with_tools(default_tools());

    let result = run_prompt(&mut agent, &prompt).await;
    let usage = result.usage;
    let transcript = result.text;

    if let Some(api_err) = &result.api_error {
        return Err(EvolutionError::Agent(format!("API error: {api_err}")));
    }

    let mut warnings = Vec::new();

    if cargo_build_test().is_err() {
        eprintln!("yoyo: post-evolution build failed; reverting src/");
        git_checkout_src().map_err(EvolutionError::PostCheckFailed)?;
        return Err(EvolutionError::PostCheckFailed(
            "cargo build/test failed after evolution; reverting src/".into(),
        ));
    }

    prepend_journal_transcript(Path::new("JOURNAL.md"), session, &transcript)
        .map_err(|e| EvolutionError::Agent(format!("failed to write JOURNAL.md: {e}")))?;

    let next_session = session + 1;
    write_day_count(next_session).map_err(|e| EvolutionError::Agent(e.to_string()))?;

    if let Err(e) = git_wrap_up(session) {
        warnings.push(format!("git wrap-up: {e}"));
    }

    if git_status_dirty() {
        warnings.push("working tree still has uncommitted changes".into());
    }

    Ok(EvolutionOutcome {
        session,
        transcript,
        usage,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn prepend_journal_transcript_inserts_at_top() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("JOURNAL.md");
        fs::write(&journal, "## Old\n\nlegacy\n").expect("seed");

        prepend_journal_transcript(&journal, 7, "Hello from session.").expect("prepend");

        let s = fs::read_to_string(&journal).expect("read");
        assert!(s.starts_with("## Session 7 — Evolution transcript"));
        assert!(s.contains("Hello from session."));
        assert!(s.contains("## Old"));
        assert!(s.find("## Session 7").unwrap() < s.find("## Old").unwrap());
    }

    #[test]
    fn prepend_journal_transcript_empty_transcript() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal: PathBuf = dir.path().join("JOURNAL.md");

        prepend_journal_transcript(&journal, 1, "   \n").expect("prepend");

        let s = fs::read_to_string(&journal).expect("read");
        assert!(s.contains("_No assistant text"));
    }
}
