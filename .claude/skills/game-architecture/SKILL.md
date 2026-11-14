---
name: game-architecture
description: Engine/game system design — separating gameplay, simulation, rendering, asset management, world streaming and platform code; ECS scheduling, render/update separation, asset lifecycle, scene management, physics/animation/audio/AI/input/networking/save-load boundaries. Use when deciding where code belongs.
---

# Game / engine architecture

## Purpose
Prevent one giant system. Each concern gets a module/crate with a narrow interface, its own data, its own schedule position and its own tests.

## Layers (dependencies point downward only)
```
gameplay        rules, missions, vehicles-as-gameplay, UI logic        (reads sim state, issues commands)
simulation      physics, AI, traffic, animation state, audio logic    (fixed timestep, active ring only)
world streaming cells, residency, prediction, HLOD policy              (decides WHAT is resident/active)
asset mgmt      loaders, processors, baked formats, handles, lifetimes (decides HOW data is read/decoded)
rendering       extract, GPU resources, passes, culling, LOD on GPU    (consumes render representations only)
platform        windowing, input devices, IO sources, threads, GPU API (wgpu/winit)
```
- **Gameplay** never touches GPU/IO. **Rendering** never reads gameplay components except the extracted minimal view. **Streaming** talks to others through (a) cell lifecycle events/messages and (b) resources, not direct calls. **Platform** is behind traits/plugins.

## Scheduling
- Input → `PreUpdate`; gameplay/AI/animation logic → `Update`; physics/vehicle sim → `FixedUpdate`; transform propagation, visibility → `PostUpdate`; streaming plan/IO polling early in `Update` (so activation lands before visibility), uploads in render world budgeted. Expose `SystemSet`s per subsystem for external ordering.
- Render/update separation: pipelined rendering means render world lags one frame; do not rely on render results in same-frame gameplay (use async readback with latency).

## Subsystem boundaries (what each owns)
- **Scene management**: cell roots are the scene unit (`open-world`); no global "scene" that must fit in memory.
- **Asset lifecycle**: owner holds `Handle`s; request → load → decode → GPU-resident → drop; cell unload drops handles (`bevy-assets`).
- **LOD/HLOD**: *policy* in streaming/lod crate; *execution* in renderer (visibility ranges / GPU).
- **Physics**: colliders per active cell; broadphase in floating-origin local space; far dynamics abstracted.
- **Animation**: skeletal near, none far; instanced vertex animation (wind) in shaders, not CPU.
- **Audio**: per-cell emitters activated with the active ring; reverb zones as cell data.
- **AI/traffic**: population as density fields far, agents near, bound to road graph.
- **Input**: device → action mapping resource; gameplay reads actions.
- **Networking**: authoritative state is cell+id based (stable IDs, `procedural-worlds`); replication scoped by the same interest management as streaming.
- **Save/load**: deltas keyed by `(cell, stable id)` + global state; load = apply deltas on cell activation.

## Project conventions
- One crate (or at least one module tree) per layer; cross-layer interfaces are small traits/messages/resources. Tests exist per layer without the layers above.
- Prefer composition via plugins; every plugin documents its sets, resources, messages.
- Data > behavior: describe content (baked data) rather than hardcode in systems.
- Budgets are first-class (frame ms per subsystem, bytes/frame IO & upload, entities spawned/frame).

## Common mistakes
Streaming code that spawns gameplay objects directly; render systems reading gameplay state; global mutable singletons; one `GameState` resource touched by every system; per-frame work proportional to world size.

## Testing requirements
Layer-level tests with fakes (fake `CellSource`, fake renderer sink). A change in one layer must not require changing tests in layers above unless the interface changed.

## References
`bevy-ecs`, `open-world`, `level-streaming`, `bevy-rendering`; `crates/bevy_app/src/main_schedule.rs`
