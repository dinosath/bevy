# `bevy_world_streaming`

World partition, predictive cell streaming, geometry-page residency and HLOD policy for large open worlds in Bevy.

Bevy's renderer is already GPU-driven (indirect draws, GPU culling, two-phase occlusion, meshlets). What it does **not** have is a layer that decides *what data should be resident* for a world too big for RAM/VRAM. This crate is that layer. See `docs/open_world/` in the repository for the audit, architecture and benchmarks.

| Module | Role |
|---|---|
| `coords` | `CellCoord`, `CellGrid`, `WorldPosition`, `FloatingOrigin` (deterministic, large-world safe) |
| `predict` | `MotionPredictor`, `Route` corridors, `InterestBuilder` (mandatory / wanted / speculative tiers) |
| `scheduler` | pure streaming state machine: bounded queues, budgets, hysteresis, eviction, generations |
| `pages` | LOD-ordered geometry **page** residency under a VRAM budget |
| `lod` | screen-space-error representation selection, hysteresis, fallback to what is resident |
| `hlod` | forest-region aggregation into merged proxy meshes |
| `diagnostics` | `StreamingStats`, optional gizmo overlay (`gizmos` feature) |
| `plugin` | thin ECS glue (`WorldStreamingPlugin`), async IO/decode on Bevy task pools |
| `sim` | deterministic headless simulator for tests and benchmarks |

All behavior lives in the pure modules and is unit-tested without a GPU.

```text
cargo test  -p bevy_world_streaming
cargo run   -p bevy_world_streaming --release --example streaming_sim
cargo bench -p bevy_world_streaming
```
