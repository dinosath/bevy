# GPU scene

## Bevy facts [Bevy]
* **Standard path** retains instance data on the GPU: `gpu_preprocessing.rs` (`BatchedInstanceBuffers`, `GpuPreprocessingMode::{None, PreprocessingOnly, Culling}`), `SparseBufferVec`/`AtomicSparseBufferVec` + `sparse_buffer_update.wesl` scatter small dirty updates. Open issue **#26257**: `gpu_preprocess` recreates the bin-unpacking/uniform-allocation bind groups every frame. Open bug #24730 touches `AtomicSparseBufferVec` dirty-page tracking.
* **Meshlet path does not retain**: `InstanceManager::reset()` clears all per-instance arrays and `extract_meshlet_mesh_entities` rebuilds them from every `MeshletMesh3d` entity each frame (`instance_manager.rs`). Per instance it holds `MeshUniform`, a model-space AABB, material id, BVH root index, plus a per-**view** per-instance visibility bit buffer (RenderLayers / `NotShadowCaster`). So meshlet CPU cost ∝ instance count and shadow views each carry their own instance visibility vector.
* Materials: bindless slabs (`material_bind_groups.rs`, `render_resource/bindless.rs`); the meshlet path resolves materials per-pass via `instance_material_ids`.
* Ray tracing (`bevy_solari`) keeps its own BLAS/instance tables (`scene/blas.rs`, `scene/extract.rs`).

## UE reference
[UE-doc] instance cap 16 M; assemblies avoid "heavy GPU scene buffer overhead" of standard instancing, each visible instance needs ≥1 root cluster drawn. [UE-bg] GPU Scene retains primitive/instance data and applies deltas.

## Proposed design [Proposal]
One retained table set, owned by a `GpuScenePlugin`, consumed by main view, shadow views, occlusion passes and RT:

| Table | Contents | Update |
|---|---|---|
| `instances[]` | world-from-local (affine 3×4 f32, **camera-relative offset** for large worlds), prev world-from-local, `bounds_id`, `mesh_id` (BVH root), `material_id`, `class` bits (STATIC / MOVING / ANIMATED / SKINNED / SPLINE / ASSEMBLY / DISPLACED), `flags` (shadow caster, render layers mask), `streaming_state` | sparse scatter of dirty indices (existing `SparseBufferVec`) |
| `bounds[]` | local AABB / sphere (shared by identical meshes) | on mesh load |
| `meshes[]` | BVH root, page-table base, root-page range, fallback mesh id | on asset load |
| `materials[]` | bindless material indices | existing slab allocator |
| per-view `visibility` | *not* a per-instance bit-vector: culling writes visible *clusters*; instance-level masks computed in the instance-cull shader from `flags` + view layers | per view, GPU only |

Rules: (1) CPU never iterates all instances per frame — extraction is **change-driven** (`Added/Changed<GlobalTransform>`, removals); (2) the `class` bits are the hook for the simple/complex split (matrix #5) — introduced **only when a second class exists**; (3) previous-transform stored GPU-side (temporal passes, motion vectors, VSM invalidation “moved this frame”); (4) one table feeds every view type.

## Measured evidence (Phase 2) [Bench] — details in `performance.md` §10
* Meshlet scenes become **CPU-bound at ≈12 k instances**; non-GPU gap ≈ 0.115 ms per 1 000 instances (100 k: 57 % of the frame; 200 k: 65 %, with the GPU doing a 4.68 B-triangle scene in 12.5 ms).
* Per frame at 100 k: `extract_meshlet_mesh_entities` 7.0 ms (serial, blocks main), `prepare_meshlet_per_frame_resources` 4.5, `queue_material_meshlet_meshes` 1.3, `prepare_material_meshlet_meshes_*` 1.2.
* Contained fixes: asset cache + no per-instance tuple clone in the extractor −1.26 ms (−18 %, kept); view-visibility loop tweaks no gain; `NoCpuCulling` no effect.

## Design constraint discovered while trying a static fast path [Bevy]
"Skip all work when nothing changed" **does not work** for any scene with a moving camera:
1. `RenderMaterialInstances` is mutated through `ResMut` every frame (`current_change_tick` bump), so `is_changed()` carries no information.
2. `extract_mesh_materials` registers an entity's material only when its `ViewVisibility` is `true` (and re-runs it on `Changed<ViewVisibility>`), so CPU frustum culling continually inserts/removes meshlet instances' material entries as the camera moves.
3. `queue_material_meshlet_meshes` and `prepare_material_meshlet_meshes_*` then rebuild per-frame material ids from that table.
Therefore a *correct* retained scene must (a) hold instance rows in stable slots with a free list, driven by `Added/Changed/Removed` on `MeshletMesh3d`, `GlobalTransform`, `PreviousGlobalTransform`, `RenderLayers`, `NotShadowCaster/Receiver` and `AssetEvent<MeshletMesh>`; (b) **decouple meshlet material registration from `ViewVisibility`** (meshlets are GPU-culled; CPU visibility is irrelevant to them) and key material ids by material asset rather than rebuilding them per frame; (c) upload dirty slot ranges only (the existing `SparseBufferVec` scatter, `bevy-gpu` skill), including a per-view bit-vector that is rebuilt only when layers/shadow flags change. Estimated ceiling: removes ≈ 11–13 of the ≈14 ms CPU path at 100 k instances (frame ≈ 9 ms, then GPU-bound) — **an estimate from the span breakdown, not a measured result**; it is the largest remaining item for large instance counts.

## Expected benefit / risk
Removes O(instances) CPU extract+upload for static scenes (forests, cities): **[Assumption]** the meshlet path's CPU cost at 12 k instances is not currently the bottleneck (frame was 5.4 ms with no shadows), so the payoff appears at ≥10⁵–10⁶ instances. First step is a **benchmark at 100 k/500 k instances** to find where CPU `extract_meshlet_mesh_entities` crosses the GPU time — not done yet.

## Tests
Dirty-update equivalence (scatter result == full rebuild), removal/reuse of slots, determinism of ids, no per-frame allocation in steady state (counting allocator).
