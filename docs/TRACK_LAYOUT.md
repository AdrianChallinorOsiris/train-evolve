# Track layout (`data/track_layout.toml`)

This document describes the **static model railway layout** consumed by the `yoyo` crate (`yoyo::layout`). The canonical file is **[`data/track_layout.toml`](../data/track_layout.toml)**. Edit that file to match your hardware; the Rust types in [`src/layout/model.rs`](../src/layout/model.rs) must stay in sync.

The **visual schematic legend** (colours = tracks, squares / circles / diamonds = track / sensor / point ids) is in **[`TRACK_LAYOUT_DIAGRAM.md`](TRACK_LAYOUT_DIAGRAM.md)**. Connection ids are editor-only in TOML and are not shown on the diagram.

The previous **v1** format (separate `fwd_end` / `bwd_end` and `[[points]]` table) is kept only as a snapshot in **[`data/track_layout.old`](../data/track_layout.old)** for reference.

**Workflow:** edit TOML → run `cargo test` (parsing + validation tests) → commit.

---

## File format (v2)

- **Root:** `version` (must be `2`), `tracks`, optional `[[couplers]]`, optional `stations`, optional `notes`.
- **No** top-level `[[points]]` table: turnouts are **embedded** in each track’s route as `RouteNode::Point` values with nested `entry` / `thru` / `branch` legs.
- **Encoding:** TOML. **Important:** standard TOML **inline tables** `{ ... }` cannot contain line breaks. Complex `point` nodes must be written as **one line** per inline table (or use only single-line `{ kind = "...", ... }` entries inside arrays).
- **Point legs (`PointLeg`):** In Rust, each leg stores an `along_fwd` list (same node types as a track). In TOML you may write **either** a **bare array** `entry = [{ kind = "inline" }, …]` **or** `entry = { along_fwd = [ … ] }`. For **deep** nesting, prefer **bare arrays** for each leg so you do not nest `{ along_fwd = … }` inside another inline `point` (invalid TOML).

---

## Identifiers and ranges

| Concept | Range | Notes |
|--------|-------|-------|
| Track id | 1–12 | Unique across `[[tracks]]`. |
| Sensor id | 1–24 | **Globally unique**: each sensor appears in **exactly one** `sensor` node somewhere in the layout tree. |
| Point id | 1–13 | Unique across **all** `point` nodes (any nesting depth) on a **single** track’s route tree; the same id must not appear twice on the same track. |
| Coupler id | 1–13 | Unique across `[[couplers]]`. **Must not** reuse an id that appears as a **point** id anywhere in the layout. |
| Connection id | 1–255 (u8) | **Globally paired**: each id must appear on **exactly two** endpoints on **two** tracks, with reciprocal `peer_track` / `peer_side` (see below). |

---

## Tracks (`TrackSegment`)

Each **track** is a powered segment with intrinsic **FWD** and **BWD** ends (your physical convention).

### Outer loop (Tracks 1 & 2)

Tracks 1 and 2 form a continuous loop. Following the `along_fwd` direction on both tracks:

```
Connection 1 (T2 FWD → T1 BWD)
  → Sensor 1 → Sensor 2 → Sensor 3         [Track 1]
Connection 2 (T1 FWD → T2 BWD)
  → Sensor 4 → [Coupler 1, Coupler 3] → Sensor 5   [Track 2]
  → back to Connection 1 … (loop repeats)
```

Connection 1 joins **Track 2's FWD end** to **Track 1's BWD end**.
Connection 2 joins **Track 1's FWD end** to **Track 2's BWD end**.

### General track structure

