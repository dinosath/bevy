---
name: bevy-world-streaming
description: How world streaming is implemented in this repo's bevy_world_streaming crate — plugin wiring, scheduler, cell sources, budgets, geometry pages, HLOD fallback, diagnostics, simulation harness. Use when extending or integrating the streaming system.
---

# bevy_world_streaming (project crate)

## Purpose
Reference for the streaming layer that sits **on top of** Bevy's renderer (nothing in Bevy upstream does cell/page streaming — see `docs/open_world/bevy_audit.md`).

## When to use
Adding cell content types, changing budgets/priorities, integrating a renderer path (standard instanced, meshlet), or debugging streaming stalls. Concepts: `level-streaming`; prediction: `predictive-streaming`.

## Layout (`crates/bevy_world_streaming/src`)
| Module | Role | Pure/ECS |
|---|---|---|
| `coords` | `CellCoord`, `CellGrid` (cell size, `distance_to_cell`, `for_each_cell_in_radius`), `WorldPosition`, `FloatingOrigin` (rebase) | pure |
| `predict` | `MotionPredictor`/`MotionState::predict`, `Route`, `InterestBuilder` (tiers Mandatory/Wanted/Speculative) | pure |
| `scheduler` | `StreamingScheduler`: state machine, priority, budgets, eviction | pure (unit-testable, deterministic) |
| `pages` | `AssetPages`/`PageDesc` (coarse→fine), `PageTable::reconcile(demands) -> PageDelta`, prefix residency under a byte budget | pure |
| `lod` | `Representation`, `LodThresholds`, `LodSelector` (hysteresis), `resolve_fallback` (finest resident ≤ desired; flags hard misses) | pure |
| `hlod` | `build_forest_hlod(trees, cfg) -> (Vec<ForestRegion>, HlodStats)`; `MergedGeometry::into_mesh` (feature `mesh`) | pure + `Mesh` |
| `diagnostics` | `StreamingStats`, `StreamingDebug`, `draw_streaming_gizmos` (feature `gizmos`) | ECS |
| `plugin` | `WorldStreamingPlugin`, `StreamingFocus`, `CellSource`/`CellDecoder` traits, `UploadMode`, messages `CellActivated/Deactivated/Evicted` | ECS |
| `sim` | deterministic headless simulator (IO latency/bandwidth model, driving routes) used by tests/benches | pure |

**Rule**: all logic lives in the pure modules; `plugin` is thin glue so behavior is testable without a GPU or window.

## Data flow in the plugin (all in `Update`, chained sets)
```
Focus  update_focus      StreamingFocus GlobalTransform (+ origin_offset) → MotionPredictor → StreamingState.motion
Poll   poll_tasks        check_ready() on IO/decode Tasks (never blocks) → scheduler.io_complete / decode_complete / failed
Plan   plan_streaming    InterestBuilder.build → scheduler.tick → Actions: StartIo (IoTaskPool), StartDecode (AsyncComputeTaskPool),
                         StartUpload (Immediate: completes next frame | External: StreamingState::take_pending_uploads + complete_upload),
                         Activate/Deactivate/Evict → messages CellActivated/CellDeactivated/CellEvicted
Stats  publish_stats     StreamingStats (bandwidths, counts, critical-miss frames)
```
Order against it with `StreamingSystems::{Focus, Poll, Plan, Stats}`. Spawn cell content in a system reading `CellActivated` and `StreamingState::prepared(coord)`.

## Project conventions
- **Config in one resource** (`StreamingConfig`): radii, hysteresis, budgets (`ram_budget_bytes`, `vram_budget_bytes`, `max_inflight_io`, `upload_bytes_per_frame`, `max_activations_per_frame`), prediction horizon, route-corridor width. No magic numbers in systems.
- **`CellSource` trait** abstracts storage: `fn read_cell(&self, c: CellCoord) -> CellFuture<CellBlob>` (+ optional `size_hint`); tests use an in-memory source with an `IoModel`. Real source: file/pack reader (see `asset-pipelines`).
- Cancel by **generation**: every request carries `(coord, generation)`; stale completions are dropped.
- The *mandatory ring* is pinned; speculative loads are capped to a fraction of IO budget.
- GPU residency: the scheduler owns *accounting* (`SchedulerStats::gpu_bytes`); actual buffers are owned by the renderer integration (e.g. meshlet manager / instance buffers). With `UploadMode::External` the integration drains `take_pending_uploads()` within its own byte budget and calls `complete_upload(coord, generation)`; eviction is signalled by `CellEvicted`.
- Geometry pages: a tree = metadata + hierarchy + pages 0..N coarse→fine. Required page count for a view = f(projected error); distant instances only require page 0..k, so **VRAM scales with visible screen-space detail, not instance count**. Honest status: this is implemented as the *policy and accounting layer*; the Bevy meshlet renderer still uploads whole `MeshletMesh`es, so true per-page GPU residency inside meshlet shaders needs upstream meshlet changes (see `bevy-meshlets`, audit).

## Implementation patterns
- New cell content type: add to `CellBlob`'s typed sections + a spawner registered with the plugin; never spawn from IO tasks.
- New priority term: extend `Prio`/`InterestBuilder` ordering in `scheduler.rs`/`predict.rs` with a unit test and re-run the sim benchmark (`cargo bench -p bevy_world_streaming` / sim example) comparing against baseline (`rust-performance` report format).
- Tuning workflow: run `sim` sweeps at 30/60/100/150/200/250 km/h; look at `critical_miss_frames`, `fallback_cell_frames`, request latency p99, IO MB/s, peak GPU MiB, speculative hit/waste (`cargo run -p bevy_world_streaming --release --example streaming_sim`).

## Common mistakes
- Spawning entities inside async tasks; blocking on `Task` results; forgetting the generation check.
- Letting `Active` radius exceed `Resident` radius (render needs data that isn't there).
- Budget accounting that ignores in-flight bytes (overshoot) or ignores VRAM.
- Using wall-clock in the pure modules (breaks determinism) — pass `dt`/`now` in.

## Performance considerations
`tick()` must be O(cells in interest + changed) — measured by the criterion bench; allocation-free in steady state (scratch buffers retained in the scheduler).

## Testing requirements
`cargo test -p bevy_world_streaming`; benches `cargo bench -p bevy_world_streaming`; headless sim examples. Any new budget or priority rule needs a test proving the invariant and a sim run showing no regression in hard misses.

## References
`crates/bevy_world_streaming/**`, `docs/open_world/architecture.md`, `docs/open_world/benchmarks.md`
