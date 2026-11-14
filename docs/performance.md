# Performance (measured)

Tags: **[Bench]** = measured here. Environment: AMD Radeon Vega iGPU (RADV RENOIR, Mesa 26.2.6), 1280×720, `AutoNoVsync`, release builds (`rustc 1.101-nightly`; stable 1.100 since installed — tests re-verified, benchmarks not re-run), 600 measured frames after 120 warm-up, one run per row unless noted. P90 was not recorded by the harness; P50/P95/P99/max were. Earlier forest/streaming results (A/B/occlusion/HLOD matrix, streaming simulator) are in `docs/open_world/benchmarks.md`.

## The A/B/C/D comparison asked for (Part 41) — what exists and what does not

| Config | Meaning here | Status | Frame ms mean (P50 / P95 / P99) |
|---|---|---|---|
| **A** current Bevy renderer | standard path, GPU preprocessing + CPU frustum, 11 952 trees | measured | forest leaf-card trees: **78.08** (77.81 / 85.95 / 87.61); opaque 23 k-tri wood: **134.58** (133.70 / 152.18 / 154.89) |
| **B** Bevy meshlet renderer | `MeshletPlugin`, opaque wood only (280 M tris) | measured | **28.70** at the best cluster size before this work (28.04 / 32.07 / 32.67); **24.98** (24.33 / 28.50 / 38.88) with the new default, **18.78** with `shadow_lod_error_scale = 2` (§7) |
| **C** "improved GPU-driven" | what is achievable on the *standard* path with existing features: mesh LOD + HLOD + occlusion | measured | **10.13** (10.00 / 12.41 / 12.99) |
| **D** virtualized geometry | page-streamed, assembly/voxel foliage, VSM | **not built** | — (no number is claimed) |

C and B are not comparable scenes (leaf cards vs opaque wood); the point is that on this GPU **the standard path with LOD already reaches ~10 ms for a 95 M-triangle forest**, so any "D" must beat that, and the reason to build D is *source-asset scale and memory*, not today's frame time on this scene.

## 1. Cluster size experiment (Part 6) — **[Bench]**

`forest_benchmark --meshlet --meshlet-tris N` on 11 952 opaque trees (3 species, 23 424 source triangles each, 280 M triangles in scene), drive path 90 km/h. Cluster size is now configurable through the new `MeshletBuildConfig` (`MeshletMesh::from_mesh_with_config`). `max_vertices = 256` in all rows.

| max tris / meshlet | meshlets (1 species, all LODs) | BVH depth | raw bytes / source tri | lz4 bytes / source tri | frame **no shadows** ms | frame **3 cascades** ms (P50 / P95 / P99 / max) |
|---:|---:|---:|---:|---:|---:|---|
| 32 | 1 660 | 5 | 43.3 | 28.0 | 5.66 | 58.37 (58.21 / 61.78 / 63.06 / 102.09) |
| 64 | 795 | 4 | 37.4 | 24.2 | 5.47 | 38.23 (37.60 / 41.78 / 42.51 / 81.21) |
| 96 | 532 | 4 | 34.6 | 21.1 | 5.51 | 32.22 (31.46 / 35.67 / 36.16 / 48.83) |
| **128** (Bevy default) | 394 | 4 | 34.5 | 21.8 | 5.44 | **28.70** (28.04 / 32.07 / 32.67 / 42.14) |

**Findings.** (1) Rendering the *main view* is insensitive to cluster size (5.4–5.7 ms); (2) **shadow cost is dominated by per-cluster overhead**: 32-tri clusters double the 3-cascade frame (58 vs 29 ms) because every cascade re-culls/re-rasters 4× as many clusters; (3) bigger is better up to the format's limit — `Meshlet::triangle_count` is a `u8` and the software rasterizer runs `@workgroup_size(128)` with **one triangle per thread** (`visibility_buffer_software_raster.wesl`), so >128 would need a shader change; 128 is therefore both Bevy's default *and* the optimum measured, so the existing default stands (no change proposed). 96 was marginally smaller on disk (21.1 vs 21.8 lz4 B/tri; noise-level). (4) The 128-tri P99/max are the tightest. Caveat: these frames are shadow-bound, so the "main view" insensitivity should be re-checked at higher resolution; meshopt `max_vertices` and cone weight were not varied.

New API used: `MeshletMesh::stats()` → `MeshletMeshStats { meshlets, triangles, meshlet_vertices, bvh_nodes, bvh_depth, *_bytes, lz4_file_bytes }`. Unit-tested in `bevy_pbr` (`meshlet::from_mesh::tests`).

