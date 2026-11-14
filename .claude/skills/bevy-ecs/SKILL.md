---
name: bevy-ecs
description: Bevy ECS patterns for engine/gameplay code on 0.20-dev — components, resources, queries, messages, schedules, plugins, change detection, parallelism. Use when designing systems or data for large entity counts.
---

# Bevy ECS (0.20-dev)

## Purpose
Use the ECS so that systems parallelize, data stays cache-friendly and per-frame work is proportional to *what changed*, not to world size.

## When to use
Adding components/systems/plugins; deciding what is an entity vs. a resource vs. plain data inside a component.

## Core concepts (verify names in `crates/bevy_ecs/src`)
- **Component / Resource**: `#[derive(Component)]`, `#[derive(Resource)]`. Required components: `#[require(Transform, Visibility)]` (see `MeshletMesh3d` in `crates/bevy_pbr/src/meshlet/mod.rs`).
- **Queries**: `Query<(&A, &mut B), (With<C>, Without<D>)>`. Use `Query::par_iter`/`par_iter_mut` for CPU-heavy per-entity work. Filters `Added<T>`, `Changed<T>`, `Has<T>`.
- **Messages** (buffered events): `#[derive(Message)]`, `MessageWriter<M>` / `MessageReader<M>` (`crates/bevy_ecs/src/message/`). Observers/`Trigger` for immediate, entity-targeted reactions (`_release-content/migration-guides/trigger_trait.md`).
- **Schedules**: `Startup`, `First`, `PreUpdate`, `Update`, `PostUpdate`, `Last`, `FixedUpdate`; render-side `ExtractSchedule`, `Render`, `RenderStartup`, `Core3d`. Order with `.before()/.after()/.chain()/.in_set()`. System sets are the unit of ordering contract for other plugins (cf. `VisibilitySystems`, `RenderSystems`).
- **Plugins**: one plugin = one cohesive feature; expose config as a `Resource`; register systems in the right schedule; avoid global side effects in `build`.
- **Commands** defer structural changes (spawn/despawn/insert) to sync points — batch spawns with `commands.spawn_batch` / `Commands::spawn` on large streams; despawn recursively with care.
- **Local<T>**: per-system scratch; reuse `Vec` capacity across frames.
- **Systems returning `Result`**: prefer `-> Result` over `unwrap`.

## Project conventions
- Heavy/rare data lives *outside* entities: a streamed cell is **one entity** (or one resource entry) owning a `Vec` of instance data, not 50 000 entities, unless each instance needs ECS-visible behavior (physics, AI). Millions of static vegetation instances should be **instance buffers**, not entities (see `vegetation-rendering`, `open-world`).
- Separate sim from render: gameplay systems write components; extraction copies a *minimal* render representation (see `bevy-rendering`).
- System parameters: take `Res`/`ResMut` only where needed; `ResMut` serializes systems that touch it. Split large resources by access pattern.
- Change detection first: `Changed<GlobalTransform>`-style filters beat full scans. Don't iterate all entities each frame to find the ones that moved.
- Name system sets for public ordering (`StreamingSystems::Plan`, `StreamingSystems::Poll`).

## Implementation patterns
- State machine per streamed object as an enum component + one system per transition, driven by budgets (`bevy-world-streaming`).
- Spatial index as a `Resource` (grid `HashMap<CellCoord, Entity>`); keep it in sync with spawn/despawn hooks (`on_add`/`on_remove` component hooks, see `#[component(on_add = …)]` on `MeshletMesh3d`).
- Use `bevy_utils::Parallel<Vec<T>>` for parallel producers → single merge system.

## Common mistakes
- One system with `ResMut<Everything>` → no parallelism.
- Per-frame `Query` over millions of entities for something that changes rarely.
- Storing handles/IDs and re-resolving through a `HashMap` every frame instead of storing the `Entity`.
- Mutating `Visibility`/`Transform` of thousands of entities per frame for LOD swaps (use `VisibilityRange` or GPU LOD instead — `bevy-visibility`).
- Depending on query iteration order.

## Performance considerations
Archetype fragmentation: avoid adding/removing many marker components per entity per frame (moves it between tables). Use enum-valued components or sparse-set storage (`#[component(storage = "SparseSet")]`) for frequently toggled markers.

## Testing requirements
Use `App::new()` + `app.world_mut()` + `app.update()` in unit tests (`MinimalPlugins`); drive time via `Time<Virtual>` manual advance for determinism.

## References
- `crates/bevy_ecs/src/{system,query,schedule,message,observer}`, `crates/bevy_app/src`
- Examples: `examples/ecs/*`; stress tests `examples/stress_tests/many_*.rs`; `examples/large_scenes/bevy_city`
