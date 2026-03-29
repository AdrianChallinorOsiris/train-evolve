# API Reference

This document describes every HTTP endpoint and the JSON formats used by the yoyo train controller.

---

## Endpoints

| Method | Path | Body | Description |
|--------|------|------|-------------|
| `GET` | `/health` | — | System health including Pi connectivity and evolution stats |
| `GET` | `/journal` | — | Evolution journal (`JOURNAL.md`) |
| `GET` | `/roadmap` | — | Planned curriculum (`ROADMAP.md`) |
| `POST` | `/evolve` | — | Run one evolution iteration |
| `POST` | `/initialise` | [`InitialiseRequest`](#initialiserequest) | Register train positions on the layout |
| `POST` | `/program` | any JSON | Placeholder — stores payload for future track program |
| `POST` | `/simulate` | [`InitialiseRequest`](#initialiserequest) | **Dry-run** — show planned route without sending commands to hardware |
| `POST` | `/route` | [`InitialiseRequest`](#initialiserequest) | Plan routes from current to target positions |
| `POST` | `/route/execute` | [`InitialiseRequest`](#initialiserequest) | Plan and execute routes on Pi hardware |
| `POST` | `/automatic` | — | Start boss-level automation (requires prior `/initialise`) |
| `GET` | `/automatic/status` | — | Current state of all trains in automatic mode |
| `POST` | `/stop` | — | Stop automatic mode; restore saved train positions |

### Pi hardware proxy endpoints

| Method | Path | Query params | Description |
|--------|------|--------------|-------------|
| `GET` | `/pi/status` | — | Full track/point/sensor/indicator snapshot |
| `GET` | `/pi/health` | — | Pi hardware health (CPU temp, fan, memory, disk) |
| `GET` | `/pi/sensors` | — | All sensor values |
| `POST` | `/pi/sensors/reset` | — | Clear all sensors |
| `POST` | `/pi/track/{id}/speed` | `direction=FWD\|BCK\|OFF&speed=0..100` | Set track speed and direction |
| `POST` | `/pi/track/{id}/stop` | — | Emergency stop one track |
| `POST` | `/pi/allstop` | — | Emergency stop ALL tracks |
| `POST` | `/pi/point/{id}` | `direction=THRU\|BRANCH` | Switch a point |
| `POST` | `/pi/sensor/{id}` | `value=true\|false` | Force a sensor bit (testing) |

---

## JSON Formats

### `InitialiseRequest`

Registers which trains are on the layout and which sensor each train is currently sitting on. This must be called before `/automatic` can start.

```json
{
  "trains": [
    { "train": 1, "sensor": 21 },
    { "train": 2, "sensor": 22, "direction": "bwd" }
  ]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `trains` | array | yes | List of train positions (max 6) |
| `trains[].train` | integer (u8) | yes | Train identifier (must be ≥ 1, unique within the request) |
| `trains[].sensor` | integer (u8) | yes | Sensor the train is currently on (1–24) |
| `trains[].direction` | string | no | Direction the train is facing: `"fwd"` or `"bwd"` (default: `"fwd"`) |
| `trains[].destination` | integer (u8) | no | Target sensor (used internally by automation) |

**Validation rules:**
- At most **6** trains
- `train` must be ≥ 1
- No duplicate `train` ids
- `sensor` must be 1–24
- `direction` (if present) must be `"fwd"` or `"bwd"`; omitted defaults to `"fwd"`
- `destination` (if present) must be 1–24

**Response (success):**
```json
{
  "status": "ok",
  "trains": 2
}
```

### Simulate (`POST /simulate`)

**Dry-run route planning** — takes the exact same input as `/route` but never sends any commands to the hardware. Use this to review which tracks will be energised, which points will be set, and the full step-by-step sequence before committing.

```json
{
  "trains": [
    { "train": 1, "sensor": 5, "direction": "fwd" },
    { "train": 2, "sensor": 23, "direction": "bwd" }
  ]
}
```

The response includes the **current** positions (from `/initialise`), the **target** you requested, and the full **plan**:

```json
{
  "status": "simulated",
  "message": "Dry run — no commands sent to hardware. Review the plan below.",
  "current": { "trains": [...] },
  "target": { "trains": [...] },
  "plan": {
    "trains": [ ... ],
    "warnings": [ ... ]
  }
}
```

Review the `plan.trains[].steps` array to see every point set, track energise/de-energise, and sensor await. If something looks wrong, adjust the target and simulate again.

---

### Route request (`POST /route`)

The route command takes the **same format as `/initialise`** — it describes the **target state**: where each train should end up and which direction it should face. The planner reads current positions from the saved `/initialise` data.

**You must call `/initialise` first** to register current train positions.

```json
{
  "trains": [
    { "train": 1, "sensor": 5, "direction": "fwd" },
    { "train": 2, "sensor": 23, "direction": "bwd" }
  ]
}
```

The planner will:
1. Match each train by its `train` id to the current saved positions
2. Find a path from the current sensor to the target sensor
3. Determine which tracks to energise and in which direction
4. Determine which points to set
5. Sequence steps so no two trains occupy the same track simultaneously
6. De-energise each track after the train has left it
7. **Never** reset points — the track hardware resets points automatically when trains pass beyond them
8. **Never** reset a HELD state — if a track goes HELD, the plan will report the issue

If the arrival direction doesn't match what the graph finds, the response includes a warning.

**Response (success):**
```json
{
  "status": "ok",
  "plan": {
    "trains": [
      {
        "train": 1,
        "from_sensor": 1,
        "to_sensor": 5,
        "target_direction": "fwd",
        "track_ids": [1, 2],
        "hop_count": 4,
        "description": "Path: S1 → S2 → S3 → S4 → S5. Tracks: T1, T2. ...",
        "steps": [
          { "action": "set_point", "train": 1, "point_id": 5, "direction": "THRU" },
          { "action": "energise_track", "train": 1, "track_id": 1, "direction": "FWD", "speed": 40 },
          { "action": "await_sensor", "train": 1, "sensor": 2, "note": "train 1 reaching sensor 2" },
          { "action": "de_energise_track", "train": 1, "track_id": 1 },
          { "action": "energise_track", "train": 1, "track_id": 2, "direction": "FWD", "speed": 40 },
          { "action": "await_sensor", "train": 1, "sensor": 5, "note": "train 1 reaching sensor 5" },
          { "action": "de_energise_track", "train": 1, "track_id": 2 }
        ],
        "already_there": false
      }
    ],
    "warnings": []
  }
}
```

**Step types:**
| Action | Description |
|--------|-------------|
| `set_point` | Set a point before the train enters the next track segment |
| `energise_track` | Power a track segment so the train can move onto it |
| `await_sensor` | Wait for the train to trigger a sensor (confirms arrival) |
| `de_energise_track` | Stop powering a track after the train has left it |

---

## REPL Commands

The same operations are available in the interactive REPL (`cargo run` without `--serve`):

```
/initialise {"trains":[{"train":1,"sensor":21},{"train":2,"sensor":22,"direction":"bwd"}]}
/simulate {"trains":[{"train":1,"sensor":5,"direction":"fwd"}]}
/route {"trains":[{"train":1,"sensor":5,"direction":"fwd"}]}
/program {"any":"json"}
/automatic
/automatic/status
/stop
/health
/journal
/roadmap
/pi status
/pi health
/pi sensors
/pi sensors reset
/pi track speed <id> OFF|FWD|BCK <0-100>
/pi track stop <id>
/pi allstop
/pi point <id> THRU|BRANCH
/pi sensor <id> true|false
```

---

## Examples (curl)

```bash
# Register two trains (train 2 is facing backward)
curl -s -X POST http://127.0.0.1:8080/initialise \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":21},{"train":2,"sensor":22,"direction":"bwd"}]}'

# Simulate a route (dry run — review before executing)
curl -s -X POST http://127.0.0.1:8080/simulate \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":5,"direction":"fwd"}]}'

# Plan a route (target state — same format as /initialise)
curl -s -X POST http://127.0.0.1:8080/route \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":5,"direction":"fwd"}]}'

# Plan and execute a route on the Pi
curl -s -X POST http://127.0.0.1:8080/route/execute \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":5,"direction":"fwd"}]}'

# Start automatic mode
curl -s -X POST http://127.0.0.1:8080/automatic

# Check automatic status
curl -s http://127.0.0.1:8080/automatic/status

# Stop automatic mode
curl -s -X POST http://127.0.0.1:8080/stop

# System health
curl -s http://127.0.0.1:8080/health
```
