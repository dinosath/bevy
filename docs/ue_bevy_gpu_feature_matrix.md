# UE 5.9 ↔ Bevy GPU feature matrix

**Evidence labels used in every doc in this series**

| Tag | Meaning |
|---|---|
| **[UE-doc]** | Stated on Epic's current UE 5.9 documentation, fetched in this session (URLs in *Sources*). |
| **[UE-bg]** | UE behaviour from public talks/papers/background knowledge — **not re-verified** in this session. |
| **[Bevy]** | Verified by reading this checkout (`0.20.0-dev`, base `41a1282e3`) or the Bevy GitHub issue tracker (queried 2026-11-14). |
| **[Proposal]** | Proposed design. |
| **[Assumption]** | Experimental assumption to be tested. |
| **[Bench]** | Measured on this machine (AMD Vega iGPU / RADV, 1280×720) — see `performance.md`. |

Status vocabulary: `IMPLEMENTED · PARTIALLY IMPLEMENTED · WIP · UPSTREAM PLANNED · MISSING · RESEARCH REQUIRED · NOT APPLICABLE · BACKEND LIMITED`. A row says `MISSING` only after I grepped the tree (search terms in the *Evidence* column).

> **Scope honesty.** This matrix covers ~35 UE systems, each of which is a multi-month engine project. The work in this change set is the *audit, the design documents, and a first measured slice on top of Bevy's existing meshlet renderer* (cluster-size/stat API, shadow-cost data). It does **not** implement UE-equivalent assemblies, voxels, virtual shadow maps, virtual textures, etc. The "Plan" column says how and in what order they would be built. Where the current docs are the deliverable, that is stated.

## A. Virtualized geometry core

