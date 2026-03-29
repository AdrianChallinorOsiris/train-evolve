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
| `POST` | `/route` | [`InitialiseRequest`](#initialiserequest) (with `destination`) | Compute routes for trains |
| `POST` | `/route/execute` | [`InitialiseRequest`](#initialiserequest) (with `destination`) | Compute and execute routes on Pi hardware |
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

### Route request

Uses the same `InitialiseRequest` format but with `destination` set on each train:

```json
{
  "trains": [
    { "train": 1, "sensor": 1, "destination": 5 },
    { "train": 2, "sensor": 10, "destination": 12 }
  ]
}
```

**Response (success):**
```json
{
  "status": "ok",
  "routes": [
    {
      "train_index": 0,
      "from_sensor": 1,
      "to_sensor": 5,
      "track_ids": [1, 2],
      "hop_count": 4,
      "description": "Path: S1 → S2 → S3 → S4 → S5. Tracks: T1, T2. ...",
      "commands": [
        { "command": "set_point", "point_id": 5, "direction": "THRU" },
        { "command": "set_track_speed", "track_id": 1, "direction": "FWD", "speed": 40 }
      ]
    }
  ]
}
```

---

## REPL Commands

The same operations are available in the interactive REPL (`cargo run` without `--serve`):

```
/initialise {"trains":[{"train":1,"sensor":21},{"train":2,"sensor":22,"direction":"bwd"}]}
/route {"trains":[{"train":1,"sensor":1,"destination":5}]}
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

# Plan a route
curl -s -X POST http://127.0.0.1:8080/route \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":1,"destination":5}]}'

# Plan and execute a route on the Pi
curl -s -X POST http://127.0.0.1:8080/route/execute \
  -H 'Content-Type: application/json' \
  -d '{"trains":[{"train":1,"sensor":1,"destination":5}]}'

# Start automatic mode
curl -s -X POST http://127.0.0.1:8080/automatic

# Check automatic status
curl -s http://127.0.0.1:8080/automatic/status

# Stop automatic mode
curl -s -X POST http://127.0.0.1:8080/stop

# System health
curl -s http://127.0.0.1:8080/health
```
