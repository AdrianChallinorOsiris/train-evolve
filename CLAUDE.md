# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

A self-evolving coding agent CLI built on [yoagent]. The agent implementation lives under `src/` (currently centered on `src/main.rs`). A daily GitHub Actions cron job (`scripts/evolve.sh`) runs the agent, which reads its own source, picks one improvement, implements it, and commits — if tests pass.

## Build & Test Commands

```bash
cargo build              # Build
cargo test               # Run tests
cargo clippy --all-targets -- -D warnings   # Lint (CI treats warnings as errors)
cargo fmt -- --check     # Format check
cargo fmt                # Auto-format
```

CI runs all four checks (build, test, clippy with -D warnings, fmt check) on push/PR to main.

To run the agent interactively:
```bash
ANTHROPIC_API_KEY=sk-... cargo run
ANTHROPIC_API_KEY=sk-... cargo run -- --model claude-opus-4-6 --skills ./skills
```

To trigger a full evolution cycle:
```bash
ANTHROPIC_API_KEY=sk-... ./scripts/evolve.sh
```

## Architecture

**Agent crate**: The binary is built from `src/` — a REPL that uses `yoagent::Agent` with `AnthropicProvider`, `default_tools()`, and an optional `SkillSet`. It handles streaming `AgentEvent`s (tool execution, text deltas, agent end) and renders them with ANSI colors. The library (`src/lib.rs`) exposes **`yoyo::layout`**: static track topology from `data/track_layout.toml` (see `docs/TRACK_LAYOUT.md`).

**Evolution loop** (`scripts/evolve.sh`): Verifies build → fetches GitHub issues (via `gh` CLI + `scripts/format_issues.py`) → pipes a structured prompt into the agent → verifies build after changes → commits or reverts → posts issue responses → pushes.

**Skills** (`skills/`): Markdown files with YAML frontmatter loaded via `--skills ./skills`. These skills define the agent's workflow:
- `self-assess` — read own code, try tasks, find bugs/gaps
- `evolve` — safely modify source, test, revert on failure
- `communicate` — write journal entries and issue responses
- `control` — interact with the train hardware

**State files** (read/written by the agent during evolution):
- `IDENTITY.md` — the agent's constitution and rules (DO NOT MODIFY)
- `JOURNAL.md` — chronological log of daily sessions (append at top, never delete)
- `ROADMAP.md` — leveled curriculum of planned improvements
- `LEARNINGS.md` — cached knowledge from internet lookups
- `DAY_COUNT` — integer tracking current evolution day
- `ISSUES_TODAY.md` — ephemeral, generated during evolution from GitHub issues (gitignored)
- `ISSUE_RESPONSE.md` — ephemeral, agent writes this to respond to issues (gitignored)

## Safety Rules

These are enforced by the `evolve` skill and `evolve.sh`:
- Never modify `IDENTITY.md`, `scripts/evolve.sh`, or `.github/workflows/`
- Every code change must pass `cargo build && cargo test`
- If build fails after changes, revert with `git checkout -- src/`
- Never delete existing tests
- One improvement per evolution session — small, focused changes only
- Write tests before adding features
