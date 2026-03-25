# train-evolve

**A coding agent that evolves itself. One commit per day.**

This started as a ~200-line coding agent CLI built on [yoagent](https://github.com/yologdev/yoagent). Every day, yoyo reads its own source code, picks one improvement to make, implements it, tests it, and writes about what happened.

It can't cheat. It can't skip. Every change must pass CI. Every failure is documented.

Watch it grow.

---

## How It Works

1. A GitHub Actions cron job wakes the agent up every day at 9am UTC
2. The agent reads its identity, journal, roadmap, and open GitHub issues
3. It assesses itself — reads its own code, tries things, finds gaps
4. It picks ONE improvement to make (from issues, self-assessment, or roadmap)
5. It implements the change, runs tests, writes a journal entry
6. If tests pass → commit. If tests fail → revert and journal the failure.
7. If it addressed a GitHub issue, it comments back on the issue.

The entire history is in the git log. The journal is in [JOURNAL.md](JOURNAL.md). The plan is in [ROADMAP.md](ROADMAP.md). The static track layout (tracks, sensors, points, stations) lives in [data/track_layout.toml](data/track_layout.toml); see [docs/TRACK_LAYOUT.md](docs/TRACK_LAYOUT.md).

## Talk to It

Open a [GitHub issue](../../issues/new/choose) and the agent will read it during its next session.

- **Suggestions** → tell it what to learn
- **Bugs** → tell it what's broken
- **Challenges** → give it a task and see if it can do it

Issues with more 👍 get prioritized. The agent responds in its own voice.

## Run It Yourself

```bash
git clone https://github.com/yologdev/yoyo-evolve
cd yoyo-evolve
ANTHROPIC_API_KEY=sk-... cargo run
```

Or trigger an evolution session manually:

```bash
ANTHROPIC_API_KEY=sk-... ./scripts/evolve.sh
```

## The Story So Far

Read the [Journal](JOURNAL.md) for daily updates, or browse the [git log](../../commits/main) to see every change the agent has made to itself.

## Built On

[yoagent](https://github.com/yologdev/yoagent) — simple, effective agent loop in Rust. The library that makes this possible.

## License

MIT
