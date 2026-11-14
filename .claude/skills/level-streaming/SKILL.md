---
name: level-streaming
description: Engine-agnostic level/world streaming theory — residency state machines, priority queues, budgets, prefetch, hysteresis, eviction, fallbacks, diagnostics. Use to reason about streaming correctness before touching Bevy-specific code.
---

# Level / world streaming (concepts)

## Purpose
Keep exactly the data needed for the next few seconds resident, never block the frame, and degrade gracefully (coarser, never missing).

## Pipeline
```
disk ─ async IO ▶ CPU memory ─ decompress ▶ prepared ─ upload (budgeted) ▶ GPU resident ─ activate ▶ visible/simulated
```
Each arrow is a **bounded queue with an explicit per-frame or in-flight budget**. The render thread only *polls* and *applies* results; it never waits.

## Cell residency state machine
```
Unloaded → Requested → Loading(IO) → Decoding → Uploading → Resident → Active
                                  ↘ Failed(retry/backoff)
Active/Resident → Evicting → Unloaded         (hysteresis: leave only when beyond unload radius)
```
- `Resident` = data available (GPU + CPU as required). `Active` = spawned/simulated/rendered. Splitting them lets you prefetch without paying simulation/render cost.
- Transitions are driven by **desired state** computed each tick from interest; the scheduler diffs desired vs. current. Idempotent; safe to recompute every frame.
- **Generation counters** on requests: results from a cancelled/evicted request are discarded.

## Priority
`priority = f(distance_to_interest, time_to_need, class, mandatory)`; lower **time-to-need** first (use predicted arrival time = distance / closing speed), then proximity. *Mandatory* ring (player's immediate cells) always outranks speculative prefetch. Use a binary heap rebuilt only on interest change, or a bucketed priority queue (few discrete tiers) to keep tick cost O(changed).

## Budgets (all explicit, all in config)
- `max_inflight_io` (bounds memory & seek storms), `decode_per_frame` or µs budget, `upload_bytes_per_frame` (PCIe/frame-time), `ram_budget_bytes`, `vram_budget_bytes`, `max_activations_per_frame` (entity spawn spikes).
- When over budget: evict lowest-priority non-mandatory residents first (LRU by last-needed time breaking ties), never block.

## Hysteresis
Load radius `R_load` < unload radius `R_unload` (e.g. +10–25% or ≥ 1 cell). Prevents thrash at the boundary; also applies to LOD/HLOD switches (separate in/out thresholds).

## Prefetch
Speculative interest from motion prediction (`predictive-streaming`). Prefetched cells enter `Resident` but not `Active` until actually inside the activation radius. Speculative loads are cancellable and lower priority than mandatory ones; cap their share of IO budget (e.g. ≤ 50%).

## Fallbacks (no pop-in rule)
Rendering never depends on the best data being present. Always have an always-resident coarse chain: `Impostor/billboard ← HLOD proxy ← coarse mesh ← full`. The *selected* representation is `min(desired_detail, best_resident_detail)`; as finer data arrives, transition with hysteresis/crossfade. Metrics: **fallback frames** (how long a visible object used a coarser-than-desired representation) and **hard misses** (nothing to draw) — hard misses must be 0.

## Diagnostics (required)
Counts per state, queue depths/high-water, IO/upload bytes per frame and per second, RAM/VRAM resident vs. budget, evictions, retries/failures, per-request latency (request→resident) histogram, hard-miss/fallback counters. Overlay + gizmos in-engine (`bevy_world_streaming::diagnostics`).

## Common mistakes
- Priority by distance only (a cell 100 m *behind* a car at 200 km/h matters less than one 400 m ahead).
- Unbounded in-flight requests (IO thrash, memory spikes).
- No unload hysteresis ⇒ load/unload loops at boundaries.
- Evicting what you just prefetched because budget accounting ignored in-flight bytes.
- Counting only RAM, forgetting GPU residency (VRAM budget separate).

## Testing requirements
Pure scheduler tests with fake clock and synthetic `IoModel{latency, bandwidth}`; property tests: budgets never exceeded, mandatory ring always resident given sufficient bandwidth, no state skips, no resurrection of cancelled requests. See `game-testing`, `bevy_world_streaming::sim`.

## References
`crates/bevy_world_streaming/src/scheduler.rs`, `docs/open_world/architecture.md`
