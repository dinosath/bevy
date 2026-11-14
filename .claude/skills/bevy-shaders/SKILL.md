---
name: bevy-shaders
description: Writing shaders for Bevy main — WESL/WGSL, imports, conditional compilation, compute (culling, Hi-Z), mesh/task shaders, atomics, immediates, debugging. Use when adding or editing .wesl/.wgsl files.
---

# Shaders in Bevy main

## Purpose
Write correct, fast shaders that fit Bevy's shader pipeline. **This checkout uses WESL (`*.wesl`), a WGSL superset, not naga_oil `#import`/`#ifdef`.** Check `crates/bevy_shader/Cargo.toml` (`wesl = "0.6"`, feature `naga-ext`).

## When to use
Any `.wesl`/`.wgsl` change, new compute pass, material shader, Hi‑Z/culling work.

## Core concepts
- **Imports**: `import package::meshlet::bindings::{ get_aabb, constants, … };` (see `cull_bvh.wesl`). Register library shaders with `load_shader_library!(app, "x.wesl")` (keeps handle alive); entry-point shaders with `embedded_asset!` + `load_embedded_asset!`.
- **Conditional compilation**: `@if(FLAG) … @elif(OTHER) … @else … ` on declarations/statements (e.g. `@if(MESHLET_FIRST_CULLING_PASS)` in `cull_bvh.wesl`; binding variants in `downsample_depth.wesl`). Flags come from pipeline specialization `shader_defs`; specialize instead of branching at runtime when it changes bindings/types.
- **Compute entry**: `@compute @workgroup_size(N,1,1) fn name(@builtin(global_invocation_id) id: vec3<u32>)`. Choose N as a multiple of the subgroup size (64 on AMD wave64 / 32 on NVIDIA); `cull_bvh` uses 128 = 16 nodes × 8 children.
- **Immediates** (`var<immediate> constants: Constants;`, feature `WgpuFeatures::IMMEDIATES`) replace push constants for tiny per-dispatch data (see `downsample_depth.wesl`).
- **64-bit atomics on textures** (`texture_storage_2d<r64uint, …>`, `u64`) power the meshlet visibility buffer (`visibility_buffer_software_raster.wesl`: `(u64(depth) << 32) | ids`). Requires `TEXTURE_INT64_ATOMIC`, `SHADER_INT64`.
- **Subgroups**: `SUBGROUP` feature; use for reductions/ballots in culling and Hi‑Z (the SPD-based depth downsample notes "TODO: Subgroup support").
- **Mesh/task shaders**: `examples/shader_advanced/mesh_shader_intro.rs` + sibling shader files; pipeline = `MeshPipelineDescriptor { task, mesh, fragment }`; payload ≲ 236 B recommended (doc comment in the example). WIP, `EXPERIMENTAL_MESH_SHADER`.
- **Pipeline cache is async**: shader/pipeline errors surface as log errors later; always check logs when a pass silently doesn't run.
- **Materials**: fragment/vertex shaders reference Bevy's `bevy_pbr` imports (`pbr_functions.wesl`, `mesh_functions.wesl`, `forward_io.wesl`); visibility-range dithering fade lives in `pbr_functions.wesl`.

## Project conventions
- One shader file per entry point group; shared helpers in `*_shared.wesl` / `bindings.wesl` library files (meshlet is the model).
- Struct layouts mirror Rust `ShaderType`/`Pod` types exactly; add Rust-side `size_of` asserts and a comment pointing to the WESL struct.
- Name queues/counters with clear in/out intent (`*_read`, `*_write`); document ping-pong conventions at the top.
- No data-dependent loops without a hard iteration bound (GPU hang safety).

## Implementation patterns
- **Frustum cull** (AABB vs 6 planes) and **LOD error → pixels**: copy `cull_shared.wesl` (`aabb_in_frustum`, `lod_error_is_imperceptible`).
- **Hi‑Z occlusion**: build mip chain of max depth (reversed-Z aware!) — `mip_generation/experimental/downsample_depth.wesl`; test AABB's screen rect at the mip where the rect ≈ 1–2 texels against nearest AABB depth (`should_occlusion_cull_aabb`).
- **Compaction**: count with atomics → prefix not needed if you write via `atomicAdd` slot and consume counts for indirect args.

## Common mistakes
- WGSL uniformity violations (derivatives/`textureSample` in non-uniform control flow) — meshlet materials forbid derivatives entirely.
- Assuming forward-Z: Bevy uses **reverse-Z** (near=1.0); depth comparisons flip.
- Struct padding mismatches (vec3 alignment 16) between Rust and shader.
- Forgetting `@if` flags in *both* the specializer key and the shader.
- Atomics on non-`atomic<T>` types; race on non-atomic read-modify-write.

## Performance considerations
Occupancy (registers/shared memory), coalesced reads (struct-of-arrays in storage buffers for culling), avoid per-thread divergent early-outs inside tight loops where subgroup-uniform work is possible, minimize `atomicAdd` contention (aggregate per subgroup first).

## Testing requirements
CPU reference + GPU compute compare test (seeded); validate shaders compile via a pipeline-creation test in CI (`naga`/wesl validation errors fail the test).

## References
`crates/bevy_pbr/src/meshlet/*.wesl`, `crates/bevy_pbr/src/render/*.wesl`, `crates/bevy_core_pipeline/src/mip_generation/**/*.wesl`, `crates/bevy_shader/src`, `examples/shader*/`, https://wesl-lang.dev
