---
name: bevy-meshlets
description: Bevy's meshlet / virtual-geometry renderer (bevy_pbr::meshlet) as implemented on main — asset format, BVH8 cluster hierarchy, LOD, GPU culling, two-phase occlusion, software+hardware raster, limits and what is missing (page streaming, alpha-test, mesh shaders). Use before touching or relying on meshlets.
---

# Bevy meshlets (`crates/bevy_pbr/src/meshlet/`, experimental)

## Purpose
Know exactly what exists so you extend it instead of duplicating it, and know when it is the *wrong tool* (e.g. alpha-tested foliage).

## When to use
High-poly opaque static/rigid geometry (rocks, buildings, trunks/branches, scanned assets, terrain props). Not for leaves-as-alpha-cards, grass, transparent or skinned/animated geometry.

## Pipeline (mesh → pixels)
```
Mesh ──MeshletMesh::from_mesh (feature `meshlet_processor`: meshopt + metis, SLOW, offline)──▶ .meshlet_mesh asset
   (build_meshlets → group ~8 meshlets (TARGET_MESHLETS_PER_GROUP) by shared verts via metis → lock group borders
    → simplify group to ~50% tris (meshopt simplify_with_attributes_and_locks) → re-meshletize → repeat;
    reject groups keeping >60% tris → BvhBuilder builds a BVH8)                    from_mesh.rs
Load: MeshletMeshLoader (lz4 frame-compressed)  asset.rs  — version MESHLET_MESH_ASSET_VERSION = 3
Upload: MeshletMeshManager::queue_upload_if_needed → PersistentGpuBuffer ×7 (range allocator; whole mesh at a time)
Extract: extract_meshlet_mesh_entities → InstanceManager (per-instance transform, material id)
GPU per view (visibility_buffer_raster_node.rs):
  clear_visibility_buffer → [early pass] cull_instances → cull_bvh (BVH8, one dispatch per depth level, 8 threads/node)
  → cull_clusters (frustum + HZB occlusion + LOD error test) → raster:
       software raster compute (small triangles, u64 atomics: depth<<32 | ids)  +  hardware raster (large triangles, draw indirect)
  → build depth pyramid (Hi‑Z) → [late pass] re-test what was occluded last frame against this frame's HZB → raster
  → resolve_render_targets (depth / material depth) → material shade passes (prepass, deferred gbuffer, main opaque)
```
Shaders (`*.wesl`): `cull_instances`, `cull_bvh`, `cull_clusters`, `cull_shared` (`lod_error_is_imperceptible`, `aabb_in_frustum`, `should_occlusion_cull_aabb`), `visibility_buffer_{software,hardware}_raster`, `visibility_buffer_resolve`, `meshlet_mesh_material`.

## Data structures (`asset.rs`) — all `Pod`, `#[repr(C)]`
- `MeshletMesh { vertex_positions: Arc<[u32]> (bitstream-quantized), vertex_normals (octahedral 2×16snorm), vertex_uvs: Arc<[Vec2]> (uncompressed), indices: Arc<[u8]>, bvh: Arc<[BvhNode]>, meshlets, meshlet_cull_data, aabb, bvh_depth }`
- `Meshlet`: bit offset into packed positions, per-channel bit widths + min + power-of-2 quantization factor (default factor 4 → 1/16 cm: `MESHLET_DEFAULT_VERTEX_POSITION_QUANTIZATION_FACTOR`), `vertex_count_minus_one: u8`, `triangle_count: u8`.
- `BvhNode`: 8 children — `aabbs: [MeshletAabbErrorOffset; 8]` (AABB + parent error + child offset), `lod_bounds: [MeshletBoundingSphere; 8]`, `child_counts: [u8; 8]` (`u8::MAX` ⇒ child is another BVH node, else a group of N meshlets).
- LOD test: simplification error projected to screen vs 1 pixel (`lod_error_is_imperceptible`, derived from meshoptimizer `demo/nanite.cpp`). **LOD is per-group, crack-free via locked borders, selected on the GPU every frame — no CPU LOD logic.**

