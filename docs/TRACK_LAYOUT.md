# Track layout (`data/track_layout.toml`)

This document describes the **static model railway layout** consumed by the `yoyo` crate (`yoyo::layout`). The canonical file is **[`data/track_layout.toml`](../data/track_layout.toml)**. Edit that file to match your hardware; the Rust types in [`src/layout/model.rs`](../src/layout/model.rs) must stay in sync.

**Workflow:** edit TOML → run `cargo test` (parsing + validation tests) → commit.

---

## File format

- **Root:** `version` (must be `1`), `tracks`, optional `points`, optional `stations`, optional `notes`.
- **Encoding:** TOML. Nested structs often use **inline tables**, e.g. `fwd_end = { kind = "buffer" }`.

---

## Identifiers and ranges

| Concept | Range | Notes |
|--------|-------|--------|
| Track id | 1–12 | Unique across `[[tracks]]`. |
| Sensor id | 1–24 | **Globally unique**: each sensor appears in **exactly one** `along_fwd` entry in **one** track. |
| Point id | 1–13 | Unique across the `points` list (one row per logical point). |

---

## Tracks (`TrackSegment`)

Each **track** is a powered segment with a fixed intrinsic **FWD** and **BWD** direction (your physical convention).

- **`along_fwd`** — Ordered list of **sensors and points** along the segment in the **FWD** direction (from the **BWD** end toward the **FWD** end). List every `sensor` and `point` that sits on this segment in travel order. A point may sit **between** two sensors, **before** the first sensor (toward the BWD end), or **after** the last sensor (toward the FWD end)—there is no requirement that sensors come first or last.

Between two sensors:

```toml
along_fwd = [
  { kind = "sensor", id = 4 },
  { kind = "point", id = 1 },
  { kind = "point", id = 2 },
  { kind = "sensor", id = 5 },
]
```

Point before any sensor (toward BWD):

```toml
along_fwd = [
  { kind = "point", id = 1 },
  { kind = "sensor", id = 4 },
  { kind = "sensor", id = 5 },
]
```

Point after the last sensor (toward FWD):

```toml
along_fwd = [
  { kind = "sensor", id = 4 },
  { kind = "sensor", id = 5 },
  { kind = "point", id = 1 },
]
```

A segment may list **only** sensors, **only** points, or any mix. The same point id must not appear twice on the same track’s `along_fwd`; it may appear on **another** track if your junction model needs it.

- **`fwd_end` / `bwd_end`** — Either a **buffer** (end of line) or an **interconnect** to another track’s end.
- **`reverses_direction`** — Exactly **one** track in the layout must have this set to `true`: the **direction reverser** (train enters one way and leaves on another track with opposite sense). Validation enforces a count of **1**.

### Interconnects (reciprocity)

An interconnect from track **A**’s **FWD** end to track **B**’s **BWD** end looks like:

```toml
# On track A
fwd_end = { kind = "interconnect", peer_track = 2, peer_side = "bwd" }

# On track B (must agree)
bwd_end = { kind = "interconnect", peer_track = 1, peer_side = "fwd" }
```

Validation checks that each interconnect **points back** to the correct track and side. If you only edit one side, validation fails with `InterconnectMismatch`.

---

## Points (`PointDef`)

Points (switches) are **degree-3** junctions: **Entry**, **Thru**, and **Branch** legs. Each leg is a [`ConnectionRef`](#connectionref).

**Coupling** (two physical motors sharing one number):

- **`independent`** — One switch, one set of three legs.
- **`coupled`** — One logical point id with **two** sets of legs (`entry_a` / `thru_a` / `branch_a` and `entry_b` / …). Both move together when the hardware commands that point id.

### `ConnectionRef`

- **`track_port`** — End of a track: `track` + `side` (`fwd` or `bwd`). The `track` id must exist in `[[tracks]]`.
- **`point_leg`** — Connect to another point’s leg: `point` id + `leg` (`entry`, `thru`, or `branch`).

Use `point_leg` when the graph needs to chain through multiple points. Referenced point ids must exist in `points`.

---

## Stations

Each **station** has a display **`name`** and a list of **`sensor_ids`**. A station that spans multiple tracks lists **all** sensors that belong to it; every id must appear on **some** track’s `along_fwd` as a `sensor` entry.

---

## Validation summary

The crate runs these checks (see `TrackLayout::validate`):

- `version == 1`
- At least one track
- Track ids unique and in range
- Sensor ids in range, globally unique across all `along_fwd`
- Point ids in `along_fwd` in range, not duplicated on the same track, and each references a defined `[[points]]` row
- Exactly one `reverses_direction == true`
- Point ids unique and in range in `points`; connection refs valid
- Interconnect reciprocity
- Station sensors exist on some track

---

## Checklist before routing code

When you start building routers (see `ROADMAP.md`):

- [ ] Every real track 1–12 you use is present (or explicitly omitted only if unused).
- [ ] Every sensor 1–24 you use appears exactly once in some `along_fwd` as a `sensor`.
- [ ] `along_fwd` order matches physical order along FWD for sensors and points.
- [ ] All interconnects are reciprocal.
- [ ] The single reverser track matches the physical reversing loop.
- [ ] All points are `independent` or `coupled` as per hardware.
- [ ] Stations list every platform/stopping sensor you care about.

---

## API reference (Rust)

- Load: `TrackLayout::from_toml_str`, `TrackLayout::from_path`
- Validate: `TrackLayout::validate() -> Result<(), LayoutError>`
- Types: `yoyo::layout::{TrackLayout, TrackSegment, TrackElement, TrackEnd, PointDef, Station, ConnectionRef, …}`
- Helpers: `TrackSegment::sensors_along_fwd()`, `TrackSegment::points_along_fwd()` — iterators over ids in order along FWD.

Future work (not in this framework): build a **navigation graph** or pathfinder on top of this model for autonomous routing.
