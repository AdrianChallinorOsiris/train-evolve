# Example Routes

Drop JSON files here to define test scenarios for the route planner. Each file
describes an initial train state and a set of target destinations. They are
automatically loaded and tested by `cargo test`.

## File format

```json
{
  "description": "Two trains from sidings to stations",
  "current": {
    "trains": [
      {"train": 1, "sensor": 21, "direction": "fwd"},
      {"train": 2, "sensor": 20, "direction": "fwd"}
    ]
  },
  "target": {
    "trains": [
      {"train": 1, "destination": "waterloo", "direction": "fwd"},
      {"train": 2, "destination": "bridge", "direction": "fwd"}
    ]
  },
  "expect": "ok"
}
```

### Fields

| Field | Required | Description |
|-------|----------|-------------|
| `description` | Yes | Human-readable name for the test case |
| `current` | Yes | `InitialiseRequest` — where the trains are now |
| `target` | Yes | `RouteRequest` — where the trains should go |
| `expect` | Yes | `"ok"` (route should succeed) or `"error"` (route should fail) |

### Valid sensors

The layout has sensors **1–21, 23, 24** (no sensor 22).

### Station names

`sidings`, `blackheath`, `waterloo`, `bridge`, `industrial`