## Requirements / limits (from `mod.rs`, `asset.rs` docs)
- GPU features `MeshletPlugin::required_wgpu_features()`: `TEXTURE_INT64_ATOMIC | TEXTURE_ATOMIC | SHADER_INT64 | SUBGROUP | DEPTH_CLIP_CONTROL | IMMEDIATES`. Vulkan/Metal only. Missing features ⇒ `check_meshlet_features` **`process::exit(1)`** at startup — always probe `RenderDevice::features()` yourself first in tests/examples. **Measured:** the AMD Vega iGPU (RADV RENOIR, Mesa 26.2) in this repo's dev machine exposes all six features and runs the meshlet path: 23 k-tri opaque trees ×11 952 (280 M tris) took 134.6 ms on the standard path vs 29.5 ms with meshlets (5.5 ms vs 104 ms without shadows) — `docs/open_world/benchmarks.md` §1b. Shadow cascades were 75 % of the meshlet frame. Always probe features (`RUST_LOG=bevy_render=debug` prints the device features) instead of assuming.
- `Msaa::Off` required; **opaque materials only** (no alpha mask/blend/transmission); materials can't control vertex shader/attributes; no derivative functions in material shaders; use `Material::meshlet_mesh_fragment_shader` variants; forward+prepass **or** deferred, not mixed.
- `MeshletPlugin { cluster_buffer_slots }` is a fixed pre-allocated queue (4 B/slot, ≤ 2^25); too small ⇒ missing/blinking meshes. Size for worst-case visible clusters.
- Asset granularity: **whole mesh resident or not.** `MeshletMeshManager::remove` frees ranges when the asset unloads; there is **no per-page/per-LOD-level streaming** and no residency feedback from the GPU.
- Large VRAM-resident base overhead; slower than the standard path for small scenes (docs say so).
- No skinning/morph, no vertex animation (⇒ no wind), no instancing API beyond many `MeshletMesh3d` entities (each is an entity → `InstanceManager` per frame).
- Mesh shaders are **not** used by meshlets; they exist only as a general pipeline type (see below).

