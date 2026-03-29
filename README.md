# train-evolve

**A coding agent that evolves itself. One commit per evolution run.**

This started as  ~1638-lines of rust code, running an agent CLI built on [yoagent](https://github.com/yologdev/yoagent). Quite a lot of this is to do with reading and verifing the track layout
and defining the interface to the track harware. 

 **Evolution is not on a calendar** — you run a long-lived **HTTP service** and trigger improvements when you want them (for example `POST /evolve`). Each run, yoyo reads its own source, picks improvements, implements them, tests them, and writes about what happened.

It can't cheat. It can't skip. Every change must pass CI. Every failure is documented.

Watch it grow.

---

## How It Works

**Primary mode: HTTP service.** Run `cargo run -- --serve` and call **`POST /evolve`** when you want an evolution iteration. That fits a machine or Pi that stays up; there is no requirement for GitHub Actions, cron, or “once per day.”

**Optional extras:**

- **Interactive REPL** — `cargo run` (no `--serve`) for local experimentation.
- **`./scripts/evolve.sh`** — optional helper that can fetch GitHub issues, run a session, and push; useful if you still use that workflow, but **not** how the service is meant to run day-to-day.

Each **evolution** (each `/evolve` trigger):

1. The agent reads its identity, journal, roadmap, and (when present) community issues
2. It assesses itself — reads its own code, tries things, finds gaps
3. It picks improvements (issues, self-assessment, or roadmap)
4. It implements changes, runs tests, updates `Cargo.toml` version when appropriate, writes a journal entry
5. If tests pass → commit. If tests fail → revert `src/` and report failure.
6. If it addressed a GitHub issue (when using the shell script path), it can comment back via `gh`.

The history lives in the git log. The journal is in [JOURNAL.md](JOURNAL.md). The plan is in [ROADMAP.md](ROADMAP.md). The static track layout (tracks, sensors, points, stations) lives in [data/track_layout.toml](data/track_layout.toml); see [docs/TRACK_LAYOUT.md](docs/TRACK_LAYOUT.md). The full HTTP API and JSON formats are documented in [docs/API.md](docs/API.md).

## Talk to It

Open a [GitHub issue](../../issues/new/choose) if you use issue-driven evolution; the agent can read those inputs when you run **`/evolve`** (via the evolution prompt) or when you use **`scripts/evolve.sh`**.

- **Suggestions** → tell it what to learn
- **Bugs** → tell it what's broken
- **Challenges** → give it a task and see if it can do it

Issues with more 👍 get prioritized when issues are loaded into the session. The agent responds in its own voice.

## Acrhitecture

The train system consists of: 

1. The physical board with the track, points and sensors. Photo's to come.... It consists of: 12 independent track segments, each programmable; 18 Points. Some of these are pairs that operate together; 24 train detector sensors.
2. A Raspberry Pi that controls the above. This is mounted below the board, and is a real rats nest of wires. This is programmed in Python. It includes a semi-intellgent engine that detects any problems, such as imminent collisions, in real-time and can stop individual tracks until the issue is cleared. 
3. A visualisation program where you can manually control and see what is going one. This is build in vue.js. This interacts with the Pi via REST, and the AI also via REST.
4. This Rust program that runs the AI. It is destined (eventually) to run on a second Pi that will also be located on the board. This one may have a display so we can see what it is doing when it runs in autonomous train management mode.

## Run It Yourself

**HTTP service** (recommended; bind defaults to `0.0.0.0:8080`):

```bash
git clone https://github.com/yologdev/yoyo-evolve
cd yoyo-evolve
ANTHROPIC_API_KEY=sk-... cargo run -- --serve
# optional: --bind 0.0.0.0:9000  or  --port 9000
```

**Interactive REPL** (optional):

```bash
ANTHROPIC_API_KEY=sk-... cargo run
# optional: --system path/to/prompt.txt  (override the embedded system prompt)
# optional: --model …  --skills ./skills  (see yoyo --help)
```

After each assistant reply, the REPL prints **tokens for this turn** and a **session total** (cumulative for that process).

| Method | Path | Body | Purpose |
|--------|------|------|---------|
| `GET` | `/health` | — | JSON: `{ "status": "ok", "automatic": true/false }` |
| `GET` | `/journal` | — | JSON: `{ "path": "JOURNAL.md", "text": "..." }` — evolution journal on disk |
| `GET` | `/roadmap` | — | JSON: `{ "path": "ROADMAP.md", "text": "..." }` — planned curriculum |
| `POST` | `/evolve` | (optional) | One evolution iteration (build → agent → verify → git wrap-up). Response includes `session` (from `DAY_COUNT`, a run counter). |
| `POST` | `/initialise` | JSON: `{ "trains": [ { "train": 1, "sensor": 4 }, { "train": 2, "sensor": 7, "direction": "bwd" } ] }` | Register trains on the layout; up to **6** trains. See [docs/API.md](docs/API.md) |
| `POST` | `/program` | JSON (any) | Placeholder; saves payload to `data/runtime/program.json` for a future track program |
| `POST` | `/route` | JSON: `{ "trains": [ { "train": 1, "sensor": 1, "destination": 5 } ] }` | Compute routes for trains with destinations. See [docs/API.md](docs/API.md) |
| `POST` | `/route/execute` | (same as `/route`) | Compute and execute routes on Pi hardware |
| `POST` | `/automatic` | — | **Boss-level** automation: loads `data/runtime/trains.json` (call `/initialise` first), saves a snapshot, runs a loop until `/stop` (timetable + Pi hardware integration is still a placeholder) |
| `POST` | `/stop` | — | Stops `/automatic` and restores **saved** train positions (`data/runtime/trains.json` from the snapshot at `/automatic` start). Moving real trains on the Pi is not implemented yet — that will use the track API later. |

Example:

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8080/journal
curl -s http://127.0.0.1:8080/roadmap
curl -s -X POST http://127.0.0.1:8080/evolve
curl -s -X POST http://127.0.0.1:8080/initialise \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":4},{"train":2,"sensor":7,"direction":"bwd"}]}'
curl -s -X POST http://127.0.0.1:8080/automatic
curl -s -X POST http://127.0.0.1:8080/stop
```

Environment: `MODEL` (default `claude-opus-4-6`), `YOYO_SKILLS` (comma-separated skill dirs, default `./skills`).

**Optional shell evolution** (issues + push — only if you choose this workflow):

```bash
ANTHROPIC_API_KEY=sk-... ./scripts/evolve.sh
```

## Developing

- Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt -- --check` before pushing (CI runs these).
- New user-facing behavior should update [README.md](README.md) or [CLAUDE.md](CLAUDE.md) and include tests when practical.
- Optional: `./commit "your message"` (repo root) runs those checks, bumps the **patch** version in `Cargo.toml`, then `git add -A`, commit, and push.

## The Story So Far

Read [JOURNAL.md](JOURNAL.md) for session-by-session updates, or browse the [git log](../../commits/main) to see every change the agent has made to itself.

## Built On

[yoagent](https://github.com/yologdev/yoagent) — simple, effective agent loop in Rust. The library that makes this possible.

## License

MIT
