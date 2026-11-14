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
- `MeshletSettings` (main-world resource): `shadow_lod_error_scale` renders shadow views from
  coarser clusters (e.g. `2.0` allows 2 pixels of error), and
  `rebuild_shadow_depth_pyramid_after_second_pass` controls an extra per-shadow-view depth pyramid
  build.
- Cull-queue overflow is detected and handled gracefully: queue occupancy is read back from the GPU (+0.05 ms measured), `MeshletPressure` exposes `peak_queue_entries`/`overflow_events`/`lod_error_scale`, and by default the LOD error budget rises when a queue overflows or nears capacity, so quality degrades instead of clusters being silently dropped (`MeshletSettings::degrade_on_queue_pressure`, `detect_queue_pressure`). A warning tells you to raise `cluster_buffer_slots`.
- Shadow views record finer GPU spans (`first_cull_and_raster`, `hzb_after_first_pass`,
  `second_cull_and_raster`, `resolve_depth`, `hzb_final`) for `RenderDiagnosticsPlugin`.

## Why it matters

On an integrated GPU with 12k high-poly trees, shadow cascades were ~80 % of the frame, and a large
share of that was fixed per-view work (depth pyramid builds), not geometry.