## Added in this repo (measured, `docs/performance.md`)
* `MeshletBuildConfig { max_vertices, max_triangles }` + `MeshletMesh::from_mesh_with_config` (default 256/128 = `from_mesh`); `MeshletMesh::stats() -> MeshletMeshStats` (meshlets, tris, BVH nodes/depth, per-array bytes, `lz4_file_bytes`). Tests in `meshlet::from_mesh::tests` (need `AsyncComputeTaskPool::get_or_init`).
* **Cluster size**: 32/64/96/128 tris gave 58.4/38.2/32.2/28.7 ms (3 shadow cascades) but the same ~5.5 ms main view → keep 128 (also the software rasterizer's limit: 128 threads × 1 tri). Shadow views each run a full cull+raster, so per-cluster overhead is multiplied by cascade count.
* **Sizes**: ~34.5 raw / ~21.8 lz4 bytes per source triangle (UE docs: 14.4 on disk). UVs are uncompressed 8 B/vertex.
* **Bakes are deterministic now** (was: identical bakes differed under concurrency) — METIS has global RNG state, so partition calls are serialized by `METIS_LOCK` in `from_mesh.rs`; keep any new METIS call behind `metis_guard()`. Test: `bakes_are_bit_identical` (failed 6/6 under concurrent baking before the fix; run `cargo test -p bevy_pbr --features meshlet_processor --lib meshlet` a few times).
* **Queue overflow is detected and degraded** (`meshlet/pressure.rs`): shaders still `atomicAdd` without bounds checks, but `PressureProbe` copies counters (first-pass BVH level counts + early+late cluster pair) into a probe buffer, reads them back asynchronously, and `MeshletPressure` (render world) raises the LOD error scale (×1.25 overflow / ×1.05 >90 % / ×0.98 after 120 observations <60 %, cap 16). Verify with `forest_benchmark --meshlet --cluster-slots 32768` and `RUST_LOG=bevy_pbr::meshlet=debug`. Sizing: the 12 k-tree forest peaks at ~68 k entries. Limits: first-pass only, first camera only, scale also coarsens the near view. Disable with `MeshletSettings { detect_queue_pressure: false, .. }` / `degrade_on_queue_pressure`.
* **Unsupported GPU is non-fatal**: `MeshletPlugin` logs an error, inserts `MeshletSupport { supported: false }` and gates every meshlet system with `meshlet_supported` (also the prepass plugin's meshlet system — any new meshlet-dependent system must use the same run condition). Msaa misconfiguration still exits.
* **`MeshletSettings`** (main-world resource, extracted): `shadow_lod_error_scale` / `shadow_cascade_lod_error_scale_step` (default 1.0; **they cost shadows**: aerial view retained shadowed ground 97 % @1.25, 84 % @1.5, 41 % @2, 13 % @4 for −7/−16/−25 % frame — always check an aerial view, a near view hides the loss; uniform beats per-cascade step) and `rebuild_shadow_depth_pyramid_after_second_pass` (default false; −15.8 % measured; old behavior = true).
* **Retained instance table** (`MeshletSettings::retained_instances`, opt-in; `meshlet/{instance_manager,retained}.rs`): stable dense slots (swap-remove), change-driven extraction (`Changed/Added` + `RemovedComponents`; any `AssetEvent<MeshletMesh>` ⇒ full resync), `RetainedBuffer<T>` with bitset dirty tracking (**encase's `Vec<u8>` scratch never truncates: always slice to `len*stride`**), incremental material ids/refcounts (not tied to `ViewVisibility`), visibility masks shared per (view layers, shadow) signature. Validate with `validate_retained_instances` (+ a mutation test when changing filters: `Changed<GlobalTransform>` alone is redundant with `Changed<PreviousGlobalTransform>`). Measured: −28…−65 % frame time static, ≈−56 % with 1–10 % moving, **+30 % at 100 % moving** (use rebuild mode there). Hot-loop lesson: don't take `&mut ResMut` per instance (marks changed) and keep the asset-cache lookup `#[inline(always)]`.
* **CPU path at scale**: meshlet scenes are CPU-bound from ≈12 k instances (≈0.115 ms/1 k): at 100 k, extract 7.0 ms (now 5.8 after an asset cache + no per-instance tuple clone), prepare 4.5, queue 1.3, material prepare 1.2. `NoCpuCulling` does nothing for meshlets. Don't try an `is_changed` static fast path: `RenderMaterialInstances` ticks every frame and registers materials only for `ViewVisibility`-true entities. Profile with `forest_benchmark --features trace` + `tools/trace_summary.py`.
* **Shadow-view fixed costs**: each cascade does two depth-pyramid builds + a depth resolve (~3.7 ms at 2048² on Vega) regardless of geometry; pyramid cost scales with area and the builder processes power-of-two inputs at 2× size (`(size+1).next_power_of_two()`). New spans per shadow view: `first_cull_and_raster`, `hzb_after_first_pass`, `second_cull_and_raster`, `resolve_depth`, `hzb_final`.

## Mesh shaders (separate, WIP)
`MeshPipelineDescriptor`/`TaskState`/`MeshState` in `crates/bevy_material/src/descriptor.rs`; `TrackedRenderPass::{draw_mesh_tasks, draw_mesh_tasks_indirect, multi_draw_mesh_tasks_indirect(_count)}` in `crates/bevy_render/src/render_phase/draw_state.rs`; the only consumer is `examples/shader_advanced/mesh_shader_intro.rs` (hard-coded cubes, intentionally unbounded). Needs `WgpuFeatures::EXPERIMENTAL_MESH_SHADER | PASSTHROUGH_SHADERS` (wgpu-level *experimental* flag; the example sets them in `WgpuSettings`, lines ~67). No PBR/material/meshlet integration.

## Project conventions
- Treat `bevy_pbr::meshlet` as upstream code: minimal, additive patches; keep `MESHLET_MESH_ASSET_VERSION` bumped on any format change and keep the loader backward-incompatible failure explicit.
- Bake offline in an asset processor / build script, never at runtime (`bevy-assets`, `asset-pipelines`).
- For foliage: meshlets for trunks/branches/rocks/buildings; **alpha-tested leaves use the standard instanced path** with visibility ranges + impostors (`vegetation-rendering`).

## Common mistakes
- Using meshlets for anything with `AlphaMode::Mask`.
- Enabling the plugin on unsupported GPU (process exit).
- Undersized `cluster_buffer_slots`.
- Believing "virtual geometry" here means streaming — it doesn't (see audit).

## Performance considerations
Cost scales with visible clusters + raster; profile per-pass with the `meshlet_*` GPU spans (`RecordDiagnostics`). Occlusion benefits need large occluders. Instance count (entities) still costs CPU in extract/queue — many small meshlet instances may lose to GPU-instanced standard meshes.

## Testing requirements
Gate GPU tests on `required_wgpu_features()`. Assets: round-trip `MeshletMeshSaver`→`Loader` test, version-mismatch test. Examples: `examples/3d/meshlet.rs`, `examples/stress_tests/many_meshlet_materials.rs` (features `meshlet`, `meshlet_processor`).

## References
`crates/bevy_pbr/src/meshlet/*`, PR history: `git log --oneline -- crates/bevy_pbr/src/meshlet | head -30`
