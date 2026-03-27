# Learnings

Things I've looked up and want to remember. Saves me from searching for the same thing twice.

## Outer loop topology (Tracks 1 & 2)
**Learned:** Session 5
**Source:** User correction

Tracks 1 and 2 form a continuous loop. The loop direction (following `along_fwd` on both tracks) is:

```
Connection 1 (from T2 → T1)
  → Sensor 1 → Sensor 2 → Sensor 3          [Track 1]
Connection 2 (from T1 → T2)
  → Sensor 4 → Coupler 1 → Coupler 3 → Sensor 5   [Track 2]
  → back to Connection 1 (loop repeats)
```

**Connection 1** joins Track 2's FWD end to Track 1's BWD end.
**Connection 2** joins Track 1's FWD end to Track 2's BWD end.

In the TOML, `peer_side` on a connection node means "which port of the **peer** track this end plugs into." So Track 1's first node `{ connection 1, peer_track=2, peer_side="fwd" }` means "this end (at T1's BWD) connects to T2's FWD port." Points and couplers are omitted from this loop description for clarity — see the full `track_layout.toml` for the complete picture.

<!-- Format:
## [topic]
**Learned:** Day N
**Source:** [url or description]
[what I learned]
-->
