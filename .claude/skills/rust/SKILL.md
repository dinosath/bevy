---
name: rust
description: Rust engineering conventions for Bevy engine code — data-oriented design, ownership/zero-copy, error handling, unsafe/FFI rules, workspace and feature-flag hygiene. Use when writing or reviewing any Rust in this repo.
---

# Rust (engine code)

## Purpose
Write Rust that is idiomatic *and* has predictable memory behavior. For engine code the rule is:
**prefer data-oriented Rust and predictable memory behavior.** Avoid unnecessary allocations, cloning, locking, synchronization, virtual dispatch and CPU/GPU synchronization — in that order of suspicion on hot paths.

## When to use
Any `.rs` change. Pair with `rust-performance` (measuring), `rust-concurrency` (threads/async), `bevy-ecs` (ECS-specific patterns).

## Core concepts and how they apply here
- **Ownership / borrowing / lifetimes**: hand out `&[T]` / `&mut [T]` views, not `Vec<T>`. Store shared immutable blobs as `Arc<[T]>` (see `MeshletMesh` fields in `crates/bevy_pbr/src/meshlet/asset.rs`: `Arc<[u32]>`, `Arc<[BvhNode]>` so GPU upload queues clone a pointer, not the data).
- **Generics vs `dyn`**: use generics/associated types on hot paths (monomorphized, inlinable). `dyn Trait` is fine at coarse seams (asset sources, loaders) — one virtual call per *cell/asset*, never per instance/vertex.
- **Iterators**: prefer iterator chains over index loops (bounds-check elision, auto-vectorization). Use `chunks_exact`, `zip`, `iter().copied()`. Avoid `collect()` into a temp `Vec` inside per-frame systems; reuse a `Local<Vec<_>>` and `.clear()`.
- **Zero-copy**: `bytemuck::{Pod, Zeroable}` + `#[repr(C)]` for GPU/disk structs (see `Meshlet`, `BvhNode`). `bytemuck::cast_slice` instead of per-element serialization. `Cow<'static, str>` / `&'static str` for labels.
- **Memory layout / cache locality**: size and align structs deliberately (`size_of` asserts in tests for GPU-facing types). Keep hot fields together; split cold data (names, debug info) into separate arrays. Prefer SoA for large homogeneous sets (see `rust-performance`).
- **Allocation avoidance**: `SmallVec`, `ArrayVec`, `with_capacity`, reuse buffers, arena/pool for per-frame scratch. Never `format!`/`String` in per-frame hot paths (use `&'static str` or lazily formatted debug-only).
- **Error handling**: libraries return `Result<T, thiserror enum>`; never `unwrap()` on I/O or user data outside tests. `expect("invariant: …")` only for true invariants. Bevy systems may return `Result` (`bevy_ecs::error::Result`) — prefer it over `unwrap`.
- **unsafe**: only when measured necessary, with a `// SAFETY:` comment stating the invariant and a test/`debug_assert!` that exercises it. This repo `#![expect(unsafe_code)]`s only where needed — new crates should `#![forbid(unsafe_code)]` unless justified.
- **FFI**: wrap behind a safe API in one module; `#[repr(C)]`, no panics across the boundary (`catch_unwind`), document ownership of every pointer. Existing example: `meshopt`/`metis` use in `from_mesh.rs`.
- **SIMD**: write scalar, check autovectorization (`cargo asm` / perf), then use `glam` (already SIMD where available) before hand-written intrinsics. Gate intrinsics behind `cfg(target_feature)` with scalar fallback.
- **Async**: see `rust-concurrency`. Never `block_on` on the main/render thread inside a frame.

## Project conventions (Bevy repo)
- Edition 2024, `rust-version = 1.100.0` in workspace `Cargo.toml` (local `rustc` may be older — `cargo check` still works for most crates; note failures honestly).
- Workspace: every crate under `crates/*` is a workspace member automatically. Internal deps are `{ path = "../x", version = "0.20.0-dev" }`. New crates need `[lints] workspace = true`.
- `no_std` where the crate already is (`bevy_tasks`, `bevy_ecs`, …): use `alloc::`/`core::` paths, `bevy_platform` for `HashMap`/`Instant`/sync types (`bevy_platform::collections::HashMap`, `bevy_platform::time::Instant`). Do not import `std::collections::HashMap` in such crates.
- Feature flags: additive only; no feature may remove API. Optional heavy deps (`meshopt`, `metis`, `lz4_flex`) sit behind features (`meshlet_processor`, `meshlet`). Document each in `docs/cargo_features.md`.
- Compile time: avoid giant generic functions in hot crates, prefer `dyn`/non-generic inner fn (`fn foo<T: Into<X>>(t: T) { foo_inner(t.into()) }`); keep proc-macro use modest; check `cargo build --timings`.
- Lints/format: `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings` (see `docs/linters.md`; CI tool in `tools/ci`).
- Docs: every public item documented, with `[`links`]`. Use `#[doc(hidden)]` sparingly.

## Common mistakes
- `clone()` of `Vec`/`String`/`Handle` in a per-entity loop; `to_vec()` just to get ownership.
- `HashMap` iteration order leaking into deterministic output (use `BTreeMap`/sorted keys or `bevy_platform` fixed-hasher maps; see `procedural-worlds`).
- Locks held across frames; `Mutex<Vec<_>>` on a per-item path instead of a channel or `bevy_utils::Parallel` thread-local.
- `f32` accumulation for large world coordinates (see `open-world`: use integer cell coords + local f32).
- `as` casts silently truncating; use `try_from` or `clamp` first.

## Performance considerations
Measure first (`rust-performance`). Release-profile numbers only; `cargo bench`/criterion for micro, a deterministic scene for macro (`game-testing`).

## Testing requirements
Unit tests beside code, `#[should_panic]` only for documented panics, property-style tests via seeded RNG (`rand_chacha`). GPU-facing structs: `size_of`/`align_of` asserts.

## References
- `Cargo.toml` (workspace), `docs/linters.md`, `docs/cargo_features.md`, `CONTRIBUTING.md`
- `crates/bevy_pbr/src/meshlet/asset.rs` (Pod + Arc<[T]> pattern), `crates/bevy_pbr/src/meshlet/persistent_buffer.rs`
