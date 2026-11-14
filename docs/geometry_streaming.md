# Geometry streaming and residency

## Bevy facts [Bevy]
* Asset granularity is the whole `MeshletMesh`: `MeshletMeshManager::queue_upload_if_needed` writes seven `PersistentGpuBuffer`s (positions, normals, uvs, indices, BVH nodes, meshlets, cull data); `remove` marks ranges unused (`meshlet_mesh_manager.rs`). `PersistentGpuBuffer` = `RangeAllocator` + doubling growth + `queue_write` / `perform_writes` (unbudgeted: all pending writes in one frame).
* After queueing, the CPU asset copy is dropped (`assets.remove_untracked`) — good for RAM, bad for re-streaming (no re-load path once evicted unless the handle's asset is reloaded).
* No GPU→CPU demand feedback, no page table, no root-residency, no budget/eviction. `RenderAssetBytesPerFrame` throttles standard `RenderAsset`s only.
* Existing in-repo policy layer (previous work): `bevy_world_streaming::pages` (`AssetPages`, prefix residency, `PageTable::reconcile` ≈ 99 µs for 2 000 assets [Bench]) and the cell scheduler. These decide *what*; the renderer integration (what this doc designs) is **not built**.

## UE reference [UE-doc]
Streaming pool size cvar (`r.Nanite.Streaming.StreamingPoolSize`), too small ⇒ cache thrashing prevents settling; `Minimum Residency (Root Geometry)` bytes; "Streaming Geometry" visualization; UE sample average 14.4 B/triangle.

## Design [Proposal]
```
GPU traversal ─(node needs child whose page is absent)→ request bit in `page_requests` (atomicOr, deduped)
        │                                   └─ traversal stops at the parent's *resident* group: draws parent LOD (never a hole)
        ▼ (async readback, 1–2 frames)
CPU: priority = f(screen error not satisfied, distance, view importance) ─→ IO ─→ decode ─→ upload (budgeted) ─→ page-table update (GPU buffer)
```
Page layout: asset = `root page (BVH top levels + coarsest clusters, always resident)` + data pages (cluster groups, ordered coarse→fine; ≈64–256 KiB). The BVH child pointer gains `page_id`; absent page ⇒ "treat as leaf" (use parent group). This reuses the existing error-driven cut: *parent_error* already makes the parent a valid representation.
Residency manager (`GeometryResidency` resource): per page `{state: Absent|Requested|Loading|Uploading|Resident, last_used_frame, priority, bytes}`; pool = range allocator with **fixed byte budget**; eviction = LRU with priority and *protection of root pages and pages used this/last frame*; thrash guard: don't evict a page loaded < N frames ago unless over hard limit; upload throttle = bytes/frame (like `upload_bytes_per_frame` in `StreamingConfig`).
Prefetch/prediction: reuse `bevy_world_streaming::predict` (camera/vehicle velocity → predicted frustum → pre-request pages whose cluster error would be unsatisfied there).
Quality ladder under pressure: raise per-view `error_scale` → evict fine pages first → lower streaming priority of low-importance views (shadow/RT) before main view.
Compression (Part 16): UVs 16-bit unorm per cluster (−4 B/vertex → ~12 % smaller raw), tangent-frame from octahedral normal + sign (none stored today), triangle strips/edge-codec for indices (1 B/index today), then entropy stage. **Optimize frame time, not only disk size**: add a CPU/GPU decode cost column when measured. Baseline today: 34.5 raw / ~21.8 lz4 B per source triangle vs UE 14.4 [Bench][UE-doc].

## Tests / benchmarks (Part 40 Scene F)
Deterministic fast fly-through + teleport with a *simulated* IO model (existing `sim` harness extended to pages): hard-miss count must be 0 (parent fallback), cache hit rate, IO MB/s, upload MB/frame, P99 frame time. GPU test: force page absence, assert image == coarse LOD render.
**Status: design only.**
