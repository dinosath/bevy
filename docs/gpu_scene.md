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

## Expected benefit / risk
Removes O(instances) CPU extract+upload for static scenes (forests, cities): **[Assumption]** the meshlet path's CPU cost at 12 k instances is not currently the bottleneck (frame was 5.4 ms with no shadows), so the payoff appears at ≥10⁵–10⁶ instances. First step is a **benchmark at 100 k/500 k instances** to find where CPU `extract_meshlet_mesh_entities` crosses the GPU time — not done yet.

## Tests
Dirty-update equivalence (scatter result == full rebuild), removal/reuse of slots, determinism of ids, no per-frame allocation in steady state (counting allocator).
