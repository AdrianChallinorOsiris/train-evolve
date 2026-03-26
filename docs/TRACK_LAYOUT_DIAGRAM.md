# Track layout schematic (visual legend)

The **authoritative topology** for this layout is the **coloured schematic** (squares = track ids, circles = sensor ids, diamonds = point ids). Connection ids are **not** drawn on that diagram; they exist only in [`data/track_layout.toml`](../data/track_layout.toml) and must stay pairwise-consistent.

## Legend (matches the JPEG / Vue canvas)

| Symbol | Meaning |
|--------|--------|
| Coloured polylines | Powered **track segments** (`[[tracks]]` by `id`) |
| Square | **Track** number (1–12) |
| Circle | **Sensor** id (1–24), unique in the graph |
| Diamond | **Point** (turnout) id |
| T-bar | **Buffer** end |
| *(not shown)* | **`connection`** ids — editor-only in TOML |

## Vue.js

If you keep a Vue **visual editor** in sync with this repo, treat **track / sensor / point** ids as the shared vocabulary; **connection** ids remain an implementation detail for graph edges between tracks.

## Point 9 and track 8 (from schematic)

In **`track_layout.toml`**, **track 8**’s `along_fwd` **starts at the BWD end with point 9** (the **thru** leg faces **BWD** toward **track 7**). **Sensors 13** and **14** sit on **point 9**’s **entry** leg toward **track 6** (same cyan segment as in the drawing, encoded under the turnout rather than as separate spine nodes before the diamond). **Point 9**’s **branch** leg aligns with **track 10** (sensor **17**). The **fwd** end of track 8 continues to **track 6** via the `connection` on the entry leg after the sensors.

Place a PNG export of your diagram under `docs/images/track-layout-schematic.png` if you want the doc to link to a file in-repo (optional).
