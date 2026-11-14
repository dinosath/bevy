---
name: rust-performance
description: Measure-first high-performance Rust — allocation/cache/contention profiling, SoA vs AoS, criterion, perf, flamegraphs. Use before and after any optimization; defines the required optimization report format.
---

# Rust performance

## Purpose
Make code faster **only with evidence**. Every optimization is recorded as:

```
baseline        – command + numbers before
bottleneck      – what the profile says (not a guess)
change          – the diff, in one sentence
measurement     – same command + numbers after (same machine, same build profile)
result          – delta, with variance
regression risk – what could get worse (memory, latency tail, readability, other platforms)
```
No row, no optimization. Claims without numbers do not go into docs, PRs or skills.

## When to use
Whenever something is "slow", before touching hot code, and when reviewing a perf PR.

## Core concepts
- **Allocation profiling**: `heaptrack`, `valgrind --tool=dhat`, or a counting `#[global_allocator]` in a test to assert "zero allocations per tick" for steady state.
- **Cache behavior**: `perf stat -e cache-misses,cache-references,instructions,cycles,branch-misses`. Lots of L1/L2 misses → data layout problem, not an algorithm problem.
- **Memory bandwidth**: if throughput plateaus below core count × single-core speed, you're bandwidth bound; shrink data (quantize, `u16`/`u8` indices, bitsets) rather than adding threads.
- **Thread/lock contention**: `perf lock`, tracy lock plots, or count wait time with `Instant` around `lock()`. Replace shared locks with per-thread buffers (`bevy_utils::Parallel`) merged once.
- **Parallel iteration**: `Query::par_iter` (ECS) or `bevy_tasks::ComputeTaskPool::scope`. Batch size matters: aim for ≥ ~10 µs of work per task; too small = scheduler overhead.
- **SIMD**: first confirm the loop vectorizes (`cargo asm`, `perf annotate`). Use `glam` types; structure data as SoA so lanes are contiguous.
- **SoA vs AoS**: AoS when you touch most fields of one item together; SoA when a hot loop reads 1–2 fields across many items (positions for culling, priorities for sorting). ECS components are already SoA-per-archetype — don't re-pack inside a component.

## Tools on this platform (Linux)
- `perf record -g --call-graph dwarf ./target/release/<bin>`; `perf report`; flamegraph via `cargo flamegraph` or `perf script | inferno-flamegraph` (see `docs/profiling.md` "Perf flame graph"). Build with `[profile.release] debug = 1` (or `CARGO_PROFILE_RELEASE_DEBUG=1`).
- Bevy spans: `--features trace_tracy` / `trace_chrome` and `info_span!` (see `docs/profiling.md`).
- Micro-benchmarks: `criterion` (workspace has `benches/` crate: `benches/benches/bevy_*/`). Use `black_box`, throughput annotations, fixed seeds.
- Compile-time perf: `cargo build --timings`, `-Zself-profile` on nightly.

## Project conventions
- Benchmarks live in the crate's `benches/` (criterion) or the top-level `benches/` crate; deterministic inputs from a seeded RNG; no wall-clock sleeps or real IO in benches.
- Report medians + a spread (p5/p95 or std-dev), warm-up excluded, `--release` (or the repo's `bench` profile), CPU governor noted.
- Hot per-frame systems must have steady-state zero allocations: reuse `Local<Vec<_>>`, `Vec::clear()`, preallocated rings.

## Implementation patterns
- Precompute once, query many (spatial grids, sorted arrays + binary search over `HashMap` for read-mostly data).
- Replace `Vec<Struct{hot, cold}>` with `(Vec<Hot>, Vec<Cold>)` when the profile shows cache misses on the hot loop.
- Replace `f32` sqrt/normalize in culling with squared-distance comparisons.
- Bound everything: queues have capacity, per-frame budgets (items or bytes) are explicit.

## Common mistakes
- Optimizing in debug builds. Comparing runs with different background load or thermal state.
- Benchmarking a loop the compiler deleted (no `black_box`).
- "Faster" that moved work to another thread but raised total CPU or frame-time variance.
- Trusting average frame time: tail (p99) is what players feel in streaming/stall problems.

## Testing requirements
Keep the benchmark; add a threshold *test* only if deterministic (op counts, allocation counts, bytes), never on wall-clock time in CI.

## References
- `docs/profiling.md`, `benches/`, `crates/bevy_utils/src/parallel_queue.rs` (`Parallel`), `crates/bevy_tasks`
- See `performance-profiling` for whole-game (CPU/GPU) triage and `GPU-profiling` for GPU.
