---
name: GPU-profiling
description: GPU timing and debugging for Bevy — per-pass timestamps via RenderDiagnosticsPlugin and RecordDiagnostics, pipeline statistics, readback counters, RenderDoc/RGP, overdraw, and platform limits. Use when you need GPU numbers or need to debug GPU culling/visibility.
---

# GPU profiling

## Purpose
Get trustworthy per-pass GPU numbers and visibility counters, and know when they're unavailable.

## Built-in per-pass timing
- Add `RenderDiagnosticsPlugin` (`crates/bevy_render/src/diagnostic/mod.rs`). On Vulkan/DX12 it records **timestamp queries and pipeline statistics**; other backends: CPU time only. Requires wgpu `TIMESTAMP_QUERY` (+ `TIMESTAMP_QUERY_INSIDE_PASSES/ENCODERS` for in-pass spans) — see `diagnostic/internal.rs`. Results surface as `bevy_diagnostic` `Diagnostic`s (print with `LogDiagnosticsPlugin`).
- Instrument your own pass:
  ```rust
  let diagnostics = ctx.diagnostic_recorder();           // no-op if plugin absent
  let diagnostics = diagnostics.as_deref();
  let span = diagnostics.time_span(ctx.command_encoder(), "my_cull");
  /* … record work … */
  span.end(ctx.command_encoder());
  ```
  (pattern used by `meshlet_visibility_buffer_raster`, spans `"meshlet_visibility_buffer_raster"` and per-shadow-view names).
- Tracy GPU zones: cargo feature `tracing-tracy` path (`diagnostic/tracy_gpu.rs`).

## Counters you can read back
- Standard path: `IndirectParametersBuffers` (draw counts after GPU culling) — copy/map pattern in `examples/3d/occlusion_culling.rs` (`ReadbackIndirectParametersPlugin`): visible vs. culled instance counts, 1–2 frames latent.
- Meshlets: no public visible-cluster counter today — add one with a GPU atomic counter + `gpu_readback` (project TODO; counts needed: visible/culled/occluded clusters).
- Never read back synchronously.

## External tools
RenderDoc (Vulkan capture; overdraw/Quad overdraw, pipeline state, buffer inspection), Radeon GPU Profiler (RADV needs `MESA` instrumentation / RDNA hardware), Nsight Graphics. `docs/profiling.md` ("GPU runtime" → vendor tools).

## Gotchas observed in practice
- `RenderDiagnosticsPlugin` span coverage is incomplete on the standard path: shadow cascade spans only showed up for the meshlet path (`shadow_directional_light_N_cascade_M`), so the GPU-span sum under-reported a ~22 ms shadow cost that wall-clock A/B (`--no-shadows`) exposed. Always cross-check spans against an A/B run.
- Span paths look like `render/<pass>/elapsed_gpu` in `DiagnosticsStore`; iterate `store.iter()` and filter on the suffix (see `examples/large_scenes/forest_benchmark`).

## GPU debugging
- Label every pass/buffer/texture; wgpu debug groups (`push_debug_group`) as used in meshlet pass.
- Validation: run debug builds with wgpu validation; for GPU hangs bound all loops, check indirect arg buffers (counts ≤ capacity).
- Visualize: debug materials (see `examples/3d/meshlet.rs` `MeshletDebugMaterial`), `bevy_dev_tools::render_debug`, project overlays (`StreamingStats`, gizmos).

## Project conventions
- Record device (`vulkaninfo --summary`) and whether timestamps were available. **This repo's dev machine: AMD Radeon Vega (RADV RENOIR) + llvmpipe** — see `docs/open_world/bevy_audit.md` for implications (timestamps incl. in-pass available; meshlet features **are** available — measured in `docs/open_world/benchmarks.md`; no `VK_EXT_mesh_shader`). Check with `RenderDevice::features()` or `RUST_LOG=bevy_render=debug`..
- GPU numbers from llvmpipe are meaningless for performance — only usable for correctness.

## Common mistakes
Reading GPU time from CPU frame time; trusting timestamp spans when `TIMESTAMP_QUERY_INSIDE_PASSES` is missing (spans cover whole encoders); comparing across drivers.

## Performance considerations
Timestamps themselves are cheap; pipeline-statistics queries are heavier — enable selectively.

## Testing requirements
Instrumented passes must work with the plugin absent (no-op) and present; add a smoke test that spans appear when the feature is supported, skip otherwise.

## References
`crates/bevy_render/src/diagnostic/*`, `crates/bevy_pbr/src/meshlet/visibility_buffer_raster_node.rs`, `examples/3d/occlusion_culling.rs`, `docs/profiling.md`
