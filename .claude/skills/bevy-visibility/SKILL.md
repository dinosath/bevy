---
name: bevy-visibility
description: Visibility in Bevy main — CPU frustum culling, VisibilityRange (HLOD), GPU culling, two-phase GPU occlusion culling with Hi-Z, meshlet culling, and when each applies at open-world scale. Use when thousands of objects must be culled cheaply.
---

# Visibility, culling, LOD/HLOD in Bevy

## Purpose
Pick the cheapest visibility mechanism that scales, and avoid per-object CPU work.

## Mechanisms (all exist on main)
| Mechanism | Where | Granularity | Runs on | Notes |
|---|---|---|---|---|
| Hierarchy visibility (`Visibility`, `InheritedVisibility`, `ViewVisibility`) | `crates/bevy_camera/src/visibility/mod.rs` | entity | CPU, PostUpdate | `VisibilitySystems` sets; `VisibilityClass` must be non-empty to be considered |
| CPU frustum culling `check_visibility_cpu_culling` | same | entity AABB | CPU (parallel) | `NoFrustumCulling` opt-out; `NoCpuCulling` ⇒ rely on GPU (`check_visibility_gpu_culling` only tracks inherited visibility) |
| **`VisibilityRange`** (a.k.a. HLOD) | `crates/bevy_camera/src/visibility/range.rs`, `crates/bevy_render/src/view/visibility/range.rs`, `VisibilityRangePlugin` | entity | CPU distance check per view (`check_visibility_ranges`, `VisibleEntityRanges`) + dithered crossfade in shader (`pbr_functions.wesl`) | each LOD level is a separate entity; `start_margin`/`end_margin` ranges; shadows use `ShadowLodOrigin`. Example: `examples/3d/visibility_range.rs` |
| GPU frustum culling + indirect draw | `crates/bevy_render/src/batching/gpu_preprocessing.rs`, `bevy_pbr/src/render/{mesh_preprocess,build_indirect_params}.wesl` | mesh instance | GPU compute | default (`GpuPreprocessingMode::Culling`); `NoIndirectDrawing` disables |
| **GPU occlusion culling** (`OcclusionCulling` view component, `OcclusionCullingPlugin`) | `crates/bevy_render/src/occlusion_culling/mod.rs`, `bevy_pbr/src/render/occlusion_culling.wesl`, `bevy_core_pipeline/src/mip_generation/experimental/depth.rs` | mesh instance AABB | GPU, two-phase (early depth prepass of last-frame-visible → depth pyramid → test all → late prepass) | needs `DepthPrepass`; incompatible with deferred; HZB builder is under `mip_generation::experimental`. Example `examples/3d/occlusion_culling.rs` |
| Meshlet instance/BVH/cluster culling + occlusion | `bevy_pbr/src/meshlet/cull_*.wesl` | meshlet group / cluster | GPU | see `bevy-meshlets` |

## When to use which (decision rules)
- **Object visibility** (CPU `ViewVisibility`): ≤ ~10⁴ dynamic entities (vehicles, NPCs, interactive props).
- **GPU culling (default) + `NoCpuCulling`**: 10⁴–10⁶ static instances of the standard mesh path; skip CPU frustum test entirely.
- **`VisibilityRange`**: *discrete* LOD/HLOD swaps and impostors for entities in the standard path. CPU cost is O(entities with range) per view — fine for thousands, **not** for hundreds of thousands: aggregate first (cell/forest HLOD), then apply ranges to the aggregates.
- **Occlusion culling (GPU, HZB)**: dense scenes with large occluders (cities, canyons, forests seen from road level). Measure — it has overhead (docs say rarely a slowdown); cheap when large depth coverage is available.
- **Meshlet/cluster visibility**: high-poly opaque geometry where partial-object culling and continuous LOD matter.
- **Cell visibility** (project layer, `bevy-world-streaming`): whole streamed cells decided on CPU from the **streaming state + coarse frustum/range**, at O(cells) ≈ tens–hundreds per frame, *before* any per-object work exists. Cells that aren't resident show their HLOD proxy.
- **HLOD visibility**: the representation per cell/region: `Full` → `Proxy` → `Impostor` by screen-space error (`lod::LodSelector` + `projected_size_px` in `bevy_world_streaming`).

## Measured (forest benchmark, 12 k alpha-tested trees, Vega iGPU — `docs/open_world/benchmarks.md`)
- `VisibilityRange` mesh LOD chain: 78.1 → 12.4 ms (6.3×). Merged forest HLOD alone: 3×; on top of LOD only +2 % GPU time but visible entities 6 767 → 943.
- **`OcclusionCulling` alone: 78.1 → 103.7 ms (slower)** — alpha-tested leaves don't occlude and the depth prepass doubles geometry work. After LOD+HLOD: 12.17 → 10.13 ms (−17 %). Enable only after measuring.

## Rules for scale
- Never loop over every instance on the CPU per frame. If an algorithm is O(instances), it must run on the GPU or be moved to O(cells)/O(changed).
- Hysteresis on every discrete switch (margins in `VisibilityRange`, `hysteresis` in cell streaming) to prevent oscillation.
- Crossfades only need both LODs present for the margin distance; budget the doubled draw cost.
- Shadows: cascades are views — see `vegetation-rendering` for shadow cost control (`ShadowLodOrigin`, `NotShadowCaster` on distant HLOD).

## Common mistakes
- Putting `VisibilityRange` on 100 k tree entities and wondering why `PostUpdate` takes ms.
- Enabling occlusion culling without `DepthPrepass`, or with deferred.
- Mixing LOD levels with different pivots/AABBs while using crossfade (docs: positions must match; `use_aabb`).
- Relying on CPU frustum culling for alpha-tested foliage far fields (overdraw, not triangles, is the cost).

## Performance considerations
Check which of CPU `PostUpdate` visibility, GPU preprocess compute, depth prepass, or raster dominates before choosing (`performance-profiling`). Occlusion test cost ∝ instances; benefit ∝ occluded fragment/vertex work.

## Testing requirements
Deterministic camera path + counters: visible/culled/occluded counts (example reads back `IndirectParametersBuffers` in `examples/3d/occlusion_culling.rs`). Test that LOD switch distances respect hysteresis (project crate has pure-function tests).

## References
`crates/bevy_camera/src/visibility/`, `crates/bevy_render/src/{view/visibility,occlusion_culling}`, `examples/3d/{visibility_range,occlusion_culling}.rs`, audit `docs/open_world/bevy_audit.md`
