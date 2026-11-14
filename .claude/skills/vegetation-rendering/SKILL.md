---
name: vegetation-rendering
description: Rendering dense forests and vegetation in Bevy — separating geometry/fragment/overdraw/shadow/material/texture/animation costs, LOD/HLOD/impostors, alpha-tested foliage, instancing, wind, shadows, and which Bevy paths fit (standard instanced vs meshlets). Use for any tree/grass/foliage performance work.
---

# Vegetation rendering

## Purpose
Make forests cheap by attacking the *actual* bottleneck. **Never assume triangle count is the only (or main) cost.**

## Cost taxonomy — identify which one before optimizing
| Cost | Symptom | Diagnose | Typical fix |
|---|---|---|---|
| Geometry (vertex/primitive) | GPU time scales with triangle count, resolution-independent | halve resolution: no change ⇒ geometry | LOD, HLOD, meshlets (opaque trunks/branches), culling |
| Fragment shading | time scales with resolution | halve resolution: big change ⇒ fragment | cheaper material, depth prepass, impostors |
| **Overdraw** (leaves) | many layered alpha-tested leaf cards | overdraw visualization (`bevy_dev_tools::render_debug`, RenderDoc) | fewer/larger leaf cards, tighter card hulls, depth prepass, impostor at distance |
| Shadow cost | frame time jumps with shadows on | toggle shadows | shadow LOD (cheaper proxy casters), fewer cascades, distance-limit casters, `NotShadowCaster` on far HLOD |
| Material cost | many materials ⇒ many bins/draws | draw/bin count | share materials, texture arrays/bindless (`material_bind_groups.rs`) |
| Texture bandwidth | high-res albedo+normal per species, many species | texture memory/bandwidth counters | BC7/BC5 + mips (`asset-pipelines`), smaller atlas at distance |
| Animation (wind) | CPU/vertex ALU cost | disable wind | vertex-shader wind (cheap, no skinning), fade wind with LOD |

## Bevy path selection (verified limits)
- **Meshlets** (`bevy-meshlets`): opaque-only, no vertex animation. ⇒ use for trunks, branches, rocks, *modeled polygon leaves* if opaque (no alpha test). **Not** for alpha-tested leaf cards, grass, or wind-animated vertices.
- **Standard path** (GPU-preprocessed instancing, indirect draws, GPU frustum + optional occlusion culling): alpha-tested foliage (`AlphaMode::Mask`), vertex-animated materials, billboards/impostors. Identical meshes+materials batch into few indirect draws. Enable `OcclusionCulling` + `DepthPrepass` for forests seen at ground level (measure).
- **`VisibilityRange`** (`bevy-visibility`): discrete LOD/HLOD with dither crossfade. CPU cost per entity ⇒ apply to *aggregates/cell proxies*, not 100 k individual trees.
- **Instances are not entities** (target design): static trees live in per-cell instance tables → GPU instance buffer. Today Bevy's standard path still needs an entity per `Mesh3d` instance; the project layer caps entity count by (a) HLOD aggregation at distance, (b) activating individual instances only within the active ring, (c) retained render-world instance data as future work (see audit).

## Representation chain for a tree (screen-space-error driven; `bevy_world_streaming::lod`)
```
Full         high-poly trunk/branches (meshlets or LOD0) + leaf cards/meshes         (< ~30 m)
Medium       LOD1–2 mesh, reduced leaf count, larger cards                            (30–120 m)
Proxy        very low poly trunk + canopy blob(s)                                      (120–400 m)
Impostor     camera-facing / octahedral billboard atlas (N views), alpha-tested       (400 m – 1.5 km)
Forest HLOD  one merged canopy hull / coarse mesh for a whole forest region           (> 1 km, replaces 100s–1000s of trees)
```
Switch by projected error `err_px = world_error · focal_px / distance` against thresholds with hysteresis (in ≠ out). Never pop: crossfade with dither (`VisibilityRange` margins) or switch only when fully resident.

## Forest HLOD (aggregate, don't draw 10 000 distant trees)
`10 000 trees → forest region (grid ≈ 100–250 m) → merged canopy hull per region (one draw) → individual trees when region error exceeds threshold`. `bevy_world_streaming::hlod::build_forest_hlod` groups instances into regions and emits one merged low-poly mesh per region (ellipsoid canopies + trunk prisms merged into one `Mesh`), plus per-region bounds and a `draw_calls_saved` count. Shadows for far regions: none or a single cheap caster.

## Measured guidance (12 k trees, ~7.9 k tris/LOD0 tree, 720p, Vega iGPU; details `docs/open_world/benchmarks.md`)
1. Mesh LOD first: 6.3× (78 → 12.4 ms). 2. Forest HLOD: 3× alone, ≈ +2 % over LOD on GPU time but cuts visible entities ~7× (CPU/entity scaling + lets far instances unload). 3. Occlusion culling: a **loss** (0.75×) on its own for leaf-card forests, a 17 % win after LOD/HLOD. 4. Shadows are the next wall: 22 ms of 78 ms baseline, 5.7 ms of 10.1 ms optimised (and 75 % of the meshlet frame for opaque wood). 5. Opaque high-poly wood through meshlets: 4.6× with shadows / 19× without vs the standard path.

## Project conventions
- Every vegetation benchmark reports the cost split above (not just FPS): GPU ms by pass (depth/shadow/main), visible instances, triangles drawn, draw calls, overdraw estimate.
- Species assets declare `alpha_tested: bool`, `leaf_card_count`, `impostor_atlas`, `shadow_policy`. The renderer picks the path from those flags.
- Wind: one shared uniform (time, direction, strength) + per-vertex mask (trunk stiff, tips loose); LOD ≥ Proxy has wind disabled or baked into impostor frames.

## Common mistakes
- Putting `AlphaMode::Mask` leaves on the meshlet path.
- LOD thresholds by distance only (ignores asset size and FOV) — use projected error.
- Rendering shadows for every tree at every cascade.
- Per-tree `VisibilityRange` entities at 10⁵ scale.
- Impostors with mismatched lighting (bake normals/AO; shade in-engine).

## Performance considerations
Overdraw from leaves frequently dominates once triangles are controlled: prefer fewer, larger cards with tight alpha hulls; early-Z via `DepthPrepass` helps opaque trunks but alpha-tested depth prepass still shades leaves — measure.

## Testing requirements
Benchmark scene `examples/large_scenes/forest_benchmark` (`cargo run --release -p forest_benchmark -- --lod --hlod --occlusion`; 10 000+ trees, multiple species, ground-level and aerial camera paths, occluded + visible mixes): run each optimization independently (baseline → +visibility ranges → +HLOD → +occlusion → +shadow LOD) and record the table in `docs/open_world/benchmarks.md`. Visual check: no popping greater than crossfade margin.

## References
`crates/bevy_world_streaming/src/{lod.rs,hlod.rs}`, `crates/bevy_camera/src/visibility/range.rs`, `examples/3d/{visibility_range,occlusion_culling}.rs`, `crates/bevy_pbr/src/meshlet/mod.rs` (limits)
