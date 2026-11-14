# Cluster hierarchy

## Bevy facts [Bevy]
* Build (`crates/bevy_pbr/src/meshlet/from_mesh.rs`): METIS partitions triangles into meshlets of ≤(`max_triangles`−2) triangles → `meshopt::build_meshlets(max_vertices, max_triangles, cone_weight = 0.0)` → groups of ~8 meshlets (`TARGET_MESHLETS_PER_GROUP`) chosen by shared vertices → group borders locked → `simplify_with_attributes_and_locks` with `SimplifyOptions::Sparse | ErrorAbsolute` to ~50 % triangles → new meshlets with `parent_error` ≥ children's → repeat until one root; groups that keep >60 % of triangles are rejected (`SIMPLIFICATION_FAILURE_PERCENTAGE`). `BvhBuilder` then builds a **BVH8**.
* Node contents (`asset.rs`): per child `MeshletAabbErrorOffset { center, error, half_extent, child_offset }` + `MeshletBoundingSphere` for LOD + `child_counts: [u8; 8]` (255 ⇒ child is a BVH node, else N meshlets). Per meshlet: bit-packed position window + quantization factor, `vertex_count_minus_one`, `triangle_count`.
* **Not stored**: normal cone (the build computes meshopt bounds incl. cone but only the sphere is kept — `cull_shared.wesl` has no cone test), page id, residency state, material id (materials are per-instance).

## Cluster-size experiment — **[Bench]**
See `performance.md` §1. Table summary (3-cascade frame): 32 tris 58.4 ms, 64: 38.2, 96: 32.2, **128: 28.7**; main view unaffected (5.4–5.7 ms). Disk (lz4): 28.0 / 24.2 / 21.1 / 21.8 B per source triangle. BVH depth 5/4/4/4.
**Conclusion**: keep 128/256. 128 is also the hardware limit of the current software rasterizer (128 threads, 1 triangle each).
Not explored (**research required**): vertex cap < 256 (affects HW raster vertex reuse), meshopt `cone_weight > 0` (better backface culling clusters, needed only once cone culling exists), BVH arity ≠ 8, group size ≠ 8.

## Proposed hierarchy additions [Proposal]
| Field | Why | Cost |
|---|---|---|
| per-meshlet normal cone (axis + cutoff, 4 B) | backface/cone culling (matrix #3); most useful for closed solid meshes (buildings, vehicles), little for foliage cards (double-sided) | +4 B/meshlet (+<1 % size) |
| `page_id` per BVH child / meshlet range (u16–u32) | streaming granularity | +2–4 B/child |
| `material_range` / material slot | per-cluster material for multi-material assemblies | +1–2 B |
| `error` stays absolute meters (already) | supports runtime error scale knob | 0 |
| Assembly `InstanceGroup` node type | `foliage.md` | new node tag |

Compatibility: new fields go in a **new asset version** (v4) with a v3 loader shim. Cone culling must pass a brute-force "never culls a visible triangle" test.

## Tests required
Stats invariants (done), "cut is crack-free" (existing upstream behaviour — locked borders; add a visual/analytic test that adjacent LOD cuts share border vertices), monotone error up the hierarchy, bake determinism mode (see `performance.md` §5).
