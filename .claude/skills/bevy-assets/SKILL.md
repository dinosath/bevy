---
name: bevy-assets
description: Bevy asset system on main — AssetServer, loaders/savers/processors, asset sources and readers, handles/lifetimes, RenderAsset, GPU asset prep, what is and isn't suitable for streaming. Use when loading, baking or unloading assets at scale.
---

# Bevy assets (0.20-dev)

## Purpose
Use Bevy's asset infrastructure for async loading and offline baking; know its limits for large-world streaming so you build the missing layer *on top* instead of fighting it.

## When to use
New asset types, loaders, baked formats, loading/unloading policy, asset dependency questions.

## Core concepts (`crates/bevy_asset/src`)
- **AssetServer**: async loads on the IO task pool; `Handle<A>` strong handles keep assets alive; dropping the last strong handle unloads. Dependencies tracked via `LoadContext` (labeled sub-assets, `load_context.load()`), recursive dependency state via `AssetServer::recursive_dependency_load_state` / `LoadState`.
- **AssetLoader / AssetSaver / Process** (`loader.rs`, `saver.rs`, `processor/`):
  - `AssetLoader::load(reader, settings, load_context)`; settings are per-asset in `.meta` files.
  - `AssetSaver` serializes a loaded asset (e.g. `MeshletMeshSaver`, compressed image saver `crates/bevy_image/src/compressed_image_saver`).
  - `Process` / `LoadTransformAndSave` (migration note: `LoadTransformAndSave::new` takes an `AssetLoader`, `with_implicit_loader` keeps old behavior — `_release-content/migration-guides/load_transform_and_save_new.md`) = *source asset → transform → processed asset* in an **imported/processed asset folder**, enabled by the `asset_processor` cargo feature; `AssetProcessor` watches sources and writes outputs + dependency log (`processor/log.rs`).
- **Asset sources & IO** (`io/`): `AssetReader::read(path) -> Reader`; `Reader: AsyncRead` and optionally seekable (`Reader::seekable()`); sources: file, embedded, memory, web, android, wasm; custom sources register via `AssetSourceBuilder`. **There is no built-in byte-range / partial-asset read API at the `AssetServer` level** — a loader gets a whole asset. For page-granular streaming implement your own reader over an *index + pack file* (`seek` + `read_exact`) inside a dedicated source/loader (see `asset-pipelines`).
- **RenderAsset** (`bevy_render/src/render_asset.rs`): main-world asset → render-world GPU asset with retry-on-deferral and per-frame byte budget hooks (check `RenderAssetBytesPerFrame` in source if you need to cap uploads).
- **Hot reload / `file_watcher`**: dev only.
- **Meshlet precedent**: `MeshletMeshLoader` reads magic+version header then an **lz4 frame** payload (`asset.rs`); `MeshletMeshManager` removes GPU ranges when the asset is removed.

## Project conventions
- Heavy preprocessing is **offline** (processor/bake tool), never in the game loop. Runtime loaders only deserialize/validate.
- Runtime formats are versioned (`MAGIC` + `VERSION`), little-endian, `Pod`-castable, 4-byte aligned for GPU copy (`COPY_BUFFER_ALIGNMENT`).
- Cell/world data are **separate assets** per cell (not one world asset). Cross-cell sharing (a tree species) is an **asset dependency** referenced by id, loaded once, refcounted.
- Own the lifetime: keep `Handle`s in cell state; unloading a cell = dropping handles, which also drops GPU residency (verify `RenderAsset` removal path frees GPU ranges).
- Never `block_on` an asset load on the main thread (`rust-concurrency`).

## Implementation patterns
```rust
// request
let h: Handle<CellData> = asset_server.load(cell_path);   // async, returns immediately
// poll
match asset_server.load_state(&h) { LoadState::Loaded => …, LoadState::Failed(e) => …, _ => keep fallback }
```
- Prefetch = `load()` early and hold the handle in a `Prefetching` state without spawning entities.
- Custom budgeted pipeline (what `bevy_world_streaming` does): own `IoTaskPool` reads for cell blobs + `AsyncComputeTaskPool` decode; hand finished data to the render world through a budgeted writer. Use `AssetServer` when you want dependency tracking/hot-reload; use direct tasks when you need explicit priorities/cancellation/byte budgets — `AssetServer` has **no priority or cancellation API**.

## Common mistakes
- Treating `LoadState::Loaded` as "GPU resident" (render asset prep is later; see `RenderAsset` retry).
- Loading thousands of tiny assets (one per tree) → IO-task and handle overhead; pack per cell/species.
- Keeping the CPU copy of mesh data after GPU upload on streamed assets (meshlet manager does `assets.remove_untracked` after queueing — copy that pattern).
- Forgetting version bump when the layout changes.

## Performance considerations
Decompression (lz4/zstd) belongs on `AsyncComputeTaskPool`, not IO threads; sequential large reads beat many small ones; keep pack files aligned to filesystem blocks.

## Testing requirements
Loader round-trip tests with in-memory reader (`io::memory`), corrupt/short/wrong-version input tests, deterministic bake output (byte-identical across runs).

## References
`crates/bevy_asset/src/{server,loader.rs,saver.rs,processor,io}`, `crates/bevy_pbr/src/meshlet/asset.rs`, `crates/bevy_image/src/compressed_image_saver`, examples `examples/asset/*` (e.g. processing example gated by `asset_processor`)
