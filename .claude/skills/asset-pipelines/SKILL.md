---
name: asset-pipelines
description: Offline asset pipeline design for large worlds — import, preprocess (meshlets/LOD/impostors), quantize/compress, cell and page packing, metadata, dependency tracking, deterministic caching — mapped onto Bevy's processor/loader/saver. Use when designing or reviewing baked formats.
---

# Asset pipelines (source → runtime)

## Purpose
Everything expensive happens offline so the runtime only reads, validates, uploads. The pipeline must output data in **streamable units** (assets → cells → pages), because the runtime streams those units.

```
source asset (glTF/EXR/KTX/…)
   ▼ import        bevy_gltf loader (supports EXT_meshopt_compression, KHR_mesh_quantization,
   ▼               EXT_mesh_gpu_instancing; *no* KHR_draco, no KHR_texture_basisu by default)
   ▼ preprocess    tangents, merge, weld, LOD chain, meshlets+BVH (MeshletMesh::from_mesh), impostor atlas bake
   ▼ optimize      meshopt vertex cache/fetch reorder, overdraw, simplify (per LOD / per cluster group)
   ▼ compress      quantize positions (meshlet bitstream), octahedral normals, 16-bit UV, lz4/zstd blocks; textures → KTX2/Basis (compressed_image_saver)
   ▼ pack          per-asset pages + index, per-cell blobs (instance tables), manifest
runtime asset      loaders only deserialize; GPU upload budgeted (bevy-world-streaming)
```

## When to use
Designing baked formats, adding a processor, deciding what to bake vs. compute at load.

## Core concepts
- **Bevy hooks**: `AssetLoader` + `AssetSaver` + `Process`/`LoadTransformAndSave` (`bevy_asset::processor`, feature `asset_processor`) — see `bevy-assets`. Per-asset settings live in `.meta`; changing settings or processor version re-bakes (dependency log `processor/log.rs`).
- **Meshlet bake**: `MeshletMesh::from_mesh(&mesh, vertex_position_quantization_factor)` (feature `meshlet_processor`; pulls `meshopt` + `metis`; "very slow, ahead of time"). Output saved by `MeshletMeshSaver` (magic, version 3, lz4 frame).
- **Quantization**: meshlet vertex positions use per-meshlet min + per-channel bit widths + power-of-2 factor (default 4 ⇒ 1/16 cm) — tune per asset class: trees can take 2–3 (cm), buildings 4–5.
- **Standard (non-meshlet) mesh compression exists**: `Mesh::compressed_mesh(&MeshCompressionArgs)` in `crates/bevy_mesh/src/mesh.rs` — u16 indices, attribute quantization (`quantize_attributes`), attribute compression flags (needs shader decode); `MeshCompressionArgs::{none, regular}`; see `examples/stress_tests/many_cubes.rs`. Use it for LOD1+/proxy meshes that stay on the standard path before inventing a new format.
- **Compression stack**: entropy (lz4 fast decode ⇒ good for streaming; zstd smaller ⇒ better for disk/IO-bound) over quantized data. Decode speed must exceed IO bandwidth or you're CPU-bound (`rust-performance`).
- **Textures**: KTX2 with Basis/BC7 + mips (`bevy_image::{ktx2,basis,compressed_image_saver}`, example `examples/asset/compressed_image_saver.rs`). `examples/large_scenes/mipmap_generator` exists for large-scene mips/compression — reuse before writing a new tool.

## Project conventions (what the streaming runtime requires from the pipeline)
1. **Granularity**: `AssetPack = header + page index + pages`. A page is a contiguous, independently decodable byte range sized ≈ 64–256 KiB (tune: ≥ 1 IO block, small enough for fine residency). Page 0 = metadata + coarsest representation ("root page", always resident once the asset is referenced).
2. **LOD-ordered paging**: pages ordered coarse→fine so "first N pages" = valid lower-detail model. Each page records its world-space geometric `error` (meters) and byte size (`pages::PageDesc`) so the runtime can decide *which pages a view needs* without reading them (`bevy_world_streaming::pages::PageDesc`).
3. **Per-cell blob**: instance table (asset id, transform, seed) + HLOD proxy ref + bounds; ≤ a few MB; deterministic ordering (sorted by Morton/hilbert key) for cache coherence and reproducible bytes.
4. **Manifest**: cell index → blob offset/len, asset id → pack offset, content hashes, `FORMAT_VERSION`. Loader refuses mismatched versions explicitly.
5. **Determinism**: same inputs ⇒ byte-identical outputs (sorted iteration, seeded RNG, no timestamps). CI diffs hashes.
6. **Fallbacks are baked artifacts**: coarse proxy mesh per species, **impostor/billboard atlas** per species (N views × albedo/normal/depth), **forest HLOD** per cell (merged canopy hull + coarse trunk) — see `vegetation-rendering`.
7. **Metadata for the runtime**: bounding sphere/AABB, max error per LOD, material set, alpha-test flag (decides meshlet vs. instanced path), shadow policy.

## Implementation patterns
- Processor = pure function `fn bake(src: &Source, cfg: &BakeConfig) -> Baked` + thin Bevy `Process` wrapper; unit-test the pure part.
- Parallelize across assets with `AsyncComputeTaskPool`/rayon; each asset's bake is single-threaded and deterministic.
- Validate after bake: reload, compare triangle counts/bounds, check `error` monotonic across LODs, check page graph has no dangling refs.
- Keep a `bake_report.json`: bytes per page level, triangle counts, quantization error (max Hausdorff-ish in mm).

## Common mistakes
- Baking at first runtime load ("it's cached after") → hitches on first run.
- One giant world file; per-instance loose files (IO storms).
- Non-deterministic `HashMap` iteration in the baker.
- Normal maps baked from high-poly onto meshlet meshes (explicitly discouraged in `MeshletMesh` docs).
- Dropping UV precision beyond what tiling textures tolerate.

## Performance considerations
Bake throughput matters only for iteration speed; runtime cost = IO bytes + decode CPU + upload bytes. Report all three in the bake report.

## Testing requirements
Round-trip (bake→load) equality, format-version rejection, truncation fuzz, determinism hash test, size regression thresholds in a report (not wall-clock).

## References
`crates/bevy_pbr/src/meshlet/{from_mesh.rs,asset.rs}`, `crates/bevy_asset/src/processor`, `crates/bevy_gltf/src/lib.rs` (extension support table), `examples/large_scenes/mipmap_generator`, `examples/asset/processing`
