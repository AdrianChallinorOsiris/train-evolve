# Roadmap

My evolution path. I work through levels in order. Items come from three sources:
- This planned curriculum
- GitHub issues from the community (marked with issue number)
- Things I discover myself during self-assessment (marked with [self])

## Level 1: Survive (Day 1–7)

Learn to not break. Build trust in my own code.

- [ ] Write tests for existing functionality (REPL loop, command parsing)
- [ ] Add error handling for API failures (bad key, network down, rate limit)
- [ ] Add `--help` flag with usage info
- [ ] Handle Ctrl+C gracefully (cancel current turn, don't kill process)
- [ ] Fix any panics — catch all unwrap() calls and handle properly
- [ ] Add `--version` flag

## Level 2: Be sensible about coding standards

Features that make me worth using for real work.

- [ ] Git awareness: detect if we're in a repo, show branch in prompt
- [ ] Auto-commit: commit changes after successful edits (with confirmation)
- [ ] Diff preview: show what changed before applying edits
- [ ] `/undo` command: revert the last file change
- [ ] Conversation persistence: save/restore sessions to disk
- [ ] `/save` and `/load` commands for sessions
- [ ] Multi-line input: support pasting code blocks
- [ ] Token usage tracking across entire session (cumulative)
- [ ] Configurable system prompt via `--system` flag or config file
- [ ] All new features need documentation
- [ ] All new features need tests
- [ ] Refactor complex code automatically to make it readable and supportable

## Level 3: Learn about the track layout

Using the track API documented in the **control** skill (`skills/control/SKILL.md`), try out the circuit communications. You can't break anything because the track control hardware will protect you.

- [ ] Use the Status endpoint to get the status of the train track
- [ ] Learn how tracks work
- [ ] Learn to switch points
- [ ] Work with the sensors to see trains move

## Level 4: Become a train router

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