## 2. Geometry size vs UE (Part 16) — **[Bench]** vs **[UE-doc]**

| | Bytes per *source* triangle |
|---|---|
| UE Nanite, Valley of the Ancients average | **14.4** on disk **[UE-doc]** |
| Bevy meshlet asset, lz4 on disk (128-tri clusters) | **21.6–22.0** (this wood test mesh; includes all LOD levels: 46 674 meshlet triangles for 23 424 source) |
| Bevy meshlet asset, raw / on GPU | **34.3–34.5** |

Not apples to apples (different meshes; UE's figure is an average over a real scene), but it indicates Bevy's meshlet format is ~1.5× UE's disk size and uses 8 B/vertex of uncompressed UVs. Per-vertex layout (from `asset.rs`): positions bit-packed per meshlet, normals 4 B/vertex (octahedral 2×16 snorm), **UVs 8 B/vertex (uncompressed `Vec2`)**, triangle indices 1 B/index; `MeshletMesh::stats()` gives the exact breakdown per asset — UVs are the obvious first compression target (see `geometry_streaming.md`). UE also trades position precision automatically ("Auto" quantization [UE-doc]); Bevy's per-asset factor defaults to 1/16 cm.

## 3. Shadow cost (motivates Part 26) — **[Bench]**

Same wood scene, 128-tri clusters.

| Shadow setup | frame ms | shadow cost vs no-shadows (5.44 ms) |
|---|---:|---:|
| none | 5.44 | 0 |
| 1 cascade, 300 m | 16.68 | 11.2 |
| 2 cascades, 300 m | 22.49 | 17.1 |
| 3 cascades, 100 m | 22.52 | 17.1 |
| 3 cascades, 150 m | 23.95 | 18.5 |
| 3 cascades, 300 m | 28.70 | 23.3 |

Per-cascade spans (earlier run): 4.8 + 5.9 + 11.5 ms = 22.2 ms; **the farthest cascade is the most expensive**. Shadow rendering is ~80 % of the meshlet frame. It is a per-view re-cull and re-raster with **no caching**: each cascade re-traverses the full cluster hierarchy every frame ([Bevy] `visibility_buffer_raster_node.rs` loops over `view_light_query` and runs `first_cull` + raster per shadow view). A VSM-style design (page demand + caching of static pages) targets exactly this cost; the achievable saving is **not measured** (nothing implemented).

## 4. Culling/LOD costs are small relative to shadows — **[Bench]**

No-shadow meshlet frame = 5.4 ms total, of which `meshlet_visibility_buffer_raster` (instance + BVH + cluster cull + raster + HZB) ≈ 3.0–3.5 ms and material resolve ≈ 0.4 ms. Adaptive hierarchy fan-out (UE's `AdaptiveFanoutThreadBudget` [UE-doc]) would target the BVH-level dispatch overhead, which is a fraction of that 3 ms. **Decision (per Part 10): not justified by the numbers; do not implement now.** Revisit at ≥10× more instances.

## 5. Meshlet bakes were not deterministic — root-caused and fixed — **[Bench]**

Observed: two bakes of the same mesh differed by a few LOD triangles. Isolated: the new test `bakes_are_bit_identical` passes when run alone (1 or 8 pool threads) but **failed 6/6 when other bakes ran concurrently** in the same process. Cause: METIS keeps global state (its RNG is re-seeded per call but shared between threads), and `compute_meshlets` calls it from parallel simplification tasks (and an asset processor bakes several meshes at once). Fix: a `METIS_LOCK` mutex scoped to only the METIS partition call; meshopt and meshlet building stay parallel. After the fix the same test passes **6/6 under concurrency**. Bakes are now bit-identical (positions, indices, meshlet descriptors, stats). *Cost of the lock not separately measured; the 3-mesh bake in the forest benchmark still takes a few hundred ms.*

## 7. Where meshlet shadow time actually goes, and what was done about it — **[Bench]**

New nested GPU spans (`first_cull_and_raster`, `hzb_after_first_pass`, `second_cull_and_raster`, `resolve_depth`, `hzb_final`) per shadow view, 2048² cascades, 280 M-triangle wood scene:

| Cascade | total | first cull+raster | HZB after first pass | HZB final | resolve depth | second pass |
|---|---:|---:|---:|---:|---:|---:|
| 0 (near) | 4.78 | 0.54 | 1.47 | 1.50 | 0.75 | 0.05 |
| 1 | 5.95 | 1.75 | 1.43 | 1.41 | 0.74 | 0.05 |
| 2 (far) | 11.57 | 7.48 | 1.40 | 1.40 | 0.78 | 0.05 |

**~11 of the 22 ms is fixed per-view work independent of geometry** (two depth-pyramid builds + depth resolve per cascade); only the far cascade's cull+raster is geometry-bound. Shadow-map-size sweep (frame ms): 512 → 9.15, 1024 → 10.44, 2048 → 24.57, 4096 → 60.27; one pyramid build costs 0.10 / 0.38 / 1.45 / 5.8 ms — scales with **area**.

Changes made (all in `bevy_pbr::meshlet`, controlled by the new `MeshletSettings` resource):

1. **Skip the second (final) shadow-view depth pyramid build by default** (`rebuild_shadow_depth_pyramid_after_second_pass = false`). Rationale: it only refines *next frame's early-pass* occlusion test; a pyramid with fewer occluders can only cull *less*, never wrongly, so it is conservative.
2. **`shadow_lod_error_scale`** (default `1.0` = unchanged): LOD error budget multiplier for shadow views, plumbed through the two LOD cull shaders via immediates.
3. **Bug fix** found on the way: perspective LOD selection ignored the instance's scale (orthographic applied it) — see migration note.

Interleaved 4-run comparison, current binary (mean frame ms; min–max across runs; P50 / P95 / P99):

| Config | mean | P50 | P95 | P99 | vs old |
|---|---:|---:|---:|---:|---:|
| old behavior (`--rebuild-shadow-hzb`) | 29.68 (29.36–29.86) | 29.04 | 33.16 | 43.65 | — |
| **new default** (skip extra rebuild) | **24.98** (24.79–25.20) | 24.33 | 28.50 | 38.88 | **−15.8 %** |
| new default + `shadow_lod_error_scale = 2` | **18.78** (18.28–19.98) | 18.10 | 22.78 | 33.55 | **−36.7 %** |

(An earlier 6-pair run of change 1 alone gave −11 % on the pre-LOD-plumbing binary; the 4-run figure above is the cleaner measurement.) Earlier sweep: scale 4 ≈ 20.7 ms, i.e. diminishing returns past 2 because the fixed per-view costs remain.

**Correctness/quality checks.** (a) Rendered images: this renderer is not pixel-reproducible on this GPU even in a *static* scene (two identical runs differ in ~15.7 k of 921 k pixels — consistent with the open software-raster flicker issue #26467); A-vs-B differences (15.6–15.8 k px) are indistinguishable from that noise, so no regression from change 1 is detectable; by construction the drawn set stays conservative. (b) Visual: the same frame at scale 1 vs 4 shows near-identical tree shadows (single view, single frame; not a quality study) — hence the default stays at 1.0 and the knob is documented as a quality/speed trade-off. (c) Capability fallback verified on hardware (see below).

**Not done (measured motivation only):** the depth-pyramid builder rounds even power-of-two inputs up to the next power of two (`(size+1).next_power_of_two()` → a 2048² shadow map is processed as 4096², 4× work) to keep full top-level precision. A shader-level variant for power-of-two sources would cut each remaining build ~4× (≈3 ms of the current 25 ms) but changes the conservative-downsample footprint; deliberately **not** attempted without a dedicated correctness test. Documented in `occlusion.md`.

## 8. Capability fallback verified — **[Bench]**
`forest_benchmark --meshlet --disable-int64-atomics` (disables `TEXTURE_INT64_ATOMIC` at device creation): before the change the process exited; now it logs `MeshletPlugin disabled: GPU lacks support for required features…`, keeps rendering (ground only, ~3 ms frames, exit code 0), and the supported path is unaffected. Root cause of a first-attempt panic: the *prepass* plugin registered its own meshlet system gated only on `InstanceManager`, now gated on `meshlet_supported`.

## 9. Reproduce

```bash
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --meshlet-tris 64 --no-shadows --label t64
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --cascades 1 --label c1
cargo test -p bevy_pbr --features meshlet_processor --lib meshlet
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --shadow-lod-scale 2 --top-spans 40
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --rebuild-shadow-hzb   # old behaviour
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --disable-int64-atomics # fallback test
```
Raw logs of this session were kept in the scratchpad (not committed); numbers above are copied from them.
