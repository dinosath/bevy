# Open-world streaming & rendering architecture

Goal: Forza Horizon / GTA-class worlds on Bevy, built on the existing GPU-driven renderer. This document says what each layer is, which piece is **implemented**, which is **upstream Bevy**, and what is **not done**.

```
                         OPEN WORLD
                             │
              WORLD PARTITION  (coords.rs)            ✅ implemented, tested (pure)
                             │
              STREAMING SCHEDULER (scheduler.rs)      ✅ implemented, tested, benchmarked
              + PREDICTION (predict.rs)               ✅ kinematic + route corridor
              + ECS/async glue (plugin.rs)            ✅ IoTaskPool/AsyncComputeTaskPool, bounded
              ┌──────────────┴──────────────┐
        WORLD CELL DATA              GEOMETRY PAGES
        (CellSource/CellDecoder)     (pages.rs)       ✅ policy + accounting, tested
              └──────────────┬──────────────┘
                      GPU RESIDENCY
                      Bevy: PersistentGpuBuffer (meshlet), mesh slab allocator
                      project: UploadMode::External hook, budgets      ⚠ renderer wiring NOT done
                             │
                      MESHLET HIERARCHY  (Bevy, experimental)           ✅ upstream
              ┌──────────────┼──────────────┐
             LOD         FRUSTUM         OCCLUSION                      ✅ upstream (GPU)
              └──────────────┼──────────────┘
                       GPU INDIRECT                                     ✅ upstream
                       MESH SHADERS                                     ⚠ upstream WIP, no integration
                             ▼
                          RENDER
```

## Layers

### World partition — `coords.rs`
`CellCoord{x,z}` (i32, ground plane X/Z), `CellGrid{cell_size}` (`cell_of` uses `floor`, exact at edges and for negatives; `distance_to_cell` is the *single* distance metric used by every radius test; `for_each_cell_in_radius` is deterministic row-major), `WorldPosition{cell, local:f32}`, and `FloatingOrigin` (rebases when the focus drifts past `rebase_distance`, returning the shift; absolute position preserved < 1 mm in tests). **Not done:** the system that shifts every `Transform` by the rebase delta.

### Streaming scheduler — `scheduler.rs`
Pure state machine, driven by `tick(now, focus, interest, &mut actions)`:

```
absent → Loading → Loaded → Decoding → Prepared → Uploading → Resident ⇄ Active
          └ Failed(backoff) → absent        cancel/evict at any stage (generation counters drop stale results)
```
* **Bounded everywhere**: `max_inflight_io`, `max_inflight_decode`, `upload_bytes_per_frame`, `max_activations_per_frame`, `ram_budget_bytes`, `vram_budget_bytes`. Speculative IO capped by `speculative_io_fraction`.
* **Priority** `(tier, time_to_need [speculative only], heading-biased distance, coord)` — total order, deterministic. Tiers: Mandatory (pinned, may exceed budget → counted in `budget_overruns`), Wanted, Speculative.
* **Hysteresis**: evict only beyond `load_radius + unload_hysteresis`; deactivate beyond `activation_radius + activation_hysteresis`.
* **Admission under pressure** evicts only strictly less urgent cells (never Mandatory).
* Allocation-free steady state (retained scratch buffers); ≈ 9 µs for interest build + tick (see benchmarks).

### Prediction — `predict.rs`
`MotionPredictor` (exponential filters, deterministic) → `MotionState::predict(t) = p + v·t + ½·a·t²` (acceleration clamped). `Route` polyline with projection/arc-length. `InterestBuilder` unions: mandatory ring (+ explicit teleport/destination targets), wanted ring (heading-biased), speculative samples along the predicted path and along the **route corridor** (only when on-route within `route_max_lateral`). Prediction never removes mandatory interest.

### ECS glue — `plugin.rs`
`WorldStreamingPlugin { config, source: Arc<dyn CellSource>, decoder: Arc<dyn CellDecoder>, upload_mode }`. IO on `IoTaskPool`, decode on `AsyncComputeTaskPool`, results polled with `check_ready` (never blocks). Emits `CellActivated/Deactivated/Evicted` messages; game code spawns/despawns content and reads decoded data from `StreamingState::prepared`.

### Geometry pages — `pages.rs`
Assets are `pages 0..N` ordered coarse→fine; residency is always a **prefix**, so every state is renderable. `required_pages(distance, focal_px, threshold_px)` maps screen-space error to a prefix length; `PageTable::reconcile(demands)` grows nearest-first (roots before finer pages), evicts idle assets finest-first in LRU order, then excess pages of demanded assets. Result: far trees need page 0 only (test: a near tree's prefix is >50× the bytes of a far one).

### LOD / HLOD / fallback — `lod.rs`, `hlod.rs`
`Representation: Culled < ForestHlod < Impostor < Proxy < Medium < Full`. `LodSelector` picks by projected size with hysteresis; `resolve_fallback(desired, resident)` returns the finest resident representation ≤ desired and flags `hard_miss` only if nothing can be drawn — with an always-resident coarse chain there are none. `build_forest_hlod` groups trees into regions (deterministic, `BTreeMap`) and emits one merged canopy mesh per region (bounded triangles by sub-grid merging).

### Diagnostics — `diagnostics.rs`
`StreamingStats` (cells per state, CPU/GPU bytes, pending requests, IO/upload bandwidth, evictions/cancels/failures, speculative loaded/hit/waste, critical-miss frames, speed, predicted position, LOD histogram, visible/culled/occluded cluster slots), `summary()` text, and with feature `gizmos` `draw_streaming_gizmos` (cells coloured by state, radii, predicted path). Visible/culled/occluded *cluster* counters must be filled by a renderer integration — Bevy has no public meshlet counter yet.

## What is not done (honest list)

1. **Per-page GPU residency inside the meshlet BVH.** `PageTable` decides; `MeshletMeshManager` still uploads whole meshes. Needs: page-addressable BVH/cluster ranges, GPU feedback of requested pages, fallback to coarser BVH levels (all upstream meshlet work).
2. **Mesh-shader meshlet path.** Upstream only has the pipeline plumbing/example.
3. **Impostor/billboard generation and rendering**, texture streaming, terrain, road/building generators, vehicles/NPC simulation LOD, save system.
4. **Floating-origin transform shifting** and renderer integration of `Representation` switching (the forest benchmark uses `VisibilityRange` + merged HLOD meshes directly).
5. **Measured results on a streaming-bound real world** — the streaming numbers come from a deterministic simulator with a synthetic IO model, not real disk reads.

## Where things live
`crates/bevy_world_streaming` (library, tests, bench, `examples/streaming_sim.rs`), `examples/large_scenes/forest_benchmark` (render benchmark), `.claude/skills/*` (agent skills), `docs/open_world/*` (this).
