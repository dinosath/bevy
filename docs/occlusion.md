# Occlusion culling / Hi-Z

## Bevy facts [Bevy]
* `OcclusionCulling` view component + `OcclusionCullingPlugin` (`bevy_render/src/occlusion_culling`), two-phase: early depth prepass of last-frame-visible → depth pyramid → test all → late prepass; needs `DepthPrepass`; not with deferred (docs). Standard path tests mesh AABBs (`occlusion_culling.wesl`); the meshlet path tests BVH node/cluster AABBs in `cull_shared.wesl::should_occlusion_cull_aabb` and keeps its own pyramid from the 64-bit visibility buffer (`downsample_depth.wesl` variants `MESHLET_VISIBILITY_BUFFER_RASTER_PASS_OUTPUT`).
* HZB builder: `bevy_core_pipeline::mip_generation::experimental::depth` (SPD-style single pass; TODO subgroups / true single pass). Reverse-Z projection throughout.
* Recent fixes: #26169 (edges, MSAA), #26168 (views <32 px).

## Measured [Bench] (`docs/open_world/benchmarks.md`)
Occlusion **alone**: 78.1 → 103.7 ms (slower) on a leaf-card forest — alpha-tested leaves don't occlude and the prepass doubles geometry work. After LOD+HLOD: 12.17 → 10.13 ms (−17 %). Lesson: occlusion value depends on occluder coverage; enable per scene after a measurement; **expose a CPU-visible `occlusion_culled_count`** (matches UE's Main/Post stats) so the decision can be data-driven (not implemented for meshlets).

## Measured cost of the pyramid build [Bench]
`downsample_depth_with_ctx` rounds the virtual input size up with `(size + 1).next_power_of_two()`, so a 2048² shadow map is reduced as 4096² (the comment says this keeps full precision at the top level for power-of-two inputs). One build costs 0.10 / 0.38 / 1.45 / 5.8 ms for 512 / 1024 / 2048 / 4096 shadow maps (area scaling) and a 3-cascade frame builds the pyramid twice per cascade. Done: removed the redundant second build for shadow views by default (`MeshletSettings`, −15.8 % frame). **Not done**: a power-of-two-source variant (~4× cheaper per build) — needs a dedicated test that the top-level conservative footprint is preserved; not attempted blind.

## Correctness requirements (Part 9) [Proposal tests]
* Conservative: test against max-depth pyramid at a mip chosen so the AABB's screen rect spans ≤2×2 texels; depth of nearest AABB corner; reverse-Z aware.
* Temporal: use *this* frame's HZB for the late pass (existing); camera cuts invalidate previous-frame visibility (flag `camera_cut` ⇒ skip early-pass reuse, render everything in the main pass).
* Scenes: dense city, forest, interior, fast vehicle, disocclusion sweep — each compares GPU-visible set against a **no-occlusion reference** render: pixel diff must be zero in a deterministic path (acceptance test: images identical with/without occlusion except for culling-induced differences = none).
* Shared system: the pyramid builder is already shared by standard and meshlet passes; VSM page culling and RT will reuse it through the same `MipGenerationJobs` API (see `virtual_shadow_maps.md`).
