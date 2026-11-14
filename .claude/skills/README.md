# Project skills (open-world rendering & streaming)

Actionable instructions for coding agents. Audited first: the repo had **no** existing skills, so all were created here. Each `SKILL.md` follows: Purpose · When to use · Core concepts · Project conventions · Implementation patterns · Common mistakes · Performance · Testing · References (real source paths).

| Area | Skills |
|---|---|
| Rust | `rust`, `rust-performance`, `rust-concurrency` |
| Bevy fundamentals | `bevy` (start here), `bevy-ecs`, `bevy-assets`, `bevy-game-engine` |
| Rendering | `bevy-rendering`, `bevy-render-graph`, `bevy-gpu`, `bevy-shaders`, `bevy-meshlets`, `bevy-visibility`, `vegetation-rendering` |
| World | `open-world`, `level-streaming`, `bevy-world-streaming`, `predictive-streaming`, `procedural-worlds`, `game-architecture` |
| Pipeline/quality | `asset-pipelines`, `performance-profiling`, `GPU-profiling`, `game-testing` |

Key findings baked into the skills (verified against source at the time of writing; re-verify with `git log` — `bevy` skill explains how):
- The render graph is gone: passes are systems in `Core3d`/`Core2d` schedules.
- Meshlets: experimental, opaque-only, whole-mesh residency, need int64 texture atomics; no page streaming.
- Mesh shaders: pipeline plumbing + one example only.
- Visibility ranges (HLOD), GPU culling, two-phase occlusion (Hi-Z): exist.
- Streaming / residency policy / world partition: missing upstream → `crates/bevy_world_streaming`.

Full matrix: `docs/open_world/bevy_audit.md`. UE 5.9 ↔ Bevy comparison and design docs: `docs/ue_bevy_gpu_feature_matrix.md`, `docs/roadmap.md` (+ `nanite_architecture`, `gpu_scene`, `cluster_hierarchy`, `gpu_culling`, `occlusion`, `geometry_streaming`, `foliage`, `virtual_shadow_maps`, `virtual_textures`, `ray_tracing`, `backend_gpu_capabilities`, `performance`).
