# Ray tracing with virtual geometry

## UE reference [UE-doc]
`r.RayTracing.Nanite.Mode 2` uses **cluster acceleration structures (CLAS)** when GPU/driver support cluster ops; otherwise ray tracing uses fallback meshes. CLAS pool default 512 MB (max 2048 MB); demand-driven; quality scaling lowers detail when usage > 85 % and raises it below 70 % (min scale 0.3); stats via `stat NaniteRayTracing`. Fallback meshes are also used for Lumen HW reflections.

## Bevy facts [Bevy] / wgpu [wgpu-docs 30.0.2]
* `bevy_solari`: realtime/path-traced lighting; `scene/blas.rs` builds **one BLAS per mesh** (`BufferUsages::BLAS_INPUT`, compaction budgeted per frame by vertex count) using wgpu `EXPERIMENTAL_RAY_QUERY` (Vulkan, DX12, Metal per wgpu docs).
* **wgpu 30.0.2 exposes no cluster-acceleration-structure feature** (no `cluster` symbol in `wgpu-types` features). So UE's CLAS path is **BACKEND LIMITED / not expressible** today.
* Meshlet geometry is not ray-traceable (visibility-buffer only; no BLAS from clusters).

## Design [Proposal]
Realistic stack: **RT over fallback meshes** with demand-driven residency:
`fallback mesh LOD chain (asset pair, matrix #13) → per-instance RT LOD from projected size/ray importance → BLAS pool with byte budget (LRU + quality scale: pick coarser fallback LOD when pool > 85 %, finer < 70 %) → TLAS from GPU scene`.
The BLAS-per-LOD-per-mesh scheme reuses Solari's `BlasManager` (compaction already budgeted). When wgpu gains cluster ops, replace per-mesh BLAS by cluster-level structures keyed to the same demand signal.
Shared data (Part 34): fallback meshes and the GPU-scene instance table are the shared representation; main-view meshlets, shadows and RT differ only in view-specific LOD/residency.
**Status: design only; no RT code in this change.** Risk: fallback generation quality for thin foliage (see Preserve Area); RT of 500k trees via fallback is likely infeasible without an assembly-aware BLAS strategy (**research required**).
