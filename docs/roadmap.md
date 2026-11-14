# Roadmap (Part 48 phases) — status and honest scope

Status: ✅ done and measured in this change set · 🟡 partly · 📐 design only · ⬜ not started. "Independently buildable & benchmarked" is the acceptance rule per phase.

| Phase | Item | Status | What exists | Next concrete step |
|---|---|---|---|---|
| 0 | UE ↔ Bevy audit | ✅ | `ue_bevy_gpu_feature_matrix.md` + this doc set (UE facts from 5.9 docs; Bevy facts from source/issues) | keep updated |
| 1 | Improve existing meshlet renderer | 🟡 (mostly ✅) | ✅ `MeshletBuildConfig`/`from_mesh_with_config`/`stats()`; ✅ **deterministic bakes** (METIS lock; was failing 6/6 under concurrency, now 6/6 pass); ✅ **non-fatal capability check** (`MeshletSupport`, verified on hardware); ✅ **shadow-view fix: −15.8 % frame (default, no quality cost)**; shadow LOD knobs shipped *off* (they trade far shadows for speed: `performance.md` §11); ✅ **extractor −1.26 ms (−18 %) at 100 k instances**; ✅ fine-grained shadow GPU spans; ✅ perspective-LOD instance-scale bug fix; release note + migration guide | ✅ **overflow detection + graceful degrade** (`MeshletPressure`, GPU counter probe, 5 controller tests, hardware-verified; +0.05 ms) | (a) far-only/per-view degrade policy and visible-cluster counters; (b) HZB builder for power-of-two sources (~4× cheaper, needs a dedicated conservativeness test); (c) UV compression (−~12 % raw) |
| 2 | GPU scene | 🟡 evidence gathered | **measured**: meshlet scenes are CPU-bound from ≈12 k instances (gap ≈0.115 ms/1 k instances; 57 % of frame at 100 k, 65 % at 200 k; GPU only 12.5 ms at **4.68 B LOD0 triangles**); span breakdown at 100 k: extract 7.0, prepare 4.5, queue 1.3, material prepare 1.2 ms. Contained fixes: extractor −18 % done; others no gain. Static-fast-path **rejected** (material registration depends on `ViewVisibility`; change tick bumped every frame) | retained tables with *incremental material registration* (see `gpu_scene.md`) |
| 3 | GPU hierarchical culling | ✅ upstream | exists (meshlet BVH traversal) | cone culling (asset v4) |
| 4 | Screen-space LOD | ✅ upstream + 🟡 knob | exists; **error budget now a per-dispatch immediate**; `MeshletSettings::shadow_lod_error_scale` exposes it for shadow views (default 1.0) | expose a main-view scale + feed it from the overflow/pressure ladder |
| 5 | Hi-Z occlusion | ✅ upstream | exists; measured loss/win cases | cull counters exposed |
| 6 | Compaction + indirect | 🟡 upstream | atomic append; no alternative measured | only if a span shows contention |
| 7 | Cluster streaming | 🟡 | policy layer (`bevy_world_streaming::pages`, scheduler, sim) | page-addressable BVH + feedback buffer (renderer work) |
| 8 | Compression | 🟡 | size baseline vs UE (21.8 vs 14.4 B/tri) | UV/index/tangent compression |
| 9 | Fallback geometry | ⬜ | — | asset pair + capability switch |
| 10 | Virtual shadow pages | 🟡 (S1 partly done) | measured: shadows ≈80 % of meshlet frame, ~half of it fixed per-view cost; S1: extra-HZB removal done (−15.8 %); LOD knobs exist but lose far shadows (uniform 1.5: −15.5 % @ 84 % coverage) | S1 remainder: area-preserving simplification so coarse shadow LODs keep thin casters; far-cascade update rate; power-of-two HZB; then S2 page-demand marking |
| 11 | Assemblies | 📐 | — | BVH `InstanceGroup` node (asset v4) |
| 12 | GPU foliage animation | 📐 | depends on 17 | — |
| 13 | Foliage far representation / voxels | 📐 | merged-region HLOD measured (`bevy_world_streaming::hlod`) | impostor vs voxel experiment |
| 14 | Terrain | ⬜ | — | clustered heightfield + VT (needs 16) |
| 15 | Spline geometry | ⬜ | — | conservative deformed bounds |
| 16 | Virtual textures | 📐 | issue #25986 exists upstream | coordinate, share IO scheduler |
| 17 | Skeletal virtual geometry | ⬜ | meshlets: no skinning | rigid-per-cluster skinning with swept bounds |
| 18 | RT virtual geometry | 📐 | Solari BLAS-per-mesh; wgpu has no CLAS | RT over fallback LODs with BLAS budget |
| 19 | Large-world streaming integration | 🟡 | cells, prediction, budgets, sim (`bevy_world_streaming`) | per-class IO budgets (geometry/texture/shadow) |
| 20 | Full stress testing | 🟡 | forest benchmark (leaf cards + opaque wood; standard/meshlet; LOD/HLOD/occlusion/shadows) | Scenes B–F (city, racing, GTA, extreme, streaming-at-speed) |

## Recommended order of the *next* work (by measured payoff)
1. **Shadows**: S1 remainder (per-cascade scale, far-cascade update rate, power-of-two HZB ≈3 ms), then S2–S4. Fixed per-view cost is now the biggest single item (§7 of `performance.md`).
2. Overflow counter + degrade (correctness/robustness; today silent).
3. Retained GPU scene (only once the 100k-instance benchmark shows CPU-bound).
4. Page streaming in the renderer (needed for source-asset scale, not for this benchmark's frame time).
5. Assemblies → foliage far-representation experiment → skinned meshlets.

## Explicitly not planned until a measurement demands it
Adaptive fan-out traversal, mesh-shader meshlet path, tessellation/displacement, voxel foliage.
