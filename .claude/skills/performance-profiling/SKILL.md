---
name: performance-profiling
description: Whole-game profiling workflow for Bevy — which metrics to capture (FPS, CPU/GPU ms, draw calls, meshlets, triangles, memory, streaming/upload bandwidth, overdraw, shadow cost), tools on Linux, and a bottleneck-classification procedure to run BEFORE optimizing.
---

# Performance profiling (Bevy games)

## Purpose
Classify the bottleneck first. Optimizing the wrong stage is the most common failure.

## Required measurements (capture all, per scenario, per camera path)
`FPS` · `CPU frame ms` (main + render-world, per-system via spans) · `GPU frame ms` (per pass) · `draw calls` · `visible/culled/occluded instances or meshlets` · `triangles` · `GPU memory` · `CPU memory` · `streaming IO bandwidth` · `GPU upload bandwidth` · `overdraw` · `shadow cost` (frame with/without shadows) · **frame-time p50/p95/p99 and max** (stutter lives in the tail).

## Bottleneck classification procedure
Run in order; each step is a controlled A/B on the same deterministic camera path (`game-testing`):
1. **CPU vs GPU**: if GPU frame ms ≈ frame ms ⇒ GPU-bound; if main/render CPU thread ms ≈ frame ms and GPU idle gaps exist ⇒ CPU-bound. (GPU timing per pass: `RenderDiagnosticsPlugin`, see `GPU-profiling`.)
2. **If CPU-bound**: which schedule? `trace_tracy`/`trace_chrome` spans: `PostUpdate` visibility? extract? prepare/queue? gameplay? streaming `activate`? Lots of time in `check_visibility*`/`extract` ⇒ per-entity scaling problem ⇒ aggregate/GPU-drive (`bevy-visibility`).
3. **If GPU-bound — split**:
   - *Resolution test*: render at 50% scale. Time drops ≫ ⇒ **fragment/raster/overdraw**; unchanged ⇒ **geometry/vertex** or compute.
   - *Shadow test*: disable shadow casting. Big drop ⇒ **shadow cost**.
   - *Material test*: swap to a trivial unlit material. Big drop ⇒ **fragment shading/material/texture bandwidth**.
   - *Geometry test*: swap meshes for LOD-lowest. Big drop ⇒ **geometry**.
   - *Texture test*: force mip bias +2. Big drop ⇒ **texture bandwidth**.
   - *Culling test*: freeze culling camera/disable occlusion. Compare compute vs raster time per pass.
4. **Memory bandwidth**: vendor counters (RGP/Nsight) or inferring from texture/mip tests.
5. **Streaming**: hitches correlated with `StreamingStats` (pending, upload bytes/frame, hard misses) ⇒ streaming; correlated with allocation (CPU) ⇒ spawn/despawn spikes.
6. **Synchronization**: gaps between CPU submit and GPU start, `map_async`/readback waits, pipeline-compilation stalls (first-use frames; check `PipelineCache` logs).

Output a one-line verdict: `bound = {CPU | GPU-geometry | GPU-raster/overdraw | GPU-fragment | memory-bandwidth | streaming | sync}` with the evidence numbers, *then* choose optimizations (`rust-performance` report format).

## Tools on this platform (Linux; this repo)
- Bevy spans: `cargo run --release --features trace_tracy` (Tracy GUI) or `trace_chrome` → `chrome://tracing`/Perfetto; see `docs/profiling.md`.
- Built-in diagnostics: `FrameTimeDiagnosticsPlugin`, `EntityCountDiagnosticsPlugin`, `bevy_dev_tools::fps_overlay`/`diagnostics_overlay`, `RenderDiagnosticsPlugin` (GPU timestamps per pass on Vulkan/DX12), `MeshAllocatorDiagnosticPlugin`, `RenderAssetDiagnosticPlugin`.
- CPU: `perf record -g`, flamegraphs (`rust-performance`).
- GPU: RenderDoc (capture + overdraw/pipeline stats), AMD Radeon GPU Profiler / NVIDIA Nsight Graphics where hardware allows; `MESA_VK_*`/`RADV_*` env for RADV; `vulkaninfo --summary` to record the device in every report.
- Memory: `heaptrack`; GPU memory — track own allocations (`bevy-gpu`); `VK_EXT_memory_budget` via vendor tools.

## Worked example (this repo)
`docs/open_world/benchmarks.md` follows this procedure: forest baseline 78 ms/frame → resolution/material/shadow A/B showed main-pass geometry+overdraw dominated (57.8 ms of 58 ms GPU spans) with shadows adding 22 ms wall-clock → LOD → HLOD → occlusion measured *separately*, with the surprise that occlusion alone regressed.

## Project conventions
- Every report starts with an **environment block**: commit hash, `rustc -V`, build profile, GPU/driver (`vulkaninfo --summary`), resolution, present mode (disable vsync: `PresentMode::AutoNoVsync`), scene + camera path name + seed.
- Always release builds; warm up 120+ frames; ≥ 1000 measured frames for p99.
- State clearly when a metric was **not measurable on the available hardware** (e.g. no GPU timestamps on llvmpipe, no meshlets on Vega) — never fill with estimates.

## Common mistakes
- Measuring with vsync on or in debug builds; mixing software rasterizers (llvmpipe) into GPU conclusions.
- Reporting average only. Comparing different camera paths.
- Concluding "GPU-bound" from low FPS without checking CPU thread times.

## Testing requirements
Profiling scenarios are deterministic scenes with scripted camera paths and a CSV frame log (`game-testing`).

## References
`docs/profiling.md`, `docs/debugging.md`, `crates/bevy_diagnostic`, `crates/bevy_dev_tools/src/{fps_overlay.rs,diagnostics_overlay.rs,render_debug.rs}`, `crates/bevy_render/src/diagnostic`
