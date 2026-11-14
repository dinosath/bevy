# GPU culling, LOD traversal, compaction, overflow

## Bevy facts [Bevy]
Pass structure per view (`visibility_buffer_raster_node.rs`): `clear_visibility_buffer` → early: `cull_instances` → `cull_bvh` (one indirect dispatch per BVH depth, 8 threads per node, 128/workgroup, ping-pong queues using the buffer's front/back via `rightmost_slot`) → `cull_clusters` (frustum, HZB, LOD, SW/HW raster split) → raster → HZB build → late pass re-tests what was occluded → resolve. `MeshletPlugin::cluster_buffer_slots` sizes two queues (`2 × slots × 4 B`).
* Culling tests in traversal: frustum on node AABB, LOD error (`lod_error_is_imperceptible`, hard 1 px), HZB occlusion (`should_occlusion_cull_aabb`). **No cone culling.**
* Queue appends: `atomicAdd` on counters; writes at `rightmost_slot - id` / `id` with **no capacity check and no overflow counter** in `cull_bvh.wesl`, `cull_clusters.wesl`, `cull_instances.wesl`.

## UE reference
[UE-doc] simple vs complex culling classes (`r.Nanite.Culling.Classification`, default 1); adaptive fanout (`r.Nanite.Culling.AdaptiveFanoutThreadBudget`, default 6144, 0 = one level per dispatch); `MaxCandidateClusters` 12 B and `MaxVisibleClusters` 16 B per entry, set in ini, *cannot resize at runtime*, overflow ⇒ missing/blinking geometry; two-pass stats Main/Post.

## Decisions (Parts 8, 10, 12)
| Question | Evidence | Decision |
|---|---|---|
| Adaptive fanout vs one level/dispatch vs persistent traversal | [Bench] full meshlet culling+raster ≈3.0–3.5 ms of a 5.4 ms no-shadow frame; 4 BVH levels at 12 k instances (`performance.md`) | **Do not implement now.** Dispatch-count overhead is bounded by `bvh_depth`≈4–5 here. Re-benchmark at ≥10⁵ instances and with 6+ shadow views; if `cull_bvh` per-level dispatch gaps exceed ~15 % of the pass, prototype fanout (`[Proposal]`: when remaining node count × 8 < thread budget, process N levels in one dispatch with workgroup-local queues) |
| Atomic append vs prefix-sum vs hierarchical compaction | Not measured | Keep atomics; add wave-aggregated append (`subgroupAdd` + one atomic per subgroup) **only if** a GPU span shows contention. Needs `SUBGROUP` (present: [Bench env]) |
| Simple/complex split | meshlets are rigid-static only | defer until skinned/spline/assembly instances exist |
| Cone culling | meshlet build computes the cone then discards it | add behind asset v4 (`cluster_hierarchy.md`); correctness test vs brute force |

## Overflow & pressure policy (Part 44) — **IMPLEMENTED for the meshlet cull queues** (`meshlet/pressure.rs`; results in `performance.md` §9)

The text below is the original design; what shipped differs in one respect: instead of a shader-side `atomicOr` flag it uses **GPU-side counter copies** into a probe buffer (no shader or bind-group changes), sampled for first-pass BVH levels and the early+late cluster pair; the policy constants are in `MeshletPressure::observe`. Not covered: second-pass BVH levels, SW-raster/visible-cluster buffers, and the non-queue pressure sources (streaming, shadow pages, RT), which remain design-only.

### Original design
Today: silent. The LOD error budget is now a per-dispatch immediate (`constants.lod_error_scale`, `cull_shared.wesl::lod_error_is_imperceptible(…, error_scale)`), exposed for shadow views as `MeshletSettings::shadow_lod_error_scale`; step 2 below only needs a *detector* and a per-view scale source. Design:
1. **Detect**: a 4-byte `overflow_flags` buffer; shaders `atomicOr` a bit when `id >= capacity` (candidate / visible / SW-raster / BVH queue) and **skip the write**. Read back asynchronously (`gpu_readback`, ≥1–2 frames latency) into a `MeshletStats` resource + diagnostics (also publishes `candidate_high_water`).
2. **Degrade**: a per-view `error_scale` uniform multiplies the LOD error threshold. On overflow the CPU raises `error_scale` (e.g. ×1.25 per flagged frame, decays ×0.98 per clean frame) ⇒ fewer clusters next frame. Quality ↓, no missing geometry.
3. **Grow** (optional): reallocate queues at frame boundary when high-water > 80 % for N frames, capped by a memory budget.
4. **Test**: shrink `cluster_buffer_slots` to force overflow in a headless GPU test; assert the flag is raised, `error_scale` rises, and visible-cluster count falls below capacity within K frames.
Other pressure sources (streaming overflow, IO starvation, decode backlog, shadow-page overflow, RT memory) follow the same ladder in their docs: *reduce distant detail → reduce RT quality → reduce shadow detail → reduce streaming priority*.

## GPU counters added (Part 43)
Shadow views now record nested spans `first_cull_and_raster`, `hzb_after_first_pass`, `second_cull_and_raster`, `resolve_depth`, `hzb_final` under `render/shadow_directional_light_N_cascade_M/…` (`visibility_buffer_raster_node.rs`). Measured breakdown: `performance.md` §7. Still missing: visible/culled/occluded *cluster* counts.

## Tests (Part 8)
Brute-force reference: CPU evaluates the same frustum/LOD predicates over all clusters of a seeded scene and compares with GPU-selected sets (superset allowed only for occlusion).
