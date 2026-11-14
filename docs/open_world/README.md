# Open-world rendering & streaming

| Doc | What |
|---|---|
| [`bevy_audit.md`](bevy_audit.md) | Feature matrix of what Bevy `main` already has (meshlets, GPU culling, occlusion, HLOD/visibility ranges, mesh shaders, compression, streaming…) with source paths |
| [`architecture.md`](architecture.md) | The layered design, what is implemented vs upstream vs **not done** |
| [`benchmarks.md`](benchmarks.md) | All measurements (real-GPU forest/meshlet runs, streaming simulation, criterion) with environment and caveats |
| [`streaming_sim_full.md`](streaming_sim_full.md) | Complete streaming simulation tables |

UE 5.9 comparison, GPU-driven-renderer design docs and roadmap: [`../ue_bevy_gpu_feature_matrix.md`](../ue_bevy_gpu_feature_matrix.md), [`../roadmap.md`](../roadmap.md), [`../performance.md`](../performance.md).

Code: `crates/bevy_world_streaming` (library + tests + bench + `streaming_sim` example), `examples/large_scenes/forest_benchmark`.
Agent skills: `.claude/skills/` (start with `bevy`, `bevy-world-streaming`, `performance-profiling`).

## Reproduce
```bash
# toolchain: repo requires Rust 1.100 (stable 1.100.0 works; the recorded benchmarks used nightly 1.101)
cargo test  -p bevy_world_streaming
cargo bench -p bevy_world_streaming --bench streaming
cargo run   -p bevy_world_streaming --release --example streaming_sim
cargo run   --release -p forest_benchmark -- --label baseline
cargo run   --release -p forest_benchmark -- --label all --lod --hlod --occlusion
cargo run   --release -p forest_benchmark --features meshlet -- --meshlet   # needs meshlet GPU features
```
(Wayland dev headers were missing on the dev machine, so the benchmark crate enables only `x11`; add `wayland` on systems that have it.)
