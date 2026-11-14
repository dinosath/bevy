# Benchmarks

Every number here was produced by a command in this document on the machine below. Nothing is extrapolated. Where something could **not** be measured, it says so.

## Environment

| | |
|---|---|
| Base commit | `41a1282e3` + the uncommitted changes of this work |
| Toolchain | `rustc 1.101.0-nightly (708c94ef0 2026-11-03)` (stable was 1.99.1 at the time, below the repo's required 1.100; stable 1.100.0 has since been installed and the crate's tests pass on it — benchmarks were not re-run), `--release` |
| GPU | AMD Radeon Vega iGPU (Cezanne/Renoir, **RADV RENOIR**, Mesa 26.2.6, Vulkan 1.4), integrated, shared memory. GPU timestamps + in-pass timestamps available |
| CPU | 16 threads, 58 GB RAM |
| Display | X11 (Xwayland) window 1280×720, `PresentMode::AutoNoVsync`, scale factor 1.0 |
| Forest runs | 600 measured frames after 120 warm-up frames, **one run per row**; five rows were repeated (below) and agreed within ≈1–3 % mean, so treat differences under ~5 % as noise |
| Not available | `VK_EXT_mesh_shader` (mesh-shader path unmeasurable), discrete-GPU numbers, VRAM readings (wgpu exposes none; "peak GPU MiB" in the sim is the scheduler's accounting, not a driver reading) |

---

## 1. Dense forest rendering (real GPU)

`cargo run --release -p forest_benchmark -- --label <x> [flags]` — 11 952 procedural alpha-tested trees, 3 species, jittered grid with a road corridor, **≈7.9 k triangles per LOD0 tree → 94.5 M triangles in the scene at LOD0**, camera drives the road at 90 km/h (fixed distance per frame, so every run renders identical frames). Shadows: 3 cascades to 300 m unless stated. "vis ent." = entities passing CPU frustum + visibility-range checks; "tris" = triangles of those entities (before GPU culling).

### 1a. Each optimisation, measured independently and combined

| Variant | FPS | frame ms mean | p50 | p95 | p99 | max | vis. ent. | tris submitted | speed-up vs baseline |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **baseline** (LOD0 everywhere, GPU preprocessing + CPU frustum) | 12.8 | 78.08 | 77.81 | 85.95 | 87.61 | 88.24 | 6 739 | 53.27 M | 1.0× |
| + `--lod` (3 mesh LODs, `VisibilityRange` crossfade) | 80.4 | 12.44 | 12.39 | 14.56 | 15.80 | 16.25 | 6 767 | 0.39 M | **6.3×** |
| + `--hlod` only (LOD0 < 350 m, merged forest-region proxies beyond) | 38.1 | 26.23 | 26.14 | 29.48 | 30.36 | 33.60 | 912 | 6.64 M | 3.0× |
| + `--occlusion` only (`DepthPrepass` + `OcclusionCulling`) | 9.6 | 103.67 | 103.25 | 114.28 | 117.05 | 120.25 | 6 739 | 53.27 M | **0.75× (slower)** |
| `--lod --hlod` | 82.2 | 12.17 | 12.00 | 14.88 | 15.47 | 16.07 | 943 | 0.47 M | 6.4× |
| `--lod --hlod --occlusion` | 98.7 | 10.13 | 10.00 | 12.41 | 12.99 | 13.51 | 943 | 0.47 M | **7.7×** |
| baseline, GPU culling only (`--no-cpu-culling`) | 11.5 | 87.05 | 86.89 | 95.26 | 97.32 | 97.75 | 11 952¹ | 94.47 M¹ | 0.9× |
| baseline `--no-shadows` | 17.8 | 56.16 | 55.80 | 63.54 | 65.20 | 65.72 | 6 114 | 48.33 M | 1.4× |
| `--lod --hlod --occlusion --no-shadows` | 225.3 | 4.44 | 4.31 | 5.42 | 7.24 | 8.07 | 491 | 0.24 M | 17.6× |
| aerial (150 m up) baseline | 11.6 | 85.91 | 86.07 | 93.39 | 94.31 | 95.08 | 6 973 | 55.11 M | — |
| aerial `--lod --hlod --occlusion` | 160.1 | 6.25 | 6.08 | 8.58 | 9.33 | 9.53 | 748 | 0.21 M | 13.7× vs aerial baseline |

¹ with CPU culling off every entity counts as "visible" on the CPU; the GPU culls them.

Repeat runs (same command): `baseline` 77.80 ms (vs 78.08), `lod` 12.50 (12.44), `hlod` 26.15 (26.23), `lod+hlod+occlusion` 10.03 (10.13), all within 1 %.

**Findings (measured, not assumed)**
* **Mesh LOD is the dominant win** here (6.3×): LOD0 trees are ~7.9 k triangles of leaf cards + branches; beyond 40 m they are invisible detail but dominate geometry/overdraw cost.
* **Forest HLOD alone gives 3×** by replacing every tree past 350 m with one of 178 region proxies (912 visible entities in total, mostly near trees); on top of LOD it adds only ≈2 % (82.2 vs 80.4 fps) because LOD2 trees are already cheap — its value is **CPU/entity scaling** (visible entities 6 767 → 943) and it lets LOD2 instances be unloaded entirely (streaming memory), not raw GPU time in this scene.
* **Occlusion culling alone made things worse (0.75×)**: the scene is alpha-tested leaf cards that don't occlude, so the early/late depth prepass doubles geometry work without culling much. It helps only after LOD/HLOD shrank the geometry (10.13 vs 12.17 ms, **−17 %**, partly the depth prepass cutting fragment overdraw). Rule: enable it only after measuring, per the Bevy docs.
* **Shadows are a large fixed cost**: 22 ms of the 78 ms baseline and 5.7 ms of the 10.1 ms optimised frame (the optimised scene without shadows runs at 225 fps). Shadow LOD (cheap/no casters for far HLOD, fewer cascades) is the next lever; **not implemented or measured here**.
* GPU-only culling (no CPU frustum test) was *slower* (87 vs 78 ms) in the baseline: 12 k instances all go through GPU preprocessing instead of 6.7 k.

GPU pass spans (avg ms, `RenderDiagnosticsPlugin`; spans don't cover every pass — shadow cascade spans only appear for the meshlet path — so wall-clock frame time above is the authoritative number):

| Variant | `main_opaque_pass_3d` | sum of recorded spans |
|---|---:|---:|
| baseline | 57.75 | 58.1 |
| `--lod` | 6.78 | 7.1 |
| `--hlod` | 13.13 | 13.4 |
| `--occlusion` | 44.38 (+ prepasses) | 83.0 |
| `--lod --hlod --occlusion` | 3.10 | 4.5 |
| baseline `--no-shadows` | 53.31 | 53.6 |

HLOD generation cost (bake/activation time): the benchmark's own timer measured 16.5–17.9 ms for its 11 952 trees → 178 regions; criterion measured `forest_hlod_build/10k_trees` = **17.9 ms** (single thread, runs once per cell load on a worker, not per frame).

### 1b. Meshlets (Bevy virtual geometry) for high-poly wood

Same placement; **opaque** trunk + 220 branches only (~23 k triangles/tree → **280.0 M triangles** in the scene), same material and shadows, standard path vs `MeshletPlugin`. Meshlet feature probe on this machine: **all six required wgpu features are present** (`TEXTURE_INT64_ATOMIC`, `TEXTURE_ATOMIC`, `SHADER_INT64`, `SUBGROUP`, `DEPTH_CLIP_CONTROL`, `IMMEDIATES`), and it runs. Baking 3 meshes: 265 ms total (`MeshletMesh::from_mesh`).

`cargo run --release -p forest_benchmark --features meshlet -- --wood` vs `-- --meshlet`

| Variant | FPS | frame ms mean | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|---:|
| standard path (`--wood`) | 7.4 | 134.58 | 133.70 | 152.18 | 154.89 | 155.69 |
| meshlets (`--meshlet`) | 33.9 | 29.48 | 28.67 | 33.95 | 35.91 | 45.11 |
| standard, no shadows | 9.6 | 104.39 | 104.22 | 121.84 | 125.51 | 126.82 |
| meshlets, no shadows | **182.1** | **5.49** | 5.37 | 6.08 | 8.19 | 9.07 |
| standard, aerial | 7.5 | 132.47 | 132.53 | 146.92 | 149.11 | 246.76 |
| meshlets, aerial | 41.0 | 24.41 | 23.84 | 27.27 | 27.70 | 39.22 |

Meshlet repeat: 28.67 ms (vs 29.48). **4.6× with shadows, 19× without** on this scene. GPU spans (meshlets, with shadows): `meshlet_visibility_buffer_raster` 3.0 ms, `meshlet_material_opaque_3d_pass` 0.48 ms, but **shadow cascades 4.8 + 5.9 + 11.5 ms = 22.2 ms** — shadow rendering is 75 % of the meshlet frame. Standard path `main_opaque_pass_3d` = 100.9 ms.

Caveats: meshlets here are the *only* geometry (no leaves — they can't do alpha-tested leaves), CPU-visible counters are 0 for meshlets because no ECS visibility applies, and this is an integrated GPU at 720p. Don't read 19× as a general constant.

---

## 2. Streaming simulation (deterministic; **synthetic IO model, not real disk**)

`cargo run -p bevy_world_streaming --release --example streaming_sim` (full output: [`streaming_sim_full.md`](streaming_sim_full.md), all rows). The real `StreamingScheduler` + `InterestBuilder` run against an IO model (latency, shared bandwidth, per-job decode). Drives 60 s at constant speed after warm-up and ramp. 128 m cells, 60 fps.
* **critical-miss frames**: frames with any cell within 200 m of the camera not resident (a visible streaming failure).
* **fallback cell-frames**: Σ frames of non-resident cells inside the 256–384 m activation radius (drawn with the coarse HLOD fallback instead).
* Strategies: `radius-only` (static radius), `radius-inflated` (radius += v·6 s — the naive fix), `predictive` (kinematic), `route-aware` (kinematic + road corridor).

**Light world (3/6 MiB cells, 512 m radius, SATA SSD):** 0 critical misses and 0 fallback cell-frames for **every** strategy at every speed 30–300 km/h (all 56 rows) (IO ≤ 16 MB/s of a 450 MB/s device). I.e. a modest world needs no prediction on fast storage — the prefetch machinery earns its keep only in constrained conditions below.

**Heavy world** (24/48 MiB cells, tight 320 m load radius, as when VRAM limits the radius), **HDD-class storage** (120 MB/s, 12 ms), winding road, 60 s per row:

| km/h | strategy | critical-miss frames | fallback cell-frames | latency p50/p99 (ms) | IO MB/s | peak GPU MiB | spec hit % | waste % |
|---:|---|---:|---:|---|---:|---:|---:|---:|
| 200 | radius-only | 0 | 14 | 567 / 1217 | 67 | 2090 | – | 0 |
| 200 | radius-inflated | 0 | 0 | 1317 / 2150 | 119 | **5795** | – | 0 |
| 200 | predictive | 0 | 0 | 467 / 1383 | 69 | 2435 | 87 | 9 |
| 200 | route-aware | 0 | 0 | 450 / 1367 | 69 | 2435 | 87 | 9 |
| 250 | radius-only | 0 | **327** | 700 / 1383 | 85 | 2129 | – | 0 |
| 250 | radius-inflated | 0 | 0 | 1383 / 2150 | 119 | **6144 (VRAM budget)** | – | 0 |
| 250 | predictive | 0 | 0 | 600 / 1317 | 100 | 2668 | 76 | 20 |
| 250 | route-aware | 0 | 0 | 583 / 1333 | 100 | 2671 | 76 | 19 |
| 300 | radius-only | **13 (0.4 %)** | **1 352** | 817 / 1683 | 104 | 2193 | – | 0 |
| 300 | radius-inflated | **210 (5.8 %)** | 1 674 | 1383 / 2150 | 119 | 6140 | – | 0 |
| 300 | predictive | 0 | 13 | 783 / 1517 | 118 | 2770 | 80 | 15 |
| 300 | route-aware | **0** | **0** | 717 / 1450 | 117 | 2827 | 83 | 12 |

(30–150 km/h: 0 critical misses and 0 fallback frames for all strategies. On the **straight** HDD road, radius-only had 1 175 fallback cell-frames at 250 km/h and 73 critical-miss frames (2.0 %) at 300 km/h, radius-inflated 52 (1.4 %), predictive/route-aware 0/0. The heavy world on **SATA SSD**: all 56 rows are 0/0 — no strategy is needed there.)

**Findings.** Prediction removes the fallback frames of the static radius at +~25 % GPU memory, whereas inflating the radius to get the same reach spends **2.7× the VRAM** and, at 300 km/h, becomes *worse* than doing nothing (the IO queue saturates with cells that arrive too late, 5.8 % critical misses). Route-awareness gave a small but real gain only at 300 km/h on the winding road (13 → 0 fallback frames vs kinematic). Speculative waste is 9–20 % at 200–250 km/h. Limits of the evidence: synthetic storage model, synthetic cells, a single road shape family (sine serpentine / straight); no real disk, no decode cost beyond bytes/throughput, no GPU upload cost beyond the per-frame byte cap.

**Teleport** (SATA SSD, light cells): cold 0.20 s to a resident 200 m ring (11 critical-miss frames); with the destination announced 1 s or 3 s ahead 0.12 s (6 frames). Not 0 because the announced target only pins the *mandatory* radius (160 m) while "critical" is 200 m — a config interplay, not a scheduler fault. In-flight IO never exceeded `max_inflight_io` (8).

---

## 3. CPU cost of the streaming core (criterion)

`cargo bench -p bevy_world_streaming --bench streaming`

| Benchmark | time |
|---|---:|
| `interest_build/route_aware_200kmh` | 6.33 µs |
| `scheduler_tick/interest+tick_steady_state_200kmh` | 8.96 µs |
| `forest_hlod_build/10k_trees` | 17.99 ms |
| `page_reconcile/2000_assets_steady` | **98.9 µs** |

Optimisation record (`rust-performance` format) for `page_reconcile`:
* **baseline** 7.43 ms; **bottleneck**: over budget every tick, `trim()` rebuilt/sorted the victim list from all resident assets on every failed growth step (O(n²));
* **change**: build the idle/excess victim lists lazily once per `reconcile` and consume them with cursors (`TrimCtx`);
* **measurement**: 98.9 µs; **result**: 75× faster, 42 unit tests + 1 integration test still green;
* **regression risk**: victim list is a snapshot — excess candidates are re-checked at use (`resident_pages(key) > wanted`), so stale entries are skipped, not evicted wrongly.

---

## Not measured / open

* Real IO (files/pack/NVMe) streaming at speed; GPU upload cost; render-thread stalls under real streaming — the plugin is tested for correctness (one integration test), not benchmarked with real disks.
* The city/GTA test (terrain + roads + buildings + vehicles), the forest-at-speed test with real streamed cells, bridges/props: **scenes not built**. `examples/large_scenes/bevy_city` exists upstream; it was not benchmarked.
* Per-page GPU residency, mesh shaders, impostors, shadow LOD, texture streaming: not implemented (see `architecture.md`).
* Shadow-cost reduction was *identified* (22 ms / 5.7 ms) but not optimised.