| # | UE feature | UE purpose & architecture | Bevy equivalent / status | Upstream / WIP | Gap | Difficulty · benefit | Dependencies / backend | Plan · tests |
|---|---|---|---|---|---|---|---|---|
| 1 | **Hierarchical clusters + automatic LOD** | Triangle clusters in a hierarchy; error-driven selection removes hand-authored LODs. [UE-doc: "pixel scale detail", no LOD authoring]. 128-tri clusters, DAG with locked group borders [UE-bg] | **IMPLEMENTED (experimental, feature `meshlet`)**: ≤256 verts/128 tris meshlets, groups of ~8 meshlets simplified ~50 % with locked borders, **BVH8**, per-group error → GPU LOD. [Bevy] `crates/bevy_pbr/src/meshlet/from_mesh.rs`, `cull_bvh.wesl`. Evidence: read source | Open fixes: #26467 (software-raster flicker), #25928 (backface SW raster), #26472, #26240 [Bevy-issues] | No extension needed for the idea; gaps are limits (opaque-only, no skinning/WPO, whole-mesh residency) | — · foundational | `TEXTURE_INT64_ATOMIC`, `SHADER_INT64`, subgroups. Bevy docs say Vulkan/Metal; wgpu feature docs list Vulkan, DX12 (SM6.6), Metal (MSL 3.1) for 64-bit image atomics [wgpu 30.0.2 docs] | **Extend, don't replace** (Part 3). This change: `MeshletBuildConfig`, `MeshletMesh::stats()`. Tests: stats consistency, bake with configs |
| 2 | **Screen-space-error LOD** | Per-cluster/group projected error vs pixel threshold during traversal | **IMPLEMENTED**: `lod_error_is_imperceptible` projects group error with FOV/viewport (`cull_shared.wesl`), 1 px threshold hard-coded [Bevy] | — | **Partly closed here**: error budget is now a per-dispatch immediate and `MeshletSettings::shadow_lod_error_scale` exposes it for shadow views (default 1.0). Main-view knob not exposed yet. UE exposes pixels-per-edge scaling [UE-bg] | Low · measured trade: uniform 1.5 ⇒ −15.5 % frame but only 84 % of far shadows retained; 2.0 ⇒ −25 % at 41 % (thin geometry vanishes) [Bench] | none | Expose main-view scale; test: tri count monotone in scale |
| 3 | **GPU culling: frustum, occlusion, LOD during traversal** | Combined in one hierarchy traversal [UE-doc: culling determines visible pieces; multi-level tests] | **IMPLEMENTED (meshlets)**: instance → BVH levels → cluster cull; frustum + HZB occlusion + LOD in `cull_bvh.wesl` / `cull_clusters.wesl`. Standard meshes: GPU preprocessing frustum + optional occlusion [Bevy] | — | no cone/backface cluster culling in meshlet path (grep `cone`: none in meshlet shaders) [Bevy] | Medium · low–medium | none | Bake cone per meshlet (meshopt provides `compute_meshlet_bounds` cone — already called, cone unused); test: no false culling vs brute-force |
| 4 | **Two-phase occlusion + Hi-Z** | Previous-frame-visible drawn first → HZB → test remainder; conservative | **IMPLEMENTED** for both meshlets and standard path: `OcclusionCulling`, early/late passes, `mip_generation::experimental::depth` [Bevy] | #26169/#26168 fixed edge/small-view bugs | HZB builder is `experimental`; "TODO subgroup, true single pass" | Medium · medium | compute | Benchmarks in `performance.md`: occlusion alone *lost* on leaf-card forests [Bench] |
| 5 | **Simple/complex culling classification** | Skinned/spline/assembly/first-person/WPO/moved instances go to a *complex* dispatch, the rest to *simple*; `r.Nanite.Culling.Classification` default 1 [UE-doc 5.9] | **MISSING** for meshlets (meshlet path has one instance-cull shader; no per-instance classification). Evidence: grep `Classification|complex` in `meshlet/`: none | — | Meshlets are rigid-static-only today, so the *need* for a complex path arrives only with skinning/spline/assembly support | Medium · grows with scene size [UE-doc: "gains grow with scene size"] | per-instance flags in GPU scene | Add `InstanceClass` bits to the meshlet instance record **when** a second class exists (don't add speculative complexity) |
| 6 | **Adaptive hierarchy fanout** | Traverse several hierarchy levels per dispatch; `r.Nanite.Culling.AdaptiveFanoutThreadBudget` default 6144; 0 = one level at a time [UE-doc] | **MISSING / one level per dispatch**: `cull_bvh` dispatched per BVH depth with indirect args (`bvh_depth` from asset) [Bevy] | — | Dispatch count ∝ BVH depth ⇒ fixed overhead for small scenes | High · unknown on this GPU | indirect dispatch (have) | Design in `gpu_culling.md`; benchmark per-pass spans first (done: culling ≪ shadows [Bench]) — **not justified yet** |
| 7 | **GPU compaction → raster bins** | Visible clusters binned by raster/material permutation, then drawn | **PARTIALLY IMPLEMENTED**: atomic-append queues (`atomicAdd`), SW/HW raster split, per-material resolve in a visibility buffer [Bevy] | — | no prefix-sum alternative evaluated; atomic contention unmeasured | Medium | — | Compare atomic vs wave-aggregated append using GPU spans |
| 8 | **Software + hardware raster** | Small triangles in compute, large in HW | **IMPLEMENTED**: `visibility_buffer_software_raster.wesl` + hardware path, 64-bit atomic depth|id | open bug #26467 | — | — | — | — |
| 9 | **Mesh shaders** | (UE uses mesh-shader raster paths on some platforms [UE-bg]) | **WIP**: `MeshPipelineDescriptor`, `draw_mesh_tasks*` (PR #25627 merged); only `examples/shader_advanced/mesh_shader_intro.rs`; no meshlet integration [Bevy] | PR "Mesh shader pipelines" merged | no integration with PBR/meshlets; this dev GPU lacks `VK_EXT_mesh_shader` ([Bench env]) | High · unmeasurable here | wgpu `EXPERIMENTAL_MESH_SHADER`; Naga mesh shaders Vulkan-only, passthrough shaders elsewhere [wgpu docs] | Design only; compute+indirect remains the default path (Part 13) |
| 10 | **Fine-grained geometry streaming** | Streaming pages on demand; `r.Nanite.Streaming.StreamingPoolSize`; thrash if too small [UE-doc] | **MISSING at sub-asset granularity.** `PersistentGpuBuffer` + range allocator (whole `MeshletMesh`); no feedback, budget or eviction policy [Bevy]. Policy layer **added earlier**: `bevy_world_streaming::pages` | — | Page-addressable BVH, GPU request feedback, root residency | Very high · high for large worlds | async IO, readback | `geometry_streaming.md`; Phase 7 |
| 11 | **Minimum (root) residency** | `Minimum Residency (Root Geometry)` bytes kept resident to reduce pop-in [UE-doc] | **MISSING in renderer**; modelled in `pages.rs` (page 0 always first) | — | no coarse-root concept in `MeshletMesh` (all-or-nothing) | High | #10 | Phase 7 |
| 12 | **Compression / quantization** | ~**14.4 B/input triangle** on disk in UE's sample; position/normal precision power-of-two steps, default Auto; explicit tangents +~10 % [UE-doc] | **PARTIALLY IMPLEMENTED**: meshlet bit-packed positions (per-meshlet bit widths), octahedral normals, raw `Vec2` UVs, lz4 file [Bevy]. Standard meshes: `MeshCompressionArgs` [Bevy] | #26030 (preserve `KHR_mesh_quantization` on GPU) | UV uncompressed (8 B/vertex); no tangents; lz4 only (fast, large) | Medium · IO/VRAM | — | **Measured**: `MeshletMesh::stats()` → B/src-tri vs UE's 14.4 [Bench] |
| 13 | **Fallback mesh** | Auto/percent/error target fallback for non-Nanite platforms, collision, lightmaps, RT; built from LOD0 [UE-doc] | **MISSING as an asset concept** (a `MeshletMesh` has no sibling `Mesh`). Evidence: grep `fallback` in `meshlet/`: none | — | asset pair + fallback generation (meshopt simplify exists in `from_mesh`) | Medium · enables unsupported GPUs, RT, collision | #1 | Phase 9; `AssetPack{virtual, fallback}` |
| 14 | **Preserve Area (foliage)** | Dilate open boundary edges to redistribute lost surface area during simplification; foliage-only [UE-doc] | **MISSING**; meshlet simplification uses `SimplifyOptions::Sparse \| ErrorAbsolute` with attribute-aware simplify and locked borders [Bevy] | — | thin geometry shrinks/vanishes in coarse LODs | Medium · quality | meshopt | `foliage.md`: experiment plan (area-loss measurement) |
| 15 | **Max 16 M instances; instance GPU scene** | Hard-limit 16 M instances [UE-doc] | **PARTIALLY IMPLEMENTED**: standard path has GPU instance buffers + preprocessing; meshlets re-extract every `MeshletMesh3d` entity every frame (`InstanceManager`) [Bevy] | #26257 (GPU preprocess bind groups recreated per frame) | no retained GPU scene for meshlets ⇒ CPU cost ∝ instances | High · large for forests | — | `gpu_scene.md`; Phase 2 |

## B. Foliage

| # | UE feature | UE facts | Bevy status | Gap · plan |
|---|---|---|---|---|
| 16 | **Nanite Assemblies** (micro-instancing) | Parts instanced inside an assembly "simply a large array of matrices"; skeletal & static; up to **65k part instances** per assembly "with a small performance trade-off during cluster culling"; no nesting; per-assembly copy of part data; far parts merge into base mesh; Witcher 4 demo: largest tree 3.5 GB → ~29 MB disk, 36 MB → ~2.7 MB streaming/view, ~500k instances of dozens of variants with 1–10 M polys each [UE-doc] | **MISSING** (grep `assembly`/`micro.instanc` : none). Bevy has `EXT_mesh_gpu_instancing` import and standard GPU instancing [Bevy] | Needs hierarchy nodes that carry a *transform array reference* resolved during traversal. [Proposal] `foliage.md`: node type `InstanceGroup{part_bvh, transforms_range}`; Phase 11. Difficulty very high |
| 17 | **Nanite Voxels** | Clusters voxelized into bricks of 4×4×4 voxels, ≤128 bricks/cluster; binned separately, special raster permutation, front-to-back depth buckets; chosen when voxels yield lower simplification error than triangles; "likely to change or be removed" (experimental) [UE-doc] | **MISSING**, RESEARCH REQUIRED | `foliage.md`: compare impostor alternatives (octahedral impostors, aggregate HLOD meshes) before any voxel work; Phase 13 |
| 18 | **Nanite Skinning (foliage wind)** | Bone-hierarchy wind instead of WPO; reasons: WPO needs worst-case *Max WPO Displacement* padding → conservative bounds/overdraw and extra raster bins; skinning gives accurate bounds from skinning matrices and fixed-function raster; *Animation Min Screen Size* default 0.1 (`r.Skinning.DefaultAnimationMinScreenSize`); 100,000 updating bones ≈ 0.1 ms GPU; VSM requires distance animation cutoff [UE-doc] | Standard skinning exists (`bevy_pbr/src/render/skin.rs`, `SkinnedMeshBounds` in `bevy_mesh`). **Not available on meshlets.** No wind system (grep `wind`: none) [Bevy] | skeletal meshlets = Phase 17; wind sim Phase 12 |
| 19 | **Dynamic Wind plugin** | Per-bone GPU sim; slow sway + fast flutter by mass/area; plants share a small set of simulations, switch to own sim on local force; radial/swirl/cone/directional/character forces [UE-doc 5.9 release notes]; limitations on the docs page (global direction only, no collision, manual JSON import — these describe an older revision and may differ from the 5.9 release-note text; **conflicting wording kept as-is**) | MISSING | Phase 12 |

## C. Special geometry classes

| # | UE feature | Bevy status | Notes |
|---|---|---|---|
| 20 | Skeletal Nanite | Standard GPU skinning **IMPLEMENTED**; skinned meshlets **MISSING** (meshlet docs: no skinning) | Rigid-per-cluster skinning with animated cluster bounds; Phase 17 |
| 21 | Spline meshes | **MISSING** (grep `spline` in render crates: only curve math) | needs conservative bounds under deformation; Phase 15 |
| 22 | Tessellation / displacement | **MISSING** (UE needs `r.Nanite.AllowTessellation=1` read-only [UE-doc]) | Phase—not planned; only if a measured need appears |
| 23 | Nanite landscape / terrain | **MISSING** (grep `terrain`: none) | `virtual_textures.md` + terrain note in `roadmap.md`; Phase 14 |

## D. Shadows, textures, lighting data

| # | UE feature | UE facts | Bevy status | Plan |
|---|---|---|---|---|
| 24 | **Virtual Shadow Maps** | Virtual res **16k×16k**, **128×128 pages**, one physical page pool (`r.Shadow.Virtual.MaxPhysicalPages`); directional light = clipmap levels 6–22 (64 cm → ~40 km, each 2× radius); spot = one VSM + mips; point = cubemap of 6; caching with invalidation (light move invalidates all; moving casters invalidate overlapped pages; WPO/skeletal every frame); separate static/dynamic depth; SMRT soft shadows; Nanite always renders to VSM [UE-doc]; requires 64-bit atomics (DX12 SM6.6 / `VK_KHR_shader_atomic_int64`) [UE-doc] | **MISSING** (grep `virtual shadow`: none; Bevy uses cascaded shadow maps, `bevy_light`, `per_view_shadow_pass`; meshlets render each cascade as a separate view [Bevy]). [Bench] shadow cascades = **75 % of the meshlet frame** | Largest measured cost ⇒ highest payoff after LOD. `virtual_shadow_maps.md`; Phase 10 |
| 25 | VSM cache invalidation | per-light/per-page invalidation by caster bounds [UE-doc] | MISSING | needs retained GPU scene + per-instance "moved this frame" |
| 26 | **Virtual textures** | complementary to Nanite [UE-bg] | **MISSING**; community prototype proposed in **#25986** "Software Virtual Texturing" (open) [Bevy-issues] | `virtual_textures.md`; coordinate with #25986 rather than duplicate; Phase 16 |
| 27 | **GPU Scene** | persistent instance/primitive buffers [UE-bg] | **PARTIALLY IMPLEMENTED**: standard path retains instance data (`gpu_preprocessing.rs`, `SparseBufferVec` dirty updates); not shared with meshlets, shadows get separate views [Bevy] | `gpu_scene.md`; Phase 2 |
| 28 | **Bindless / material tables** | Nanite shades via material IDs/bins | **IMPLEMENTED for materials**: bindless slabs (`render_resource/bindless.rs`, `material_bind_groups.rs`) with open bugs #26429, #26427 [Bevy]; meshlet path has per-material shade passes | `gpu_scene.md` |

## E. Ray tracing

| # | UE feature | UE facts | Bevy status | Plan |
|---|---|---|---|---|
| 29 | **Nanite RT via cluster acceleration structures (CLAS)** | `r.RayTracing.Nanite.Mode 2`, needs GPU/driver cluster ops; pool `CLASBufferSize` default 512 MB (max 2048); demand-driven; quality scaling thresholds 85 %/70 %, min scale 0.3; fallback meshes without support [UE-doc] | **BACKEND LIMITED / MISSING**: Bevy's `bevy_solari` builds a **BLAS per mesh** (`solari/src/scene/blas.rs`) with `EXPERIMENTAL_RAY_QUERY`; wgpu 30.0.2 exposes no cluster-acceleration-structure feature (grep `cluster` in `features.rs`: none) [Bevy][wgpu] | Realistic path: RT over **fallback meshes** with LOD by demand + BLAS memory budget; `ray_tracing.md`; Phase 18 |

## F. Tooling

| # | UE feature | Bevy status | Plan |
|---|---|---|---|
| 30 | NaniteStats, visualization modes (Mask, Triangles, Patches, Clusters, Primitives, Instances, Overdraw, Raster Bins, Shading Bins…) [UE-doc] | **MISSING** for meshlets. Standard: `bevy_dev_tools::render_debug`; meshlet has `MeshletDebugMaterial` example only [Bevy] | Add cluster/LOD debug via resolve shader flags (Phase 1 follow-up) |
| 31 | `MaxCandidateClusters`/`MaxVisibleClusters` (12 B / 16 B each; overflow ⇒ missing or blinking geometry) [UE-doc] | **PARTIALLY IMPLEMENTED (this change)**: `MeshletPlugin::cluster_buffer_slots` (4 B/slot, ≤ 2^25) is still fixed and the shaders still don't bounds-check, **but overflow is now detected** (GPU counter copies + async readback, +0.05 ms) and **degraded gracefully** (`MeshletPressure` → LOD error scale; forced-overflow test: detected frame 1, converged in 3 events). Visible/SW-raster buffers and second-pass levels not probed | Per-view / far-only degrade policy; visible-cluster counters |
| 32 | GPU profiling counters | **PARTIALLY**: `RenderDiagnosticsPlugin` spans incl. `meshlet_visibility_buffer_raster`, per-cascade shadow spans, **plus new per-stage shadow spans added here** [Bevy][Bench] | extend with cluster counters |

## G. Systems that are UE-specific and **not** to be copied

World Partition (Bevy: `bevy_world_streaming`), Lumen (Bevy: `bevy_solari`, different), PCG/Mass: `NOT APPLICABLE` as Nanite-equivalent work.

## Sources
- UE 5.9 release notes — https://dev.epicgames.com/documentation/unreal-engine/unreal-engine-5-9-release-notes
- Nanite Technical Details — https://dev.epicgames.com/documentation/unreal-engine/nanite-technical-details
- Nanite Foliage — https://dev.epicgames.com/documentation/unreal-engine/nanite-foliage
- Nanite Assemblies — https://dev.epicgames.com/documentation/unreal-engine/nanite-assemblies
- Virtual Shadow Maps — https://dev.epicgames.com/documentation/unreal-engine/virtual-shadow-maps-in-unreal-engine
- Bevy issues/PRs: #25986, #26467, #25928, #26030, #25627, #26257, #26429, #26427 (bevyengine/bevy, queried 2026-11-14)
- wgpu-types 30.0.2 `features.rs` docs (local registry)
