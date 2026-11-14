---
name: game-testing
description: Testing engine features — deterministic scenes, benchmark scenes, scripted camera paths, streaming/high-speed/teleport/memory-stress tests, visual regression, and the rule that every performance feature has a reproducible benchmark. Use whenever adding or changing engine behavior.
---

# Game / engine testing

## Purpose
Make behavior and performance **reproducible** so changes can be judged by numbers and diffs.

## Test layers
1. **Pure unit tests** (fast, in `src/`): coordinates, prediction, scheduler, LOD selection, page tables, HLOD builder. No ECS/GPU. Seeded RNG, fake clock (`dt` passed in).
2. **Simulation tests** (`bevy_world_streaming::sim`): headless deterministic world + IO model + scripted routes. Assert invariants (below). These are the *streaming regression tests*.
3. **ECS integration tests** (`MinimalPlugins` + manual `Time` advance): plugin wiring, spawn/despawn, budgets respected under real task pools (assert completion + budgets, not wall time).
4. **Render/GPU tests** (skip when features missing): compute shader vs CPU reference; headless render with screenshot compare (CI via `bevy_dev_tools::ci_testing`, `crates/bevy_dev_tools/src/ci_testing`).
5. **Benchmarks**: criterion micro (`cargo bench -p …`) + macro scene runs with CSV frame logs.

## Deterministic scenes
Fixed seed ⇒ identical instance tables, routes and camera paths. A scene is defined by `(name, seed, params)`; the benchmark prints them. Forest scene: `N` trees, `S` species, density clusters, occluder blocks. City scene: reuse `examples/large_scenes/bevy_city`. Never use wall-clock or frame count as the camera driver — use a **time-parameterized path** advanced by a fixed `dt`.

## Streaming test matrix (must exist)
| Test | Assertion |
|---|---|
| Speed sweep 30/60/100/150/200/250 km/h (straight, curved, switchback) | hard misses == 0 up to the configured guarantee speed; report fallback frames, p99 latency |
| Teleport (near/far/destination known) | proxies visible same frame; mandatory ring resident ≤ bound; no unbounded queue growth |
| Boundary & negative coords | cell mapping exact at edges; rebase preserves world position < 1 mm |
| Memory stress (tiny budgets) | RAM/VRAM never exceed budget; mandatory ring pinned; no deadlock; degrade to fallbacks |
| IO stall injection (latency spikes, failures) | retries/backoff; never blocks `tick`; hard misses reported not hidden |
| Cancel/evict while in flight | stale results discarded (generation) |
| Determinism | same seed ⇒ identical stats trace (hash) |
| Prediction A/B | predictive ≤ baseline in hard misses at every speed |

## Rendering tests
Camera-path screenshots at fixed frames compared with tolerance; LOD pop test (consecutive-frame difference ≤ threshold near switch distances); counters test (visible/culled counts within expected bounds for a known layout, e.g. the occlusion example).

## Every performance feature needs
a benchmark (command, scene, seed), a baseline number committed in `docs/open_world/benchmarks.md`, and a report using the `rust-performance` format. No numbers ⇒ no claims.

## Project conventions
- Tests never touch network/real disk (use in-memory sources); GPU tests probe adapter & features and `return` (skip) with a printed reason.
- Name sim scenarios explicitly (`drive_250kmh_forest_road`); log scenario + seed + commit on failure.
- Keep CI tests fast (< seconds); long sweeps are benches/examples run on demand.

## Common mistakes
Timing assertions in CI; nondeterministic ordering (`HashMap`); tests that pass only with warm caches; skipping GPU tests silently (print why).

## Performance considerations
Benchmarks: release, warm-up, ≥ N samples, record spread; macro runs log per-frame CSV so p99 can be recomputed offline.

## References
`crates/bevy_world_streaming/{src/sim.rs,benches,tests}`, `docs/open_world/benchmarks.md`, `crates/bevy_dev_tools/src/ci_testing`, `.github/workflows`, `tools/ci`