- **`along_fwd`** — Ordered list of [`RouteNode`](#route-nodes) values from the **BWD** end toward the **FWD** end: sensors, couplers, **`connection`** hops to other tracks, nested **`point`** junctions, **`buffer`**, **`inline`**, etc.

There are **no** separate `fwd_end` / `bwd_end` fields: track-to-track links are **`connection`** nodes placed in order inside `along_fwd`.

### `RouteNode` variants

| `kind` | Meaning |
|--------|--------|
| `sensor` | Occupancy sensor `id`. |
| `coupler` | Hardware coupler `id` (must exist in `[[couplers]]`). |
| `connection` | Endpoint of a **global** link: `id`, `peer_track`, `peer_side` (`fwd` or `bwd` — which port of the **peer** track this end plugs into). |
| `point` | Turnout: `id`, `entry`, `thru`, `branch` — each leg is a [`PointLeg`](#point-legs) (same `along_fwd` vocabulary as a track). |
| `buffer` | Buffer stop / end of line on that leg. |
| `inline` | No-op continuation (placeholder on a leg). |

Example (shortened; inline tables kept on one line in real files):

```toml
along_fwd = [
  { kind = "connection", id = 1, peer_track = 2, peer_side = "bwd" },
  { kind = "sensor", id = 4 },
  { kind = "point", id = 5, entry = [{ kind = "inline" }], thru = [{ kind = "coupler", id = 1 }], branch = [{ kind = "buffer" }] },
]
```

### Point legs

Each of `entry`, `thru`, and `branch` is a **`PointLeg`**: a sequence of route nodes. In TOML, **either**:

- `thru = [{ kind = "sensor", id = 4 }, { kind = "buffer" }]` (bare array — recommended for nested trees), or
- `entry = { along_fwd = [{ kind = "inline" }] }` (explicit `along_fwd` field).

Both deserialize to the same `PointLeg { along_fwd: [...] }` in Rust.

### Connections (reciprocity)

Each **connection id** identifies **one** hop between two tracks and must have **exactly two distinct endpoints** (one on each track), with reciprocal `peer_track` references. If track **A** has `{ kind = "connection", id = N, peer_track = B, peer_side = "bwd" }`, then track **B** must contain the paired endpoint `{ kind = "connection", id = N, peer_track = A, peer_side = "<the side of A that N attaches to>" }`. The same id may appear **more than twice** in the route tree if you repeat the same hop (e.g. duplicate subtrees); validation **merges identical** `(track, peer_track, peer_side)` endpoints before checking the pair rule.

---

## Couplers (`CouplerDef`)

Unchanged from v1: a **coupler** models two physical turnouts sharing one motor id — four straight legs `entry_a`, `thru_a`, `entry_b`, `thru_b`. Each leg is still a [`ConnectionRef`](#connectionref) (`track_port` or `coupler_leg`).

```toml
[[couplers]]
id = 1
entry_a = { type = "track_port", track = 1, side = "bwd" }
thru_a = { type = "track_port", track = 1, side = "bwd" }
entry_b = { type = "track_port", track = 3, side = "fwd" }
thru_b = { type = "track_port", track = 4, side = "bwd" }
```

List couplers on routes with `{ kind = "coupler", id = N }` inside `along_fwd` or inside a `point` leg wherever that hardware sits.

---

## `ConnectionRef`

Used **only** inside `[[couplers]]` (not for `connection` route nodes).

- **`track_port`** — `track` + `side` (`fwd` or `bwd`). The `track` id must exist in `[[tracks]]`.
- **`coupler_leg`** — `coupler` id + `side` (`a` or `b`) + `leg` (`entry` or `thru`).

---

## Stations

Each **station** has a display **`name`** and **`sensor_ids`**. Every listed id must appear as a `sensor` somewhere in the layout and must be in range **1–24**.

---

## Validation summary

The crate runs these checks (see `TrackLayout::validate`):

- `version == 2`
- At least one track; track ids unique and in range
- Sensor ids in range, globally unique across the whole route forest
- Point / coupler ids in range; no duplicate point (or coupler) id on the same track’s tree; coupler ids must appear in `[[couplers]]`
- No id used as both a point and a coupler on the layout
- Every `connection` id: exactly **two** endpoints, reciprocal peers, `peer_track` exists
- `[[couplers]]` `ConnectionRef` targets valid
- Station sensors exist on the layout

---

## Checklist before routing code

When you start building routers (see `ROADMAP.md`):

- [ ] Every real track 1–12 you use is present (or explicitly omitted only if unused).
- [ ] Every sensor you rely on appears exactly once as a `sensor` node.
- [ ] `along_fwd` order matches physical order along the segment for sensors, couplers, and connections.
- [ ] Every `connection` id is paired and reciprocal.
- [ ] Couplers match hardware numbering; `coupler_leg` uses the correct `side` and `leg`.
- [ ] Stations list every platform/stopping sensor you care about.

---

## API reference (Rust)

- Load: `TrackLayout::from_toml_str`, `TrackLayout::from_path`
- Validate: `TrackLayout::validate() -> Result<(), LayoutError>`
- Types: `yoyo::layout::{TrackLayout, TrackSegment, RouteNode, PointLeg, TrackSide, CouplerDef, CouplerSide, CouplerLegRole, Station, ConnectionRef, …}`
- Helper: `TrackSegment::sensors_in_route()` — preorder sensor ids along the segment’s top-level `along_fwd` (including nested `point` subtrees).

Future work: build a **navigation graph** or pathfinder on top of this model for autonomous routing.
