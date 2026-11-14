---
name: rust-concurrency
description: Concurrency patterns for engine code — Bevy task pools, channels, atomics, Arc/Rc, async IO, never blocking the render/main thread. Use when adding threads, async loading, or shared state.
---

# Rust concurrency (engine)

## Purpose
Use parallelism and async IO without stalls, races or lock convoys. Core rule: **the main thread and the render thread never wait on IO or on a lock another thread may hold for long.**

## When to use
Async asset/cell loading, background preprocessing, parallel systems, cross-thread queues, shared counters/stats.

## Core concepts
- **Bevy task pools** (`bevy_tasks`): `IoTaskPool` (blocking-ish IO, many threads), `AsyncComputeTaskPool` (CPU-heavy background work across frames), `ComputeTaskPool` (per-frame fork/join, used by the ECS executor). Choose by *how long and how blocking*, not by habit. Source: `crates/bevy_tasks/src/`.
- **Task handles**: `pool.spawn(async { … })` → `Task<T>`; drop cancels. Poll from a system with `bevy_tasks::futures::check_ready(&mut task)` / `block_on(poll_once(&mut task))` — a *poll-once*, never a blocking `block_on(task)`.
- **Channels**: prefer bounded `crossbeam-channel`/`async_channel` for producer→consumer handoff with backpressure; unbounded only when producer rate is provably bounded. A bounded queue is a *budget* — make it explicit.
- **Atomics**: counters/flags/stats only (`AtomicU64::fetch_add(Relaxed)`). Use `Acquire/Release` pairs for publish/consume of data; if you reach for `SeqCst` or CAS loops, stop and consider a channel.
- **Lock-free structures**: only for measured hot contention (e.g. MPSC queues). Prefer per-thread buffers merged once per frame (`bevy_utils::Parallel<T>`).
- **Arc / Rc**: `Arc<[T]>` for shared immutable blobs crossing threads; `Rc` is `!Send` — don't put it in components/resources. Cloning an `Arc` per item per frame is still an atomic RMW: batch.
- **Async Rust**: futures must be `Send` (or `ConditionalSend`; wasm is single-threaded — see `cfg::conditional_send` in `bevy_tasks`). Don't hold a `MutexGuard` across `.await`.
- **Thread pools**: sizing is configured via `TaskPoolPlugin`/`TaskPoolOptions` (`crates/bevy_app/src/task_pool_plugin.rs`). Don't create private OS threads in game code.

## The streaming pipeline shape (reference pattern)
```
main thread system          IoTaskPool              AsyncComputeTaskPool        render world
request(cell) ───────────▶ read bytes ───────────▶ decompress/prepare ───────▶ upload (budgeted)
        ▲  poll_once()           │ bounded in-flight cap     │ bounded cap          │ bytes/frame cap
        └──────── results drained each frame, applied within a time/bytes budget ◀───┘
```
Each arrow is a bounded queue. See `bevy-world-streaming`.

## Project conventions
- Every queue: capacity constant + metric (`pending`, `dropped`, `high_water`). Every per-frame drain: a budget (count, bytes or µs).
- No `std::thread::sleep`/`recv()` (blocking) in systems; use `try_recv()`.
- Shared stats: `Arc<AtomicU64>` or a resource written only on the main thread; never `Mutex<HashMap>` for stats.
- Deterministic tests: run the same logic through a **synchronous** `CellSource` test double; async glue is thin and separately tested.

## Common mistakes
- `block_on` inside a system; `join` on a task you spawned this frame.
- Unbounded channel + slow consumer → memory growth; "fixed" by `Mutex<VecDeque>` → contention.
- Spawning one task per tiny item (use batches/chunks).
- Reading assets on `ComputeTaskPool` (it's for short fork/join) — starves the ECS executor.
- Forgetting cancellation: unloading a cell must drop/ignore its in-flight task result (generation counter).

## Performance considerations
Measure contention (`rust-performance`). Prefer *fewer, larger* tasks; avoid cross-thread ping-pong per item; keep data owned by one thread at a time.

## Testing requirements
- Unit-test the state machines synchronously. Add one integration test with real tasks that asserts *completion + budgets*, not timing.
- Loom is overkill here unless you write a custom lock-free structure (then required).

## References
- `crates/bevy_tasks/src/{task_pool.rs,usages.rs,iter,futures.rs}`
- `crates/bevy_asset/src/server/` (async loading), `crates/bevy_pbr/src/meshlet/meshlet_mesh_manager.rs` (queue → budgeted GPU write)
