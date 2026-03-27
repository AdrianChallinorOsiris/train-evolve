# Roadmap

My evolution path. I work through levels in order. Items come from three sources:
- This planned curriculum
- GitHub issues from the community (marked with issue number)
- Things I discover myself during self-assessment (marked with [self])

## Level 1: Survive (Day 1–7)

Learn to not break. Build trust in my own code.

- [x] Write tests for existing functionality (REPL loop, command parsing) (session 2)
- [x] Add error handling for API failures (bad key, network down, rate limit) (session 2)
- [x] Add `--help` flag with usage info (session 2)
- [x] Handle Ctrl+C gracefully (cancel current turn, don't kill process) (session 2)
- [x] Fix any panics — catch all unwrap() calls and handle properly (session 2)
- [x] Add `--version` flag (session 2)

## Level 2: Be sensible about coding standards

Features that make me worth using for real work.

- [x] Git awareness: detect if we're in a repo, show branch in prompt
- [ ] Auto-commit: commit changes after successful edits (with confirmation)
- [ ] Diff preview: show what changed before applying edits
- [ ] `/undo` command: revert the last file change
- [ ] Conversation persistence: save/restore sessions to disk
- [ ] `/save` and `/load` commands for sessions
- [ ] Multi-line input: support pasting code blocks
- [x] Token usage tracking across entire session (cumulative) (REPL: per-turn + session total)
- [x] Configurable system prompt via `--system` flag or config file (`--system <file>`; UTF-8 text)
- [x] All new features need documentation (policy: see README **Developing**)
- [x] All new features need tests (policy: see README **Developing**)
- [ ] Refactor complex code automatically to make it readable and supportable

## REST Interface Level

You can run as a REST interface with `--serve`. In this mode you should support the following endpoints.

- [x] /evolve - This will run one evolution cycle. At the end of this session there is a bit of work to do.
    [x] Trap an anthropic 529 error and report "Anthropic overloaded"
    [x] all tests must pass.
    [x] the journal must be updated
    [x] If you have completed something on the roadmap, update the roadmap
    [x] clippy must not report errors or warnings 
    [x] Then bump the version number.
    [x] Build the code.
    [x] Add all changed files using git -A
    [x] Commit the git changes with a suitable message
    [x] Push the code using git push
    [x] Shut down the application and restart to get the latest changes
- [x] /health — report your health
- [x] /journal — return the journal text
- [x] /roadmap — return the roadmap text

## Level 3: Learn about the track layout

Using the track API documented in the **control** skill (`skills/control/SKILL.md`), try out the circuit communications. You can't break anything because the track control hardware will protect you.

- [x] Use the Status endpoint to get the status of the train track (session 3)
- [ ] Learn how tracks work
- [x] Learn to switch points (session 3)
- [x] Work with the sensors to see trains move (session 3)

## Level 4: Become a train controller

Features that separate a toy from a tool.

You will be given the locations of a number of trains by their position on a track segment and the direction the train is facing. Each train will have a desired destination, this being a sensor position.

- [ ] Work out a route for each train.
- [ ] Be able to display this route as a series of track commands
- [ ] Upon request, execute these routes by sending commands to the track hardware

## Boss Level: Prove It

You will be given the same starting points as in the previous level.

- [ ] Continually route trains, avoiding collisions
- [ ] You will use all track segments
- [ ] Some sensors indicate train stations. You will stop trains at the station for a period of up to 15 seconds
- [ ] Trains at a station may stop awaiting another train to arrive at an adjacent platform
