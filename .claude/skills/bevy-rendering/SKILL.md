---
name: bevy-rendering
description: Deep guide to Bevy renderer internals on main — main world → extract → render world → prepare → queue → GPU passes. How to add or modify rendering without fighting the architecture. Use for any change in bevy_render/bevy_pbr/bevy_core_pipeline or custom render code.
---

# Bevy renderer internals (0.20-dev)

## Purpose
Modify/extend rendering *with* Bevy's pipeline. Reuse its buffers, batching, visibility, specialization and GPU preprocessing instead of side-channeling.

## When to use
New draw types, GPU buffers, passes, per-view data, performance work in rendering. For meshlets see `bevy-meshlets`; passes/schedules see `bevy-render-graph`; WGSL see `bevy-shaders`/`bevy-gpu`.

## The frame
```
Main World (gameplay, ECS, assets)           PostUpdate: VisibilitySystems (bevy_camera::visibility)
        │  ExtractSchedule  (bevy_extract; runs with exclusive access to both worlds)
        ▼
Render World (RenderApp sub-app, separate World, runs on render thread when pipelined)
  RenderSystems order (crates/bevy_render/src/lib.rs `enum RenderSystems`):
   ExtractCommands → PrepareAssets → PrepareMeshes → CreateViews → Specialize → PrepareViews
   → Queue{ QueueMeshes, QueueSweep } → PhaseSort → Prepare{ PrepareResources → …Batch/Write/Collect phase buffers → Flush → PrepareBindGroups }
   → Render (Core3d/Core2d schedules record GPU commands) → Cleanup
        ▼
GPU (wgpu): compute (preprocess/cull) → depth prepass → shadow → main passes → post
```
Pipelined rendering (`crates/bevy_render/src/pipelined_rendering.rs`): render world for frame N runs concurrently with main world N+1. **Nothing in extract may hold references into the main world; extraction copies.**

## Core concepts
- **Extraction**: `ExtractComponent`/`ExtractResource` derives with `#[extract_app(RenderApp)]` (`crates/bevy_extract/src/{extract_component,extract_resource,extract_instances}.rs`). Entities are synced via `SyncComponent`/`MainEntity`↔`RenderEntity` (`sync_world.rs`). Keep extracted data *small* (POD, ids, transforms); do not extract per-instance data for static vegetation every frame — retain it in a render-world resource and extract only changes (`Changed`/`Added`, or an explicit "dirty" flag).
- **RenderAsset**: `RenderAsset` / `prepare_assets::<A>` converts main-world assets to GPU form (`crates/bevy_render/src/render_asset.rs`, `erased_render_asset.rs`). Mesh data lives in a slab allocator (`mesh/allocator.rs`, `slab_allocator.rs`) — meshes share big vertex/index buffers, which is what makes batching and indirect draw possible.
- **Specialization & pipeline cache**: pipelines are keyed and compiled asynchronously (`PipelineCache`, `render_resource/pipeline_cache.rs` uses `Task`s). A "pipeline not ready" frame is normal — skip drawing, don't block.
- **Render phases**: `BinnedRenderPhase`/`SortedRenderPhase` (`render_phase/`). Items are *queued* per view during `Queue`, sorted, then drawn via `RenderCommand`s. Opaque3d is binned (batchable), transparent is sorted.
- **GPU preprocessing** (default): per-instance transforms uploaded to a buffer; a compute pass (`bevy_pbr/src/render/mesh_preprocess.wesl`, driver `gpu_preprocess.rs`) does frustum + (optional) two-phase occlusion culling and writes **indirect draw parameters** (`build_indirect_params.wesl`). Mode enum: `GpuPreprocessingMode::{None, PreprocessingOnly, Culling}` — `Culling` is the default; camera component `NoIndirectDrawing` opts out. Source: `crates/bevy_render/src/batching/gpu_preprocessing.rs`.
- **Views**: cameras, shadow cascades/cubemap faces, and probes are all *views*, each with `ExtractedView`, `ViewUniform`, depth/colour targets (`crates/bevy_render/src/view`). Per-view GPU data goes on the view entity.
- **Bind groups**: build in `PrepareBindGroups`; cache by key; avoid recreating every frame (`combined_bind_group.rs`, `material_bind_groups.rs` – bindless materials).

## Project conventions
- **Don't fight the architecture**: add data via components + extract; add passes as systems in `Core3d`; add GPU resources in `RenderStartup`/`PrepareResources`; order with `RenderSystems` sets.
- Prefer extending an existing compute pass/buffer (e.g. extra culling predicate) over a parallel pipeline — but only after reading `gpu_preprocess.rs`.
- Keep render-world systems free of main-world `Query`s (they don't exist there) and free of blocking calls.
- Any new per-frame GPU upload goes through a *budgeted* writer (see `PersistentGpuBuffer::perform_writes` in `meshlet/persistent_buffer.rs`).

## Implementation patterns
- New renderable kind: `Component` (main) → `ExtractComponent` → render-world `Resource` of instance data (retained) → `Prepare` writes into `StorageBuffer`/`BufferVec` → `Queue` adds phase items → draw fn or compute indirect.
- Retained GPU residency of heavy assets: slab/range allocator + `queue_write` + `mark_slice_unused` (meshlet manager pattern). Free ranges, don't rebuild.
- Reading data back (stats/tests): `gpu_readback.rs` (async, ≥1 frame latency) — never stall for it.

## Common mistakes
- Putting `Handle<Image>` lookups in `Prepare` before `PrepareAssets` completed (ordering).
- Re-uploading whole buffers each frame; creating buffers/bind groups per frame.
- Extracting per-entity for 100k+ instances every frame.
- Assuming main-world components exist in the render world (only extracted ones do).
- Blocking on the pipeline cache or on `map_async` results.

## Performance considerations
CPU cost per frame ≈ extract + prepare + queue; all scale with *entity/instance count* unless retained/GPU-driven. GPU cost splits geometry / raster / fragment / bandwidth (`GPU-profiling`).

## Testing requirements
Headless render tests via `bevy_render::test_utils` and CI screenshot tests (`bevy_dev_tools::ci_testing`); GPU-feature-dependent tests must skip gracefully when features are missing (`check_meshlet_features` pattern in meshlet/mod.rs — note it `exit(1)`s; tests must pre-check `RenderDevice::features()`).

## References
`crates/bevy_render/src/{lib.rs,batching/gpu_preprocessing.rs,render_phase,render_asset.rs,view,renderer}`, `crates/bevy_extract/src`, `crates/bevy_pbr/src/render/{mesh.rs,gpu_preprocess.rs,mesh_preprocess.wesl}`, `docs/debugging.md`
