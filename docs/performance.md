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
| new default + `shadow_lod_error_scale = 2` | **18.78** (18.28–19.98) | 18.10 | 22.78 | 33.55 | −36.7 % — **but see §11: retains only 41 % of far shadows** |

(An earlier 6-pair run of change 1 alone gave −11 % on the pre-LOD-plumbing binary; the 4-run figure above is the cleaner measurement.) Earlier sweep: scale 4 ≈ 20.7 ms, i.e. diminishing returns past 2 because the fixed per-view costs remain.

**⚠ CORRECTION (see §11): the quality check below only covered a *near* view and the `shadow_lod_error_scale = 2` figure above is not a free win — in an aerial view scale 2 retains only 41 % of shadowed ground.**

**Correctness/quality checks.** (a) Rendered images: this renderer is not pixel-reproducible on this GPU even in a *static* scene (two identical runs differ in ~15.7 k of 921 k pixels — consistent with the open software-raster flicker issue #26467); A-vs-B differences (15.6–15.8 k px) are indistinguishable from that noise, so no regression from change 1 is detectable; by construction the drawn set stays conservative. (b) Visual: the same frame at scale 1 vs 4 shows near-identical tree shadows (single view, single frame; not a quality study) — hence the default stays at 1.0 and the knob is documented as a quality/speed trade-off. (c) Capability fallback verified on hardware (see below).

**Not done (measured motivation only):** the depth-pyramid builder rounds even power-of-two inputs up to the next power of two (`(size+1).next_power_of_two()` → a 2048² shadow map is processed as 4096², 4× work) to keep full top-level precision. A shader-level variant for power-of-two sources would cut each remaining build ~4× (≈3 ms of the current 25 ms) but changes the conservative-downsample footprint; deliberately **not** attempted without a dedicated correctness test. Documented in `occlusion.md`.

## 8. Capability fallback verified — **[Bench]**
`forest_benchmark --meshlet --disable-int64-atomics` (disables `TEXTURE_INT64_ATOMIC` at device creation): before the change the process exited; now it logs `MeshletPlugin disabled: GPU lacks support for required features…`, keeps rendering (ground only, ~3 ms frames, exit code 0), and the supported path is unaffected. Root cause of a first-attempt panic: the *prepass* plugin registered its own meshlet system gated only on `InstanceManager`, now gated on `meshlet_supported`.

## 9. Queue overflow detection and graceful degradation — **[Bench]**

Implemented in `bevy_pbr::meshlet::pressure` (design in `gpu_culling.md`): GPU-side 4-byte copies of the per-BVH-level node counts and the early+late candidate-cluster counters (first pass) into a probe buffer → 3-slot staging ring → asynchronous `map_async` readback (1–2 frames latency, never stalls) → `MeshletPressure::observe(peak)` → `lod_error_scale` fed into the cull shaders for **all** views. Policy: overflow ×1.25; >90 % capacity ×1.05; <60 % for 120 observations ×0.98 (floor 1.0; cap 16). Pure controller unit-tested (5 tests incl. a convergence model).

| Test (11 952 opaque trees, 280 M tris) | Result |
|---|---|
| Normal (4 194 304 slots) | peak **61–68 k entries** (1.6 % of capacity), scale 1.0, 0 overflows. **The benchmark's previous 4 M-slot queue was ~60× oversized**; 96 k slots would have been ample here |
| Forced overflow, 32 768 slots, **degrade on** | detected on frame 1 (peak 61 672 > 32 768); scale 1.25 → 1.95 within 3 events; peak falls to ~11.6 k; later relaxes to 1.70 with peak 22–24 k (68–74 %); **3 overflow events in 400 frames**, 1 warning, mean frame 21.2 ms |
| Forced overflow, **degrade off** | overflow every frame (403 events), geometry dropped (visibly sparser far field in the screenshot comparison), 4 warnings (rate-limited; was 397 before the fix), mean frame 20.9 ms — *faster only because geometry is missing* |
| Probe cost, interleaved 4+4 runs, normal config | **off 24.56 ms vs on 24.61 ms (+0.05 ms, +0.2 %)** |

Quality cost of degrading is real: the scale applies to the main view too, so near-camera branch detail visibly thins (screenshot at scale ≈1.7–1.95) — the trade for never dropping clusters. A smarter policy (apply to far clusters only, or per view class) is a follow-up.
**Limits**: second-pass BVH levels are not sampled (their counters are cumulative, not cleared per level) — the first-pass early+late pair already includes the late entries; BVH-level queue occupancy is sampled only in the first pass; with several cameras only the first raster-node invocation per frame is probed.
**Measurement lesson (again)**: a first non-interleaved run suggested the probe cost +2 ms (27.07 vs 24.98); the interleaved A/B showed +0.05 ms — the difference was machine drift. Only interleaved comparisons are trusted in this document.

## 10. Instance scaling and the CPU path (Phase 2 evidence) — **[Bench]**

`forest_benchmark --meshlet --no-shadows --trees N` (opaque 23 k-tri trees, drive path). Scene size reaches **4.68 billion LOD0 triangles** at 200 k instances.

| Instances | frame ms (mean / P99) | GPU: `meshlet_visibility_buffer_raster` | other GPU spans | non-GPU gap |
|---:|---|---:|---:|---:|
| 12 k | 6.37 / 9.97 | 3.57 | ~0.9 | ~1.9 |
| 50 k | 12.13 / 14.39 | 5.34 | ~1.0 | ~5.8 |
| 100 k | 20.28 / 23.42 | 7.67 | ~1.0 | ~11.6 |
| 200 k | 35.46 / 41.67 | 11.43 | ~1.0 | ~23 |

GPU time grows gently (it tracks *visible* clusters); the frame is **CPU-bound from ≈12 k instances up**, with a gap of ≈0.115 ms per 1 000 instances (57 % of the frame at 100 k, 65 % at 200 k).

Per-frame system cost at **100 k instances** (Bevy `trace_chrome`, `--features trace`): `extract_meshlet_mesh_entities` **7.04 ms** (serial, blocks the main world), `prepare_meshlet_per_frame_resources` **4.55**, `queue_material_meshlet_meshes` 1.27, `prepare_material_meshlet_meshes_main_opaque_pass` 1.19, `check_visibility_cpu_culling` 1.73 — ≈ 14 ms, matching the gap. Root causes read from source: per instance per frame the extractor does several hash lookups (`is_managed`, `is_loaded_with_dependencies`, `queue_upload_if_needed` which also *cloned a ~150-byte tuple*, material and binding lookups) and rebuilds/uploads four whole `StorageBuffer<Vec<_>>`s plus one visibility bit-vector per view.

Contained optimizations tried (interleaved old/new binaries, 3 pairs each):
* **Per-frame asset cache + no tuple clone in the extractor: extract 7.07 → 5.81 ms (−1.26 ms, −18 %)** — consistent in both rounds. Kept.
* Hoisting `RenderLayers::default()` / pre-sizing / memoizing the per-view visibility loop in `prepare`: **no gain, `prepare` +0.3 ms in every pair** (equality on `RenderLayers` costs more than the `intersects` it avoids). Reverted.
* `NoCpuCulling` on the camera: frame 19.07 vs 19.06 ms, the CPU culling system still ran at 1.65 ms. **No effect on meshlet scenes.**

Conclusion: contained fixes address ≈ 9 % of the CPU path; **the remaining ≈ 13 ms at 100 k needs the retained GPU scene**. A *static fast path* ("skip everything if nothing changed") was investigated and rejected: `RenderMaterialInstances.current_change_tick` is bumped every frame (so `is_changed()` is useless) and materials are registered only for entities whose `ViewVisibility` is true, so a moving camera continually flips instances in and out of the table — any scene with a moving camera is "dirty". A correct retained design must make material registration incremental too (see `gpu_scene.md`).

## 11. Shadow LOD: speed vs. shadow retention — **[Bench]**

Metric: fraction of ground pixels that are shadowed in an **aerial** frame (150 m up; far cascades dominate), relative to the default (3.17 %); tool: `forest_benchmark/tools/shadow_coverage.py`. Frame time: 3 interleaved drive-path runs per config (mean frame ms vs the base in the same loop).

| Setting | frame vs base | shadowed ground retained |
|---|---:|---:|
| base (scale 1, step 1) | 25.19 ms | 100 % |
| uniform scale 1.25 | −6.6 % | 97 % (3.06 %) |
| **uniform scale 1.5** | **−15.5 %** | **84 %** (2.66 %) |
| per-cascade step 1.1 | −3.6 % | 97 % |
| per-cascade step 1.25 | −13.1 % | 80 % |
| per-cascade step 1.5 | −28.3 % (far cascade 10.1 → 3.6 ms) | 26 % (0.82 %) |
| uniform scale 2 | ≈ −25 % | 41 % (1.31 %) |
| per-cascade step 2 | −30.5 % | 13 % (0.41 %) |
| uniform scale 4 | — | 13 % (0.41 %) |

**Findings.** (1) Speed bought by coarse shadow LOD is bought with *missing shadows*: thin branches/leaves simplify away entirely (the Preserve-Area problem, `foliage.md`). (2) The earlier single-view check ("scale 4 looks near-identical", near camera) was misleading — near cascades are fine-grained; the aerial view exposes the loss. (3) In this view, **uniform scaling dominates per-cascade stepping** (1.5 uniform: −15.5 % at 84 % vs step 1.25: −13.1 % at 80 %), so the new `shadow_cascade_lod_error_scale_step` is shipped but **not recommended**; both defaults stay `1.0`. (4) The proper fix is not a scale but keeping thin geometry in coarse clusters (area-preserving simplification) or page-based shadows. Caveats: one scene, one aerial frame, one threshold; the metric counts dark ground pixels, not shadow shape fidelity.

## 12. Retained meshlet instance table (the "pure GPU scene" step) — **[Bench]**

`MeshletSettings::retained_instances` (default **`false`**, opt-in). Instead of clearing and rebuilding the instance table from every `MeshletMesh3d` entity each frame and re-uploading four whole buffers plus one visibility bit-vector per view, the retained mode keeps rows in stable slots (swap-remove, dense), extracts only entities whose `MeshletMesh3d`/`GlobalTransform`/`PreviousGlobalTransform`/`RenderLayers`/`NotShadowCaster`/`NotShadowReceiver` changed (plus removals), uploads only dirty element ranges (`RetainedBuffer`, bitset dirty tracking, byte-identical std430 layout to `StorageBuffer<Vec<T>>`), resolves materials incrementally (refcounts, no per-frame table scans; meshlet material registration no longer depends on CPU `ViewVisibility` in this mode), and shares one incrementally-updated visibility mask per (view layers, shadow-view) signature across views. Any `AssetEvent<MeshletMesh>` forces a full resync.

### Methodology (what "accurate" means here)
* **Real before/after binary**: "head" = the committed code before this change, built from a separate git worktree with its own benchmark app; "rebuild" = the new binary with the original algorithm (`retained_instances = false`); "retained" = same binary, setting on. Same machine, release builds, window 1280×720, `AutoNoVsync`, 300 measured frames after 60 warm-up.
* **Interleaved**: configurations run round-robin, order rotated every repetition (5–6 repetitions; 4 for the dynamic scenarios).
* **A/A control**: a second copy of the baseline (`head_AA` / `rebuild_AA`) in every set shows the noise floor; differences within it are not claimed. Welch t is reported against the baseline.
* **Same-scenario baselines**: dynamic scenarios compare retained against *rebuild with the same moving/churning instances*, not against a static run.
* **Correctness oracle** (not only images): `MeshletSettings::validate_retained_instances` rebuilds the expected table from the live world every frame and compares it row by row (transforms, previous transforms, AABBs, BVH roots, layers, shadow flag, BVH depth) and, after the queue stage, every resolved material id. Passed **0 mismatches over 250–300 frames in 7 scenarios** (static, 10 % moving, 100 % moving, 200 spawn+despawn/frame, both, 100 k instances with 2 % moving + 500 churn/frame, shadows on). **Mutation test**: removing `Changed<GlobalTransform>` alone was *not* detected (legitimately: `PreviousGlobalTransform` changes at the same time, so the mutant was equivalent); removing both transform filters produced **178 055 logged mismatches**, so the validator does catch real faults.
* **Pixel equivalence**: retained vs rebuild screenshots at frame 100 are **bit-identical** (0 differing pixels, 4 runs each) in a static scene and in a dynamic scene (20 % of trees moving + 50 respawns/frame, moving camera, shadows on). (Earlier in this project identical runs of older binaries differed in ~15 k pixels; that nondeterminism did not reproduce in these runs and its cause was not investigated.)
* Unit tests (18 in `meshlet`): range encoding vs full encode for `u32`, the padded `MeshletAabb` and the real `MeshUniform`; a regression test for encase's non-truncating scratch (a real bug the dynamic scenario exposed as a `write_buffer` overrun); swap-remove/slot invariants; BVH-depth histogram; material refcounts; slot-bit patching.

### Results (mean frame ms; GPU `meshlet_visibility_buffer_raster` span in the last column)
Static scene, shadows off, drive path, opaque 23 k-tri trees (final binary, n = 6; 50 k from the previous round, n = 5):

| Instances | head | rebuild (same binary) | **retained** | retained vs head | A/A control | raster span head → retained |
|---:|---:|---:|---:|---:|---:|---|
| 12 k | 5.80 | 5.70 (−1.8 %) | **4.17** | **−28.2 %** | −1.9 % | 3.49 → 3.28 |
| 50 k | 11.11 | 11.58 (+4.2 %) | **6.19** | **−44.3 %** | +2.6 % | 5.29 → 5.00 |
| 100 k | 18.67 | 18.72 (+0.3 %) | **8.53** | **−54.3 %** | +0.7 % | 7.46 → 6.96 |
| 200 k | 36.09 | 34.65 (−4.0 %, n = 5) | **12.71** | **−64.8 %** | −1.7 % | 11.45 → 10.74 |

Dynamic/churn at 100 k, shadows off, **retained vs rebuild with the same scenario** (previous round, n = 4; A/A within ±3.4 % and mostly < 1.5 %):

| Scenario | rebuild | retained | change |
|---|---:|---:|---:|
| 1 % of instances move every frame | 21.56 | 9.43 | **−56.3 %** |
| 10 % move | 20.38 | 9.01 | **−55.8 %** |
| 50 % move | 20.59 | 17.17 | **−16.6 %** |
| **100 % move** | 21.29 | 27.59 | **+29.6 % (regression)** |
| 100 spawn+despawn per frame | 22.56 | 9.18 | **−59.3 %** |
| 1000 spawn+despawn per frame | 33.02 | 14.50 | **−56.1 %** |

Shadows on (3 cascades, 2048², previous round, n = 5): 12 k **−12.2 %** (24.88 → 21.84), 100 k **−12.6 %** (33.84 → 29.59) — the frame is dominated by shadow rendering (§3, §7), which this change does not touch.

CPU span breakdown at 100 k (`trace_chrome`, 4 interleaved runs, ms/frame): `extract_meshlet_mesh_entities` 5.67 → **0.42**, `prepare_meshlet_per_frame_resources` 4.18 → **0.24**, `queue_material_meshlet_meshes` 1.22 → **0.08**, `prepare_material_meshlet_meshes*` 1.14 → **0.03**, `ExtractSchedule` 7.26 → **1.27**. Remaining CPU hot spot: `check_visibility_cpu_culling` 1.7 ms (unchanged; it is wasted work for meshlets).

### Findings and limits
* The result matches the pre-implementation estimate (≈ 9 ms at 100 k, ≈ 13 ms at 200 k): the scene becomes **GPU-bound** (retained 100 k = 8.5 ms vs GPU spans ≈ 8 ms).
* **Not a win for scenes where most instances move every frame**: break-even is between 50 % and 100 % of instances changing per frame (per-row cost of the incremental path ≈ 2× the bulk rebuild). Use the default (rebuild) mode for such scenes; an adaptive switch was not implemented.
* The GPU raster span is 5–7 % lower with retained instances. **Hypothesis (untested)**: the iGPU shares memory bandwidth with the CPU-side uploads that rebuild mode performs every frame.
* Rebuild-mode parity needed two fixes found by tracing: (1) taking `&mut ResMut` for the asset resources per instance marked them changed on every instance; (2) the asset-cache lookup as a non-inlined function cost 0.9 ms per 100 k instances (inlining restored parity: the loop measured 6.96 → 6.07 ms vs the original 5.67 ms in an in-process timing harness; end-to-end the A/A-bounded difference is within noise). A residual of ≈ +0.4 ms per 100 k remains in that in-process measure.
* One 200 k run (twice across rounds) ended with "No windows are open, exiting" before printing a row — a window-system flake, not a panic; those cells report n = 5.
* Material ids are now stable across frames in retained mode and must stay ≤ 65 535 (the shade pass draws `material_id × 3`); asset reload, new materials or toggling the setting at runtime trigger a resync (the generic material extractor re-registers all meshlet entities on the frame the setting changes).
* Not measured: GPU-written transforms, instances without ECS entities (stage 3 of the plan), page-granular residency.

## 13. Reproduce

```bash
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --meshlet-tris 64 --no-shadows --label t64
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --cascades 1 --label c1
cargo test -p bevy_pbr --features meshlet_processor --lib meshlet
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --shadow-lod-scale 1.5 --top-spans 40
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --path aerial --shadow-lod-scale 1.5 --screenshot-frame 60 --screenshot-path a.png   # then tools/shadow_coverage.py
cargo run --release -p forest_benchmark --features meshlet,trace -- --meshlet --no-shadows --trees 100000   # trace-*.json, summarize with tools/trace_summary.py
# retained instance table: scenarios and the correctness oracle
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --retained --validate --dynamic-fraction 0.1 --churn 200
# interleaved A/B with A/A control (see the docstring of tools/ab_bench.py)
python3 examples/large_scenes/forest_benchmark/tools/ab_bench.py --reps 5 --common "--meshlet --no-shadows --trees 100000 --frames 300" --config rebuild=./forest --config retained=./forest:--retained --config rebuild_AA=./forest
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --rebuild-shadow-hzb   # old behaviour
cargo run --release -p forest_benchmark --features meshlet -- --meshlet --disable-int64-atomics # fallback test
RUST_LOG=bevy_pbr::meshlet=debug cargo run --release -p forest_benchmark --features meshlet -- --meshlet --cluster-slots 32768   # forced overflow; add --no-degrade / --no-pressure-probe to compare
```
Raw logs of this session were kept in the scratchpad (not committed); numbers above are copied from them.
