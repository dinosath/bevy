---
name: procedural-worlds
description: Deterministic procedural generation for large worlds — seeded per-cell generation, density maps, instance placement, hashing for stable IDs, and offline bake vs runtime generation. Use when generating forests, cities, props or terrain content.
---

# Procedural worlds

## Purpose
Generate huge content sets that are **reproducible, cell-local and bakeable**, so streaming and saves can rely on them.

## Core rules
- **Cell-local determinism**: content of cell `c` is a pure function `gen(world_seed, c, config)`. Never depend on neighbor generation order or global RNG state. Derive per-cell and per-instance RNG by hashing: `seed_c = hash(world_seed, c.x, c.z)`, `id_i = hash(seed_c, species, i)`. Use a fixed, documented hash/RNG (`rand_chacha::ChaCha8Rng` seeded from a stable 64-bit mix) — **not** `std` `HashMap` order, not `Instant`, not thread count dependent.
- **Border consistency**: features crossing cell borders (roads, rivers, forest edges) derive from *continuous fields* (noise, splines) sampled in world space, not per-cell randoms; instances are assigned to the cell containing their position, and generation reads a margin around the cell so edges match.
- **Placement**: density map (noise × biome × slope × distance-to-road mask) → Poisson-disk / jittered grid (stratified, O(n), cache friendly) → species by weighted choice → scale/rotation jitter. Jittered grid on a cell-aligned lattice guarantees uniform deterministic coverage and trivial neighbor margins.
- **Stable IDs** for save deltas and gameplay references: `InstanceId(u64)` from the hash above; sorting by `(morton(x,z), id)` gives reproducible bytes.
- **Bake vs runtime**: bake in the asset pipeline (`asset-pipelines`) so runtime reads instance tables; runtime generation only for cheap, small things (grass detail within ~50 m) on `AsyncComputeTaskPool`, still deterministic.
- **Roads first**: generate the road network graph/splines globally (low-res, resident), then terrain carving and placement masks derive from it (route-aware streaming reads the same graph).

## Project conventions
- Generators are pure functions returning plain data (`Vec<Instance>`); spawning is separate.
- Parameter structs are `serde`-able and hashed into the bake manifest (changing a parameter invalidates cached cells).
- Tests: generate the same cell twice ⇒ identical; generate with thread counts 1 and N ⇒ identical; neighbor margins match.

## Common mistakes
- Using a single RNG advanced across cells (order dependent).
- f32 world coordinates in noise sampling far from origin (use cell + local, or f64).
- Float-to-int cell assignment using `as` on negatives (truncation toward zero ≠ floor): use `floor`.
- Non-hermetic generators (system time, env).

## Performance considerations
O(instances) with no allocations beyond the output `Vec` (reserve from expected density). Generate in parallel *across cells*, sequentially within a cell.

## Testing requirements
Determinism hash, golden snapshot of a small cell, statistical test of density (±tolerance), no overlapping instances closer than min-radius.

## References
`crates/bevy_world_streaming/src/sim.rs` (synthetic world generator for benchmarks), `examples/large_scenes/bevy_city` (procedural city), `rust` skill (determinism)
