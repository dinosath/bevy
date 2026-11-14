---
name: bevy-render-graph
description: How render passes are scheduled on Bevy main — the old RenderGraph was replaced by per-camera system schedules (Core3d/Core2d with Core3dSystems sets, ViewQuery, RenderContext). Use when adding/reordering passes or compute work.
---

# Render passes on Bevy main ("render graph as systems")

## Purpose
Add, order and debug GPU passes. **Do not write `impl Node for …` / `RenderGraph::add_node` code** — advice from older tutorials no longer applies. Release note: "render graph as systems" (git: `fdee26bcb`, `14d366b9b` "Fix parallel render graph").

## When to use
Adding a compute pass (culling, Hi‑Z, streaming upload), a prepass, a post-process, or reordering existing passes.

## Core concepts (verified in source)
- Hierarchy (`crates/bevy_render/src/renderer/mod.rs` docs): `render_system` → `RenderGraph` (**now a root `Schedule`**, sets `RenderGraphSystems::{Begin, Render, Submit, Finish}`) → `camera_driver` system → per-camera pipeline schedules `Core2d` / `Core3d` / your own `CUSTOM_PIPELINE` schedule. You can add an entire custom pipeline as its own schedule label.
- Schedule labels `Core3d` / `Core2d` (`crates/bevy_core_pipeline/src/schedule.rs`). They run **once per camera/view**, in the `Render` phase of the render world.
- Ordered sets: `Core3dSystems::{Prepass, MainPass, EarlyPostProcess, PostProcess}` chained weakly. Add your pass with `.in_set(Core3dSystems::…)` and `.before/.after(other_pass_fn)`.
- A pass is a normal system taking:
  - `ViewQuery<(&ExtractedCamera, &ViewUniformOffset, …)>` – per-view components (`crates/bevy_render/src/renderer/render_context.rs`), skipping the system if the view lacks them,
  - `RenderContext` – command encoder/pass helpers + diagnostics (`ctx`),
  - `Res<…>`/`Query<…>` of render-world data, `&World` if needed.
- Real example to copy: `meshlet_visibility_buffer_raster` and registration in `crates/bevy_pbr/src/meshlet/mod.rs`:
  ```rust
  .add_systems(Core3d, (
      meshlet_visibility_buffer_raster.before(per_view_shadow_pass::<EARLY_SHADOW_PASS>),
      meshlet_prepass.after(per_view_shadow_pass::<EARLY_SHADOW_PASS>).in_set(Core3dSystems::Prepass),
      meshlet_main_opaque_pass.before(main_opaque_pass_3d).in_set(Core3dSystems::MainPass),
  ))
  ```
- Core passes to order against: `main_opaque_pass_3d`, `per_view_shadow_pass::<EARLY_SHADOW_PASS>` (early/late shadow passes exist for two-phase culling), prepass systems in `crates/bevy_core_pipeline/src/prepass`.
- Mesh-shader example registers a custom pass the same way: `examples/shader_advanced/mesh_shader_intro.rs` (uses `main_opaque_pass_3d`, `Core3dSystems`, `CORE_3D_DEPTH_FORMAT`).
- Custom per-pass GPU timing: `RecordDiagnostics` from `bevy_render::diagnostic` (see `GPU-profiling`).

## Project conventions
- One pass = one system = one file; name `*_pass`. Pass code only *records*; all buffers/bind groups are prepared in `Prepare*` sets earlier.
- Per-view state lives on the **view entity** (e.g. `MeshletViewResources`), created in `PrepareResources`/`PrepareViews`.
- Prefer one big compute dispatch with indirect args over many small passes (see "TODO: Reuse compute/render passes" in `visibility_buffer_raster_node.rs` — passes are expensive).

## Implementation patterns
```rust
fn my_cull_pass(
    view: ViewQuery<(&ViewUniformOffset, &MyViewBindGroups)>,
    pipelines: Res<MyPipelines>, cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let Some(pipeline) = cache.get_compute_pipeline(pipelines.cull) else { return }; // not compiled yet: skip, don't block
    let (view_off, groups) = view.into_inner();
    let mut pass = ctx.command_encoder().begin_compute_pass(&Default::default());
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &groups.cull, &[view_off.offset]);
    pass.dispatch_workgroups_indirect(&groups.dispatch_args, 0);
}
```
(Check exact `RenderContext`/`ViewQuery` signatures in source before copying — they are evolving.)

## Common mistakes
- Looking for the node API (`impl Node`, `ViewNode`, `add_node`, slot edges) — gone. `RenderGraph` now names only the root schedule.
- Ordering a pass only relative to a *set* when you need it relative to a *specific* system (shadow passes!).
- Doing allocation or pipeline lookups that can fail with `unwrap()` inside the pass.
- Adding a CPU readback inside a pass to decide what to dispatch (use indirect dispatch).

## Performance considerations
Each pass = encoder work + barriers; wgpu inserts barriers automatically between passes that use the same resource. Merge compute work; use indirect dispatch/draw counts written by earlier compute (`remap_1d_to_2d_dispatch.wesl`, `fill_counts.wesl` in meshlet show the patterns).

## Testing requirements
Visual/headless screenshot test of any new pass; verify pass order in a debug build by checking GPU timing span names.

## References
`crates/bevy_core_pipeline/src/schedule.rs`, `crates/bevy_core_pipeline/src/core_3d/mod.rs`, `crates/bevy_pbr/src/meshlet/{mod.rs,visibility_buffer_raster_node.rs,material_shade_nodes.rs}`, `examples/shader_advanced/*`
