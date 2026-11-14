# Backend GPU capability matrix

Sources: **[wgpu-docs]** = `wgpu-types 30.0.2` `features.rs` doc comments (the version in `Cargo.lock` at the time); **[Bevy]** = Bevy docs/source; **[Bench-env]** = features actually reported by the dev GPU (AMD Vega/RADV, device-feature dump from `RUST_LOG=bevy_render=debug`). I did **not** test Metal or DX12 — those cells are what the documentation claims.

| Capability | wgpu feature | Vulkan | Metal | DX12 | Dev GPU (RADV Vega) | Used by |
|---|---|---|---|---|---|---|
| Compute shaders, storage buffers | core | yes | yes | yes | yes | all GPU culling |
| Indirect draw/dispatch | core + `INDIRECT_FIRST_INSTANCE` | yes ("mostly") | Apple3+/Mac1+ | yes | yes | indirect rendering |
| Multi-draw indirect **count** | `MULTI_DRAW_INDIRECT_COUNT` | 1.2+ / `VK_KHR_draw_indirect_count` | not listed | yes | yes (`drawIndirectCount`) | GPU-generated draw lists |
| Subgroup ops | `SUBGROUP` | yes | MSL 2.3+ | DXC only | yes (size 64) | wave-aggregated append, HZB |
| 64-bit shader ints | `SHADER_INT64` | yes | MSL 2.3+ | DXC | yes | meshlet visibility packing |
| 64-bit **image** atomics | `TEXTURE_INT64_ATOMIC` (+`TEXTURE_ATOMIC`) | `VK_EXT_shader_image_atomic_int64` | MSL 3.1+ | SM 6.6+ | **yes** | meshlet SW raster; UE-style VSM also needs 64-bit atomics [UE-doc] |
| 64-bit buffer atomics | `SHADER_INT64_ATOMIC_ALL_OPS` | `VK_KHR_shader_atomic_int64` | MSL 2.4+ | SM 6.6+ | yes | alt. visibility/VSM designs |
| Immediates (push constants) | `IMMEDIATES` | yes | yes | yes | yes | meshlet, HZB constants |
| `DEPTH_CLIP_CONTROL` | WebGPU feature | yes | — (see wgpu) | — | yes | meshlet HW raster |
| Binding arrays / non-uniform indexing | `TEXTURE_BINDING_ARRAY`, `…NON_UNIFORM_INDEXING`, `PARTIALLY_BOUND_BINDING_ARRAY` | 1.2+/descriptor indexing | MSL 2.0+ | yes | yes | bindless materials/textures |
| Mesh + task shaders | `EXPERIMENTAL_MESH_SHADER` | `VK_EXT_mesh_shader` (Naga supported) | yes (passthrough shaders) | yes (passthrough) | **no** `VK_EXT_mesh_shader` | `MeshPipelineDescriptor`; only an example |
| Timestamp queries (+inside passes) | `TIMESTAMP_QUERY*` | yes | yes | yes | yes | GPU profiler |
| Ray query (inline RT) | `EXPERIMENTAL_RAY_QUERY` | yes | yes | yes | not enabled | `bevy_solari` BLAS-per-mesh |
| **Cluster acceleration structures** | none in wgpu 30.0.2 | — | — | — | — | UE's CLAS path is not expressible |
| Depth pyramid / Hi-Z | compute + storage textures | yes | yes | yes | yes | occlusion |

## Bevy's own stated limits [Bevy]
`MeshletPlugin` docs: **works only on Vulkan and Metal**; requires `TEXTURE_INT64_ATOMIC | TEXTURE_ATOMIC | SHADER_INT64 | SUBGROUP | DEPTH_CLIP_CONTROL | IMMEDIATES`; not compatible with MSAA; process-exits on missing features. wgpu's docs list DX12 (SM 6.6) for the image-atomic feature, so DX12 may be feasible but is **untested here** — classify as `BACKEND LIMITED (untested)`.

## Capability-driven path selection [Proposal]
```
caps = probe(RenderDevice::features())
virtual_geometry  = caps ⊇ MESHLET_REQUIRED          → meshlet renderer
else              → standard GPU-preprocessed path (+ VisibilityRange/HLOD) with fallback meshes   (Part 18)
mesh_shaders      = caps ∋ EXPERIMENTAL_MESH_SHADER  → optional task/mesh raster; compute+indirect remains default
vsm               = caps ∋ TEXTURE_INT64_ATOMIC       → virtual shadow pages; else cascaded maps
rt                = caps ∋ EXPERIMENTAL_RAY_QUERY     → BLAS over fallback meshes
bindless          = caps ⊇ binding-array set          → material tables; else per-material bind groups
```
**Done**: `check_meshlet_features` no longer exits the process; it inserts a render-world `MeshletSupport { supported }` resource and every meshlet system is gated on it (verified on hardware with `--disable-int64-atomics`). Apps can read `MeshletSupport` to fall back. (The `Msaa` misconfiguration check still exits.)
