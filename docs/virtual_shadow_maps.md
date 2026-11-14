# Virtual shadow maps

## UE reference [UE-doc: Virtual Shadow Maps page]
16k×16k virtual resolution per map, **128×128 pages**, allocated only where on-screen pixels need them (depth-buffer analysis), one physical page pool shared by all lights (`r.Shadow.Virtual.MaxPhysicalPages`); directional light = clipmap levels 6–22 (64 cm … ~40 km, each 2× the radius); spot = one VSM + mip chain; point = cube of six; caching on by default with invalidation (light moved ⇒ all pages; caster moved/added/removed ⇒ overlapped pages; WPO/skeletal ⇒ every frame); **separate static and dynamic depth** so dynamic movement doesn't redraw static; coarse pages for fog/translucency; SMRT ray-marched soft shadows; Nanite always renders into VSM. Requires 64-bit atomics (DX12 SM 6.6 / `VK_KHR_shader_atomic_int64` / Apple M2+). Stats: pages allocated/cached/invalidated.

## Bevy facts [Bevy] / measured [Bench]
* Shadows are cascaded maps (`bevy_light`, `CascadeShadowConfigBuilder`) rendered as extra views each frame; meshlets run a **full cull + raster per shadow view**, with no cache (`visibility_buffer_raster_node.rs`).
* **Measured**: for 280 M triangles of opaque wood, the 3-cascade shadows cost 23.3 ms of a 28.7 ms frame; 1 cascade 11.2 ms; farthest cascade is most expensive (11.5 of 22.2 ms) (`performance.md` §3). Cluster size strongly modulates it (32-tri clusters ⇒ 58 ms).
* Bevy already has the primitives VSM needs: u64 image atomics (meshlet visibility buffer), HZB builder, per-view GPU culling, `PersistentGpuBuffer`-style range allocation, early/late shadow passes (`EARLY_SHADOW_PASS`).

## Design [Proposal] — staged so each stage is measurable and independently shippable
| Stage | What | Expected effect (to be measured) | Risk |
|---|---|---|---|
| S0 | Instrument: per-cascade clusters visible/culled, pixels covered (already per-cascade spans) | baseline | none |
| S1 | **Cheap wins in existing cascades** — **partly done**: `shadow_lod_error_scale` (coarser LOD for shadow views) and removal of the redundant per-view depth-pyramid rebuild: measured **−15.8 %** (rebuild removal) and **−36.7 %** (with scale 2) of the meshlet frame (`performance.md` §7). Remaining: per-cascade scale (far cascade is 7.5 of 11.6 ms), far-cascade update rate, tighter cascade fitting | measured | temporal lag; quality: scale 4 looked near-identical in one view, not a study |
| S2 | **Page-demand marking**: screen depth → required shadow-page mask per light (compute, same math as UE) | renders only visible regions | needs a page table sampled in lighting |
| S3 | Physical pool + page table; per-page *view* culling reuses `cull_instances/bvh/clusters` with a page rect instead of a frustum | | large shader+lighting change |
| S4 | **Cache**: keep static pages, invalidate by moved-instance bounds (needs `moved` flag in GPU scene, `gpu_scene.md`) | the only stage that avoids *re-rendering* static geometry (the 23 ms) | invalidation correctness, LOD-change invalidation (UE has an experimental cvar for it) |
| S5 | Local lights (cube/spot), SMRT-style filtering, coarse pages | | |
Mandatory interlocks: foliage animation must be cut off with distance or its shadows invalidate every frame (UE docs say the same); skinned/WPO-like casters invalidate per frame.

## Tests
Reference test: VSM output vs the existing cascade shadows on a static scene — shadow-mask difference bounded; cache test: static scene frame 2+ renders **0** pages (counter); invalidation test: move one instance ⇒ only overlapped pages re-rendered; overflow test: shrink pool ⇒ coarser mips/lower-res pages, never missing shadows.
**Status: design for S2–S5; S1 partly implemented and measured.** New finding that reshapes S3/S4: ~half of a cascade's cost is *fixed per-view work* (two depth-pyramid builds + depth resolve ≈ 2.9+0.75 ms at 2048²), so page-based rendering must also avoid per-view full-resolution passes, not only geometry.
