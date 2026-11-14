# Virtualized geometry architecture: UE problem → algorithm → Bevy design

Evidence tags as in `ue_bevy_gpu_feature_matrix.md`.

## 0. Decision: extend the existing meshlet renderer (Part 3)

**[Bevy]** `bevy_pbr::meshlet` already is a Nanite-class design: offline cluster DAG with locked-border simplification, a BVH8 over cluster groups, per-view GPU traversal (instance → BVH levels → clusters) with frustum + two-phase HZB occlusion + screen-space-error LOD, SW/HW rasterization into a 64-bit visibility buffer, material resolve passes. **Decision: it is the foundation; no second meshlet system is created.** Everything below is an extension or a layer around it.

Reasons not to replace: (a) it is the only implementation of cluster LOD/culling in the tree; (b) its data structures (`BvhNode`, `Meshlet`, `MeshletCullData`) are `Pod`, versioned (`MESHLET_MESH_ASSET_VERSION = 3`); (c) replacement would discard ongoing upstream fixes (#26467, #25928, #26240, #26472).
Where architectural change is unavoidable (page residency, assemblies), it is **additive**: new node/page types behind new asset versions, loader keeps reading v3.

## 1. Problem → UE solution → algorithm → Bevy approach

| Problem | UE solution | Underlying algorithm | Bevy approach |
|---|---|---|---|
| Per-object LODs don't scale | Cluster DAG + error-driven cut | simplification with group borders locked (cracks impossible), monotone error, cut by projected error | **Have it** (`from_mesh.rs`, `lod_error_is_imperceptible`) |
| CPU can't loop 100k+ objects | GPU traversal | hierarchical culling in compute with persistent/indirect work queues | **Have it** for meshlets; standard path has GPU preprocessing. Missing: *retained* GPU scene for meshlets (`gpu_scene.md`) |
| Overdraw & hidden geometry | Two-phase HZB | draw last-frame-visible, build HZB, test rest | **Have it** |
| Small triangles slow HW raster | SW raster in compute | per-triangle atomic min on packed depth|id | **Have it** (u64 atomics) |
| Source asset ≫ VRAM | Page streaming + root residency | GPU requests pages, CPU streams, coarse root always resident | **Missing**; design in `geometry_streaming.md` |
| Dense foliage duplicates | Assemblies | per-part transform arrays resolved in traversal | **Missing**; `foliage.md` |
| Thin geometry vanishes at LOD | Preserve Area | area-preserving simplification | **Missing**; experiment in `foliage.md` |
| Wind via WPO breaks bounds | Skinning for foliage | bones + animated bounds | **Missing**; `foliage.md` §GPU animation |
| Shadows redraw everything per cascade | Virtual shadow maps | page table + caches + invalidation | **Missing**; measured motivation (80 % of meshlet frame) in `performance.md` |
| Backend without features | Fallback mesh | conventional LOD chain | **Missing** as asset pairing (`roadmap.md` Phase 9) |

## 2. Layering (what each plugin owns) — Part 38

```
VirtualGeometryPlugin   asset format (v3 loader + new pages), cluster build config/stats        ← bevy_pbr::meshlet (extend)
GpuScenePlugin          retained instance/bounds/material/prev-transform tables                 ← new, shared by main/shadow/RT
GpuCullingPlugin        instance classification + hierarchy traversal (existing meshlet shaders)
GpuOcclusionPlugin      HZB (existing mip_generation::experimental::depth)  — reusable by standard path + meshlets
GeometryStreamingPlugin page table, requests, residency, budgets (policy exists in bevy_world_streaming::pages)
VirtualShadowMapPlugin  page demand, physical pool, cache/invalidate                            ← new
VirtualTexturePlugin    page table + feedback                                                  ← coordinate with issue #25986
GpuProfilerPlugin       RenderDiagnosticsPlugin spans + cluster counters + overlay
```
Existing `MeshletPlugin` remains the entry point and gains the above incrementally; none of these is a single giant plugin. All follow the main→extract→render-world→prepare→queue→`Core3d` systems architecture (`bevy-rendering`, `bevy-render-graph` skills).

## 3. Invariants every phase must keep
1. **No GPU→CPU readback in the frame that consumes it.** Feedback (residency, overflow counters) is read ≥1–2 frames later via `gpu_readback`.
2. **Conservative culling**: a test is allowed to keep invisible geometry; never to drop visible geometry. Each new culling rule ships with a brute-force comparison test.
3. **Graceful degradation**: overflow/pressure lowers detail (error scale ↑), never produces missing geometry (see `gpu_culling.md` §Overflow).
4. **Capability-driven paths**: no feature is a hard requirement of the whole renderer (`backend_capabilities.md`).
5. **Measure first**: a phase merges only with a benchmark row in `performance.md`.

## 4. What was done in this change set vs designed only
Done and measured: `MeshletBuildConfig`/`from_mesh_with_config`, `MeshletMesh::stats()` (+tests), cluster-size sweep, shadow-cost sweep and per-stage shadow spans, deterministic baking (METIS lock), non-fatal capability check (`MeshletSupport`), `MeshletSettings` (redundant shadow pyramid rebuild removed: −15.8 % meshlet frame; shadow LOD scale / per-cascade step knobs, shipped off because they cost far-shadow coverage — see `performance.md` §11), perspective-LOD instance-scale fix, **queue overflow detection + automatic LOD degrade** (`MeshletPressure`), **retained/change-driven meshlet instance table** (`MeshletSettings::retained_instances`, opt-in; static −28…−65 % frame time, `performance.md` §12). Everything else in these documents is **design**; none of the big subsystems (assemblies, voxels, virtual shadow maps, virtual textures, page streaming in the renderer, RT clusters, terrain, spline, skeletal virtual geometry) is implemented.
