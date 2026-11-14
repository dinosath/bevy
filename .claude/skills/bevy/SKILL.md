---
name: bevy
description: Entry-point skill for working on Bevy 0.20-dev (main) in this repo — how to verify version-specific behavior in source, API stability classes, crate map, and the pitfalls of stale docs. Use first for any Bevy question that depends on the current version.
---

# Bevy (this checkout: `0.20.0-dev`, `main`)

## Purpose
Give correct, *version-specific* Bevy advice. Training data and blog posts describe older Bevy (0.12–0.16). This checkout has diverged: **trust the source in this repo, not memory.**

## When to use
Always before advising on rendering, ECS API names, plugin setup or asset APIs. Then load the narrower skill (`bevy-ecs`, `bevy-rendering`, `bevy-meshlets`, `bevy-visibility`, `bevy-assets`, `bevy-shaders`, `bevy-world-streaming`).

## Mandatory first step: inspect the source
```bash
git log -1 --format='%h %ad %s' ; grep -m1 '^version' Cargo.toml
grep -rn "<symbol>" crates/*/src | head          # does it exist? where?
ls _release-content/migration-guides _release-content/release-notes   # what changed recently
git log --oneline -S'<symbol>' -- crates | head   # when did it appear/disappear?
```
Examples (`examples/**`) are compiled in CI and are the most reliable usage reference. `examples/README.md` indexes them.

## API stability classes — always label advice with one
| Class | How to recognize | Example here |
|---|---|---|
| **Stable** | Re-exported at `bevy::prelude` or a crate root, no `experimental` in path/feature | `VisibilityRange`, `OcclusionCulling`, `Mesh3d`, `MeshMaterial3d` |
| **Experimental** | Under an `experimental` module or Cargo feature documented "(experimental)" | `bevy::pbr::experimental::meshlet::*` (`Cargo.toml` features `meshlet`, `meshlet_processor`), `bevy_core_pipeline::mip_generation::experimental::depth` |
| **WIP** | Doc comment says "TODO"/"not yet implemented"/"currently"; example intentionally partial | Mesh shaders: `examples/shader_advanced/mesh_shader_intro.rs`; meshlet "chain-rule UV arithmetic (TODO: not yet implemented)" in `asset.rs` docs |
| **Deprecated** | `#[deprecated]`, migration guide in `_release-content/migration-guides/` | check `grep -rn '#\[deprecated' crates/<x>` |
| **Main-branch change** | Differs from latest crates.io release; see `git log`, release notes | **Render graph → systems** (see `bevy-render-graph`); `Event`→`Message` buffered events |

## Crate map (rendering/world relevant)
`bevy_ecs`, `bevy_app`, `bevy_asset`, `bevy_tasks`, `bevy_transform`, `bevy_camera` (cameras + **visibility**), `bevy_mesh`, `bevy_image`, `bevy_shader`, `bevy_material`, `bevy_extract` (main→render extraction), `bevy_render`, `bevy_core_pipeline` (Core3d/Core2d schedules, prepasses, mip generation/Hi‑Z), `bevy_pbr` (meshes, materials, **meshlets**, GPU preprocessing), `bevy_light`, `bevy_gizmos(_render)`, `bevy_dev_tools`, `bevy_diagnostic`, `bevy_gltf`, `bevy_solari` (RT lighting).

## Project conventions
- Docs and migration notes: user-visible changes need `_release-content/` entries (see its README).
- Cargo features and their meaning: `docs/cargo_features.md`. Rendering examples needing features list them via `required-features` in root `Cargo.toml`.
- Build with `cargo check -p <crate>` for speed; whole-engine example builds take minutes — use `cargo build --release --example <n>` only when you must run it.
- Local toolchain may be older than `rust-version` (1.100); say so if it blocks a build.

## Common mistakes
- Recommending removed APIs (`RenderGraph` *nodes* (`impl Node`; `RenderGraph` is now a root schedule), `EventReader`/`EventWriter` for buffered events — now `MessageReader`/`MessageWriter`) without grep'ing.
- Treating the meshlet path as general-purpose: it is opaque-only (see `bevy-meshlets`).
- Assuming a feature exists because Unreal/Nanite has it (see audit matrix: `docs/open_world/bevy_audit.md`).

## Performance considerations
Bevy's default 3D path already does GPU preprocessing + indirect drawing + GPU frustum culling (`GpuPreprocessingMode::Culling`, `crates/bevy_render/src/batching/gpu_preprocessing.rs`). Measure before replacing it.

## Testing requirements
`cargo test -p <crate>`; examples run in CI via `bevy_ci_testing` (`crates/bevy_dev_tools/src/ci_testing`). See `game-testing`.

## References
- `README.md`, `CONTRIBUTING.md`, `docs/`, `_release-content/`, `examples/README.md`
- Audit of GPU-driven/open-world features: `docs/open_world/bevy_audit.md`
