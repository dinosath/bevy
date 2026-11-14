---
name: open-world
description: Architecture of large open-world games (Forza Horizon / GTA class) — world partition, cells, HLOD, terrain, roads, cities, forests, interiors, traffic/NPC/physics/AI scoping, save systems, large world coordinates and floating origin. Use when deciding how to split and stream a huge world.
---

# Open-world engine architecture

## Purpose
Split a huge world into independently streamable regions and make every subsystem (render, physics, AI, audio, save) respect the same partitioning. Patterns here are general public knowledge of AAA open-world engines, not copies of any proprietary implementation.

## When to use
Designing world data layout, deciding what loads when, picking coordinates, scoping simulation LOD.

## Core concepts
- **World partition**: regular 2D grid over the ground plane (XZ; Y up) of **cells** (typical 128–512 m; vehicle games use larger, 256–512 m, with *multiple grids* for different content classes: e.g. 64 m props, 256 m vegetation, 1 km HLOD/terrain tiles). Deterministic coordinates: integer `CellCoord { x, z }`. Cells are the unit of: IO, residency, LOD/HLOD proxy, simulation activation, save delta.
- **Large world coordinates**: `f32` loses precision (1 cm at ~65 km, ~1 m at ~8,000 km—shimmer/physics jitter starts ~ a few km). Store authoritative positions as **(CellCoord, local `Vec3` in cell)** or `f64`; render/physics in a *floating origin* frame near the camera (rebase when |local| > threshold; shift all `Transform`s by the cell delta in one system; GPU sees camera-relative f32). Types: `bevy_world_streaming::coords::{CellCoord, CellGrid, WorldPosition, FloatingOrigin}`.
- **Content classes** (each own streaming radius & representation chain):
  - *Terrain*: heightfield tiles (clipmap or quadtree), 3–5 LOD rings out to the horizon; always-resident coarse far tiles.
  - *Roads/rails/bridges*: spline network (graph) resident globally as lightweight data (it drives route-aware streaming, AI, minimap); geometry per cell.
  - *Buildings/cities*: instanced modular kits + unique landmarks; **HLOD per city block** (merged facade proxy); interiors are separate streamable "pockets" activated by proximity/portal, not part of the exterior cell.
  - *Vegetation*: instance tables per cell + species assets; density maps; see `vegetation-rendering`.
  - *Props*: instance tables, mostly static ⇒ no ECS entity per instance (GPU instance buffers).
  - *Vehicles/NPCs*: simulation LOD tiers (full physics/animation near → simplified kinematic along road splines → statistical/off-screen "population density" far). Entities exist only near the player; the rest are abstracted.
- **HLOD**: hierarchy of proxies: instance → cluster/forest region → cell → super-cell. Chosen by **screen-space error** with hysteresis; whole sub-hierarchies swap, which is what removes per-object cost at distance (`bevy_world_streaming::lod`, `hlod`).
- **Streaming**: see `level-streaming` (concepts), `bevy-world-streaming` (Bevy implementation), `predictive-streaming` (high-speed).
- **Simulation scoping**: gameplay/physics/AI tick only in the *active* ring (smaller than the render ring); the rest is frozen or abstracted. Physics broadphase uses local floating-origin coordinates.
- **Save systems**: persist *deltas* keyed by `(CellCoord, stable instance id)`; the baked world is read-only. Stable ids derive from bake-time deterministic hashes. Streamed-out dynamic state serializes into the cell's delta on unload.
- **Audio/AI nav**: nav meshes and audio zones are also per-cell assets with the same lifetime states.

## Project conventions
- Never make the world one asset or one scene. Never store world-space `f32` > ~4 km from origin for anything long-lived.
- Cell size is a config (`StreamingConfig::grid` (`CellGrid`)), not a constant; tests sweep sizes.
- Every cell-resident object carries its `CellCoord` (or is parented to the cell entity) so unload is O(1): despawn the cell root.
- Data flows one way: **bake → manifest → streaming → cell entities/instance buffers → render**. Gameplay never reads baked files directly.

## Implementation patterns
- Cell entity (`StreamedCell { coord, state }`) as parent of its spawned objects; unload = `despawn` recursive; GPU residency freed via asset handle drop.
- Interest = union of: radius around camera (hysteresis), predicted corridor along velocity/route (`predictive-streaming`), mandatory ring around the player (hard guarantee), spawn/teleport destination.
- Teleports: treat as `Interest` jump; show proxies (HLOD) instantly (always-resident far data), stream near ring with highest priority behind a *short* fade if needed.

## Common mistakes
- Square radius vs circular radius inconsistencies at diagonal corners (pick one metric and test it).
- Evicting the cell the player stands in because of memory pressure (pin the mandatory ring).
- Instance entity per tree (ECS cost) when GPU instancing suffices.
- Mixing content classes in one cell grid, forcing the worst-case radius for all.

## Performance considerations
Costs to track per frame: cells active, cells resident, bytes IO/upload per frame, entities spawned/despawned, visible instances. Budget each; expose in diagnostics (`StreamingStats`).

## Testing requirements
Deterministic route sweeps at 30–250+ km/h, teleport tests, cell-boundary tests (positions exactly on cell edges, negative coordinates), floating-origin rebase invariants (world-space position preserved to < 1 mm). See `game-testing`.

## References
`docs/open_world/{README.md,bevy_audit.md,architecture.md}`, `crates/bevy_world_streaming`, `examples/large_scenes/bevy_city` (existing procedural city scene; reuse for city benchmarks)
