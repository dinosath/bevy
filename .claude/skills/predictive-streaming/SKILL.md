---
name: predictive-streaming
description: Motion-predictive and route-aware prefetching for high-speed traversal (30–250+ km/h) — velocity/acceleration extrapolation, camera direction, road corridors, time-to-need priority, required lookahead math. Use when tuning streaming for vehicles or fast cameras.
---

# Predictive streaming

## Purpose
At speed, the *distance you need data ahead of you* is `v · (t_latency + t_margin)`. Prediction decides **which cells to request** and **in what order**, so the data is resident before the camera needs it.

## Core math
- **Predicted position** `p(t) = p₀ + v·t + ½·a·t²` (clamp `a` and `t`; acceleration is noisy — apply smoothing, and fall back to constant-velocity when |a| is implausible).
- **Required lookahead distance**: `D = v · (T_io + T_decode + T_upload + T_margin)`; with `T_io` from the *p95 measured* request latency (`StreamingStats`), not an assumed one. Example: 250 km/h = 69.4 m/s; with 1.2 s total pipeline latency and 1 s margin ⇒ D ≈ 153 m *beyond the needed radius*.
- **Speed → radius**: `R_prefetch(v) = R_base + v · T_horizon`, `T_horizon` ≈ 3–6 s of flight. Radius grows with speed; width of the interest shape *shrinks* (cone/corridor) because you can only be in a narrow set of places.
- **Time-to-need** for a cell: `t = max(0, dist_ahead − R_active) / max(closing_speed, ε)`. Sort speculative requests by ascending `t`. Cells *behind* have negative closing speed ⇒ lowest priority (but keep hysteresis ring so reversing doesn't thrash).
- **Road/path corridor**: sample the route (polyline/spline) ahead of the vehicle's projection onto the route for arc-length `L = v · T_horizon`; take all cells within `corridor_half_width` of those samples. At junctions include all branches within the horizon weighted by turn probability (planned destination ⇒ weight 1 on that branch). If off-route (no route, or lateral error > threshold), fall back to the kinematic cone.
- **Camera direction**: widen the interest toward the look direction when it differs from velocity (drift, look-back); camera yaw rate predicts upcoming view — include a cone around `yaw + yaw_rate·t`.
- **Destination/teleport**: a known destination (fast travel, respawn, mission start) is an *explicit* high-priority interest; stream the destination's mandatory ring while the transition animation plays.

## Implementation (`bevy_world_streaming::predict`)
- `MotionPredictor::push(pos, dt)` → smoothed velocity/acceleration (exponential filters, deterministic).
- `Route` (polyline with cumulative arc length): `project(p) -> RouteProjection{s, lateral, point}`, `project_near(p, hint_s, window)`, `point_at(s)`; corridor cells are sampled inside `InterestBuilder::build` every `cell_size/2` of arc length.
- Interest assembly: `mandatory ring ∪ kinematic cone/prediction samples ∪ route corridor ∪ explicit destinations`, each tagged with `time_to_need` and `speculative: bool`.

## Project conventions
- Prediction only ever *adds* speculative interest; it must never remove mandatory cells.
- Speculative IO share capped (default ≤ 50% of `max_inflight_io`), so a wrong prediction cannot starve a needed cell.
- All thresholds in `StreamingConfig` (`prediction_horizon_s`, `max_prediction_accel`, `corridor_half_width_m`, `speculative_io_fraction`).
- Report **prediction accuracy**: fraction of loaded-speculative cells that became needed within horizon (waste ratio) next to stall metrics.

## Common mistakes
- Predicting with raw per-frame velocity (jitter → interest flicker): smooth it.
- Over-trusting acceleration (explosive over-prediction on bumps/collisions).
- Prefetch radius fixed regardless of speed.
- Treating all speculative cells equal; ignoring time-to-need.
- No fallback when the route is stale (player left the road).

## Performance considerations
Corridor sampling is O(L / step); keep step ≈ cell_size/2. Don't recompute cell sets per frame if the focus moved < ε and the route hasn't changed — cache by (cell of focus, quantized heading).

## Testing requirements
Sweep 30/60/100/150/200/250 km/h on straight, curved, switchback, and teleport routes through `sim`; assert critical misses = 0 up to the configured guarantee speed given the IO model (guarantee ≈ `(load_radius − critical_radius) / pipeline latency`), and compare `baseline (radius only)` vs `predictive` vs `route-aware` — numbers in `docs/open_world/benchmarks.md`.

## References
`crates/bevy_world_streaming/src/{predict.rs,scheduler.rs,sim.rs}`, `level-streaming`, `open-world`
