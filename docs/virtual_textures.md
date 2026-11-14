# Virtual / streaming textures

## Bevy facts [Bevy]
* No virtual texturing. Open issue **#25986 "Software Virtual Texturing"** (a contributor is prototyping on 0.20.0-rc.2 as a separate package/plugin; hardware sparse textures are not available in wgpu/WebGPU per the issue). Also open: #22676 (KTX2 selective layer loading for texture arrays).
* Existing: KTX2/Basis/DDS loading, `compressed_image_saver` (offline compression), bindless texture arrays in materials (`render_resource/bindless.rs`) with active bugs (#26429, #26427), `RenderAssetBytesPerFrame` upload throttle.
* No texture mip streaming/residency management; meshlet materials sample ordinary textures with explicit-gradient restrictions (`MeshletMesh` docs: no derivative builtins).

## Design [Proposal] — coordinate with #25986, do not fork it
`camera → feedback pass (page id per pixel at reduced res, written by material shaders) → async readback (1–2 frames) → page requests → cache (physical tile atlas, LRU) → async streaming → page-table texture (indirection) → sample`. Requirements shared with geometry streaming: one **IO scheduler** with per-class budgets (geometry/texture/shadow) so neither starves — implemented as a weighted queue in `bevy_world_streaming` (class weights, per-class in-flight caps, speculative cap already exist per-scheduler; per-class split is new). Interaction with meshlets: the visibility-buffer resolve already knows the pixel's material+UV, so the **feedback write belongs in the resolve pass** (no derivatives needed there).
Terrain pairing: clustered terrain geometry + virtual textures for splat/albedo is the planned landscape stack (`roadmap.md` Phase 14/16).
**Status: design only.** First step is to read/review #25986's prototype and agree an API boundary; no code written here.
