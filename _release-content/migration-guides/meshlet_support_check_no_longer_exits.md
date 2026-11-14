---
title: Meshlet renderer no longer exits the process on unsupported GPUs, and LOD now respects instance scale
pull_requests: []
---

`MeshletPlugin` used to call `std::process::exit(1)` during `RenderStartup` when the GPU lacked
`MeshletPlugin::required_wgpu_features()`. It now logs an error, inserts a render-world
`MeshletSupport { supported: false }` resource, and disables all meshlet systems, so
`MeshletMesh3d` entities are not rendered and the app keeps running. Query `MeshletSupport` (or
check `MeshletPlugin::required_wgpu_features()` against `RenderDevice::features()`) to fall back to
the standard renderer. The `Msaa` misconfiguration check still exits.

Meshlet LOD selection in perspective views previously ignored the instance's scale when comparing a
meshlet group's simplification error against the screen-space budget (orthographic views already
applied it). Scaled instances now select LODs consistently: instances scaled up use finer clusters,
scaled down use coarser ones. Unscaled instances are unaffected.

Shadow views no longer rebuild their depth pyramid a second time after the second culling pass by
default. Set `MeshletSettings::rebuild_shadow_depth_pyramid_after_second_pass = true` to restore the
previous behavior.
