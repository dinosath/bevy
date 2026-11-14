---
title: Meshlet tuning, statistics and deterministic baking
authors: []
pull_requests: []
---

## What changed

- `MeshletMesh::stats()` reports meshlet/triangle/BVH counts and byte sizes (including the lz4
  file size), so tooling and budgets don't have to guess.
- `MeshletBuildConfig` and `MeshletMesh::from_mesh_with_config` expose meshlet size limits
  (default unchanged: 256 vertices / 128 triangles; 128 triangles also matches the software
  rasterizer's one-triangle-per-thread design).
- Baking is now deterministic: METIS is not safe to call concurrently (its random number state is
  global), so concurrent partitioning calls produced slightly different meshlets for identical
  input. Calls are now serialized; the rest of the bake stays parallel.
- `MeshletSettings` (main-world resource): `shadow_lod_error_scale` and
  `shadow_cascade_lod_error_scale_step` render shadow views from coarser clusters. **They trade
  shadow quality for speed**: coarse clusters drop thin geometry, so far shadows weaken (aerial
  benchmark: 84 % of shadowed ground kept at `1.5` for −16 % frame time, 41 % at `2.0`). Defaults are
  `1.0` (no change). `rebuild_shadow_depth_pyramid_after_second_pass` controls an extra
  per-shadow-view depth pyramid build, off by default (−15.8 % frame, no quality impact).
- Cull-queue overflow is detected and handled gracefully: queue occupancy is read back from the GPU (+0.05 ms measured), `MeshletPressure` exposes `peak_queue_entries`/`overflow_events`/`lod_error_scale`, and by default the LOD error budget rises when a queue overflows or nears capacity, so quality degrades instead of clusters being silently dropped (`MeshletSettings::degrade_on_queue_pressure`, `detect_queue_pressure`). A warning tells you to raise `cluster_buffer_slots`.
- `MeshletSettings::retained_instances` (opt-in): keeps the meshlet instance table on the GPU across frames and only touches/uploads instances that changed, instead of rebuilding and re-uploading everything every frame. With 100 000 meshlet instances on an integrated GPU the frame went from 18.7 ms to 8.5 ms (200 000: 36.1 → 12.7 ms); scenes where nearly every instance moves every frame get slower (+30 % at 100 %), so it is off by default. `validate_retained_instances` checks the table against a from-scratch rebuild every frame.
- Shadow views record finer GPU spans (`first_cull_and_raster`, `hzb_after_first_pass`,
  `second_cull_and_raster`, `resolve_depth`, `hzb_final`) for `RenderDiagnosticsPlugin`.

## Why it matters

On an integrated GPU with 12k high-poly trees, shadow cascades were ~80 % of the frame, and a large
share of that was fixed per-view work (depth pyramid builds), not geometry.
