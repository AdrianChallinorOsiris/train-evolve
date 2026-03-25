# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

A self-evolving coding agent built on [yoagent]. The implementation lives under `src/` (binary `main.rs`, library `lib.rs`). **Primary operation:** a long-lived **HTTP service** (`cargo run -- --serve`) with `POST /evolve` whenever you want an evolution iteration — no calendar or Git workflow required. Optionally **`scripts/evolve.sh`** can fetch GitHub issues and run a similar session (for maintainers who still use that path).

## Build & Test Commands

```bash
cargo build              # Build
cargo test               # Run tests
cargo clippy --all-targets -- -D warnings   # Lint (CI treats warnings as errors)
cargo fmt -- --check     # Format check
cargo fmt                # Auto-format
```

CI runs all four checks (build, test, clippy with -D warnings, fmt check) on push/PR to main.

To run the agent interactively (REPL):
```bash
ANTHROPIC_API_KEY=sk-... cargo run
ANTHROPIC_API_KEY=sk-... cargo run -- --model claude-opus-4-6 --skills ./skills
```

HTTP service (`GET /health`, `POST /evolve`, `POST /initialise`, `POST /program`, `POST /automatic`, `POST /stop`):
```bash
ANTHROPIC_API_KEY=sk-... cargo run -- --serve
```

Shell evolution cycle (issues + push):
```bash
ANTHROPIC_API_KEY=sk-... ./scripts/evolve.sh
```

## Architecture

**Agent crate**: The binary is built from `src/` — a REPL that uses `yoagent::Agent` with `AnthropicProvider`, `default_tools()`, and an optional `SkillSet`. It handles streaming `AgentEvent`s (tool execution, text deltas, agent end) and renders them with ANSI colors. The library (`src/lib.rs`) exposes **`yoyo::layout`**: static track topology from `data/track_layout.toml` (see `docs/TRACK_LAYOUT.md`).

**Evolution** (HTTP `POST /evolve` or REPL): Pre-flight build → agent session with skills → verify build/tests → increment session counter (`DAY_COUNT`) → optional git wrap-up. **Optional shell script** (`scripts/evolve.sh`): Verifies build → fetches GitHub issues (via `gh` + `scripts/format_issues.py`) → runs the agent → verifies → commits/reverts → issue responses → push.

**Skills** (`skills/`): Markdown files with YAML frontmatter loaded via `--skills ./skills`. These skills define the agent's workflow:
- `self-assess` — read own code, try tasks, find bugs/gaps
- `evolve` — safely modify source, test, revert on failure
- `communicate` — write journal entries and issue responses
- `control` — interact with the train hardware

**State files** (read/written by the agent during evolution):
- `IDENTITY.md` — the agent's constitution and rules (DO NOT MODIFY)
- `JOURNAL.md` — chronological log of evolution sessions (append at top, never delete)
- `ROADMAP.md` — leveled curriculum of planned improvements
- `LEARNINGS.md` — cached knowledge from internet lookups
- `DAY_COUNT` — integer session counter (increments each evolution run; filename is legacy, not calendar-based)
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
