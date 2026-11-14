---
name: bevy-gpu
description: GPU-side resource management in Bevy main — buffers (StorageBuffer, BufferVec, PersistentGpuBuffer, sparse updates), indirect draw/dispatch, GPU residency/uploads, readback, memory and sync rules. Use when moving work from CPU loops to the GPU or managing GPU memory.
---

# GPU resources & the CPU/GPU boundary

## Purpose
Design GPU-driven work: **CPU prepares compact work descriptions; GPU does the massively parallel visibility/rendering.** Not: CPU loops over thousands of objects every frame.

## When to use
New GPU buffers, culling/LOD compute, indirect draws, residency/upload code, anything that reads GPU results.

## Core concepts
- **wgpu under Bevy** (`wgpu 30.x` in `Cargo.lock`): resource lifetime = Rust ownership (`Buffer` is ref-counted); barriers are automatic per pass/usage. Feature/limit gating via `RenderDevice::features()/limits()` and `WgpuSettings`.
- **Buffer helpers** (`crates/bevy_render/src/render_resource/`): `UniformBuffer`/`DynamicUniformBuffer`, `StorageBuffer<T: ShaderType>` (std430, handles layout), `RawBufferVec<T: NoUninit>` (POD arrays, `write_buffer` growth), `BufferVec`, `UninitBufferVec` (GPU-written, no CPU data), `PartialBufferVec`, **`SparseBufferVec`** + `sparse_buffer_update.wesl` (GPU-side scatter of small updates — use for sparse dirty-instance updates instead of re-uploading whole arrays).
- **Persistent, range-allocated buffers**: `crates/bevy_pbr/src/meshlet/persistent_buffer.rs` (`PersistentGpuBuffer<T>`: `RangeAllocator`, `queue_write`, `perform_writes` via `write_buffer_with`, doubling growth + GPU `copy_buffer_to_buffer`, `mark_slice_unused`). **This is the in-tree template for GPU residency of streamed geometry.** Gap: growth only doubles, no compaction/defrag, no size cap, no eviction policy (callers free manually).
- **Mesh slab allocator**: `bevy_render/src/{slab_allocator.rs,mesh/allocator.rs}` (+`MeshAllocatorDiagnosticPlugin`) for standard meshes.
- **Indirect draw/dispatch**: `TrackedRenderPass::{draw_indirect, multi_draw_indirect, multi_draw_indirect_count}` and mesh-task variants (`render_phase/draw_state.rs`); standard path generates `IndirectParametersBuffers` in compute (`build_indirect_params.wesl`, `reset_indirect_batch_sets.wesl`). Meshlets use indirect *dispatch* args computed on GPU (`fill_counts.wesl`, `remap_1d_to_2d_dispatch.wesl` splits 1D thread counts into 2D dispatches to respect the 65535 workgroup limit).
- **Atomics & counters**: append-queues = storage buffer + `atomicAdd` counter; read counts on GPU to build indirect args (see `cull_bvh.wesl` ping-pong queues with `rightmost_slot`/`read_from_front`).
- **Readback** (`crates/bevy_render/src/gpu_readback.rs`): async `map_async`; latency ≥ 1–2 frames; use for stats/tests/streaming *feedback* (what pages did the GPU want?). Never block on it.
- **Uploads**: `RenderQueue::write_buffer` / `write_buffer_with` stage through wgpu's staging belt; cost = bytes/frame over PCIe. Budget it (see `bevy-world-streaming`: `upload_bytes_per_frame`).
- **Memory**: no built-in VRAM query/budget in wgpu; track bytes you allocate yourself (resource of `gpu_bytes_resident`). Texture memory usually dominates vegetation scenes — mips/compression (`asset-pipelines`).

## Project conventions
- Every GPU buffer has a label (debuggers: RenderDoc, Radeon GPU Profiler, Nsight).
- Size-dependent buffers use named constants + a diagnostic (`MeshletPlugin::cluster_buffer_slots` is the cautionary example: fixed, silent failure mode when too small → expose an overflow counter instead).
- GPU-written, CPU-unread buffers: `UninitBufferVec`/raw `Buffer` with `STORAGE | INDIRECT`. Don't mirror them on the CPU.
- Feedback loop for residency: GPU writes "requested page" bits → async readback → CPU streams → write pages → *next-next* frame uses them. Always render with the coarse fallback meanwhile.

## Implementation patterns
- Cull on GPU: input = instance array (POD, static, retained) + per-view uniform; output = compacted visible index buffer + indirect args; draw = one `multi_draw_indirect_count`.
- Dirty updates: CPU collects `(index, value)` pairs → upload small buffer → scatter compute (`sparse_buffer_update`).
- Double-buffer anything the CPU rewrites each frame that the GPU may still be reading (wgpu does this implicitly for `write_buffer`, not for mapped-at-creation reuse).

## Common mistakes
- Readback inside the frame to decide CPU-side what to draw (stall).
- Fixed-size GPU queues without overflow detection.
- Creating buffers per frame; growing a persistent buffer every frame (thrash).
- Forgetting COPY_SRC/COPY_DST/INDIRECT usages.
- Assuming mesh shaders/64-bit atomics exist (probe features).

## Performance considerations
Throughput = occupancy × memory bandwidth. Keep per-instance data ≤ 64 B for culling input; quantize (`u16` extents, packed rotation) — meshlet format shows bit-packing. Avoid divergent branches in culling shaders; use subgroup ops where `SUBGROUP` is supported.

## Testing requirements
Compute shaders: test via a headless `RenderDevice` (skip if no adapter) comparing against a CPU reference implementation on random seeded input. Buffer managers: pure-CPU allocator tests (range allocator invariants, no overlap, free/reuse).

## References
`crates/bevy_render/src/render_resource/*`, `crates/bevy_pbr/src/meshlet/{persistent_buffer.rs,resource_manager.rs}`, `crates/bevy_render/src/{gpu_readback.rs,slab_allocator.rs}`, `docs/debugging.md`
