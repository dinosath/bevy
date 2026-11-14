# Bevy `main` audit: what exists for open-world rendering

Audited against this checkout (`0.20.0-dev`, HEAD `41a1282e3`, 2026-11-14) by reading source, not release notes. Paths are relative to the repo root. "Existing" = merged and usable; "WIP" = merged but partial/experimental/example-only; "Missing" = not in the tree.

| Feature | Status | What it is / limits | Relevant source |
|---|---|---|---|
| **Meshlets / clusters** | Existing (**experimental**, feature `meshlet`) | Meshes baked offline into ≤256-vertex meshlets; opaque-only; needs Vulkan/Metal and `TEXTURE_INT64_ATOMIC \| TEXTURE_ATOMIC \| SHADER_INT64 \| SUBGROUP \| DEPTH_CLIP_CONTROL \| IMMEDIATES`; `Msaa::Off`; process-exits if features missing | `crates/bevy_pbr/src/meshlet/{mod,asset,from_mesh}.rs` |
| **Virtual geometry (Nanite-like)** | Existing, partial | Meshlet **BVH8** over simplified cluster groups (~8 meshlets/group, locked borders, ~50 % simplification per level), per-group error → continuous GPU LOD, software + hardware raster into a 64-bit visibility buffer. **No streaming**: whole `MeshletMesh` is GPU-resident or not | `meshlet/{from_mesh.rs,cull_bvh.wesl,cull_clusters.wesl,visibility_buffer_software_raster.wesl}` |
| **LOD (continuous)** | Existing (meshlets only) | Screen-space error test per BVH node/cluster on the GPU | `meshlet/cull_shared.wesl::lod_error_is_imperceptible` |
| **LOD (discrete)** | Existing | `VisibilityRange` — each LOD is a separate entity; dithered crossfade; CPU distance check per entity per view | `crates/bevy_camera/src/visibility/range.rs`, `crates/bevy_render/src/view/visibility/range.rs`, `examples/3d/visibility_range.rs` |
| **HLOD (hierarchy/aggregation)** | **Missing** upstream (the `VisibilityRange` docs call range-swapping "HLOD" but nothing builds or manages aggregate proxies) → **added**: forest-region aggregation | `crates/bevy_world_streaming/src/hlod.rs` |
| **Impostors / billboards** | Missing (only mentioned in `VisibilityRange` docs). *Not implemented here either*: `lod::Representation::Impostor` is a selection slot only | `crates/bevy_camera/src/visibility/range.rs` (doc table) |
| **GPU culling (frustum) + indirect draw** | Existing, default | GPU preprocessing compute builds indirect parameters; `GpuPreprocessingMode::{None, PreprocessingOnly, Culling}`; opt-out `NoIndirectDrawing`; `NoCpuCulling` to skip CPU frustum test | `crates/bevy_render/src/batching/gpu_preprocessing.rs`, `bevy_pbr/src/render/{gpu_preprocess.rs,mesh_preprocess.wesl,build_indirect_params.wesl}` |
| **Occlusion culling** | Existing | Two-phase (early depth prepass of last-frame-visible → HZB → test all → late prepass); view component `OcclusionCulling`; needs `DepthPrepass`; not with deferred | `crates/bevy_render/src/occlusion_culling/mod.rs`, `bevy_pbr/src/render/occlusion_culling.wesl`, `examples/3d/occlusion_culling.rs` |
| **Hi-Z / depth pyramid** | Existing, **experimental** module | SPD-style single-pass depth downsample (also for meshlet 64-bit visibility buffer); "TODO: subgroup support, true single pass" | `crates/bevy_core_pipeline/src/mip_generation/experimental/{depth.rs,downsample_depth.wesl}` |
| **Visibility ranges** | Existing | see LOD (discrete) | `crates/bevy_camera/src/visibility/range.rs` |
| **GPU-driven rendering** | Existing | Standard path (GPU preprocessing/indirect) and meshlet path (fully GPU-driven cull→raster) | as above |
| **Render passes** | Changed on main | Old node graph replaced by per-camera system schedules `Core3d`/`Core2d` under a root `RenderGraph` *schedule* | `crates/bevy_render/src/renderer/mod.rs`, `crates/bevy_core_pipeline/src/schedule.rs` |
| **Mesh shaders** | WIP | `MeshPipelineDescriptor`/`TaskState`/`MeshState`, `draw_mesh_tasks*` incl. multi-indirect-count, one intro example with hard-coded cubes; needs `EXPERIMENTAL_MESH_SHADER`; **no** PBR/material/meshlet integration. RADV on this dev machine does not expose `VK_EXT_mesh_shader` | `crates/bevy_material/src/descriptor.rs`, `crates/bevy_render/src/render_phase/draw_state.rs`, `examples/shader_advanced/mesh_shader_intro.rs` |
| **Geometry compression / quantization** | Existing (two forms) | Meshlets: bit-packed quantized positions, octahedral normals, lz4 asset. Standard meshes: `Mesh::compressed_mesh(MeshCompressionArgs)` (u16 indices, attribute quantization/compression); glTF `KHR_mesh_quantization`, `EXT_meshopt_compression` (feature `gltf_meshopt`); Draco ❌ | `meshlet/asset.rs`, `crates/bevy_mesh/src/mesh.rs` (`MeshCompressionArgs`), `crates/bevy_gltf/src/lib.rs` |
| **Asset preprocessing** | Existing, generic | `asset_processor` feature: `Process`/`LoadTransformAndSave`, `.meta` settings, dependency log. Concrete bakers: meshlets (`MeshletMesh::from_mesh`, slow, offline), compressed images (KTX2/Basis). **No** baker for LOD chains, impostors, forest HLOD, cell packs | `crates/bevy_asset/src/processor/`, `crates/bevy_image/src/compressed_image_saver`, `examples/asset/processing` |
| **Streaming (assets)** | Existing, basic | `AssetServer` async loads on `IoTaskPool`; no priorities, no cancellation, no byte-range/partial reads, no budgets | `crates/bevy_asset/src/server/` |
| **World partition / cells / streaming policy** | **Missing** → **added** | deterministic cells, interest tiers, budgets, hysteresis, eviction, generation-based cancel | `crates/bevy_world_streaming/src/{coords,scheduler}.rs` |
| **Predictive / route-aware prefetch** | Missing → **added** | kinematic prediction + road corridor | `crates/bevy_world_streaming/src/predict.rs` |
| **Floating origin / large coordinates** | Missing → **added** (pure math; the ECS transform shift is **not** wired) | cell+local coordinates, rebase | `crates/bevy_world_streaming/src/coords.rs` |
| **GPU residency** | Partial | `PersistentGpuBuffer` (range allocator, doubling growth, `queue_write`/`mark_slice_unused`) for meshlets; mesh slab allocator for standard meshes; `RenderAssetBytesPerFrame` upload limiter. **No** eviction policy, VRAM budget, compaction, or GPU feedback on what was needed | `meshlet/persistent_buffer.rs`, `crates/bevy_render/src/{slab_allocator.rs,mesh/allocator.rs,render_asset.rs}` |
| **Geometry page residency (sub-asset)** | Missing → policy **added**, renderer wiring **missing** | LOD-ordered pages, prefix residency under budget | `crates/bevy_world_streaming/src/pages.rs` |
| **Texture streaming** | Missing | mips/KTX2 exist, no residency management | — |
| **Terrain** | Missing | only free-camera/atmosphere hits for "terrain" | — |
| **Large-scene examples** | Existing | `bevy_city`, `bistro`, `caldera_hotel`, `zero_day` | `examples/large_scenes/` |
| **Diagnostics** | Existing | frame time, entity count, per-pass CPU+GPU spans (`RenderDiagnosticsPlugin`, Vulkan/DX12 timestamps), mesh allocator & render asset diagnostics, `render_debug` | `crates/bevy_render/src/diagnostic`, `crates/bevy_dev_tools/src` |

## Consequences for the plan

1. **Don't write a new renderer.** GPU preprocessing/culling, occlusion + Hi‑Z, `VisibilityRange`, and meshlets are the foundation.
2. **Meshlets are the wrong tool for alpha-tested foliage** (opaque-only, no vertex animation). They fit trunks/branches/rocks/buildings. Leaves use the standard instanced path.
3. **The real gap is data management**: cells, streaming policy, residency accounting, HLOD aggregation, prediction. That is what `crates/bevy_world_streaming` implements as a pure, tested layer.
4. **Per-page GPU residency inside the meshlet BVH is upstream renderer work** that this change does *not* do (see `architecture.md` → "What is not done").

## Dev-machine capability notes (affects what could be measured)

AMD Radeon Vega iGPU (RADV RENOIR, Mesa 26.2.6, Vulkan 1.4) + llvmpipe. Reports `shaderInt64`, `shaderImageInt64Atomics`, subgroup size 64, timestamps, `multiDrawIndirect`/`drawIndirectCount`; **no** `VK_EXT_mesh_shader`. Meshlet feature probe result and all measurements: `benchmarks.md`.
