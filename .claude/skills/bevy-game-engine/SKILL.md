---
name: bevy-game-engine
description: How to build a large game on top of Bevy in this repo — crate/plugin layout, where engine-level additions belong (upstream-able crate vs game crate), feature gating, examples and benchmarks. Use when starting new engine features or deciding project structure.
---

# Building a game engine layer on Bevy

## Purpose
Extend Bevy without forking its internals unnecessarily, and keep additions upstream-contributable.

## Where code goes
| Kind | Location | Rule |
|---|---|---|
| Renderer fixes/features everyone needs (e.g. meshlet page residency, GPU counters) | `crates/bevy_pbr`, `bevy_render`, … | small, additive PRs; format-version bumps; docs + release note in `_release-content/` |
| Engine-level systems independent of rendering (world partition, streaming policy, prediction, HLOD policy) | **new crate** `crates/bevy_world_streaming` | `#![forbid(unsafe_code)]`, minimal deps (`bevy_app/ecs/math/tasks/transform`), no GPU dependency in core modules; optional features for gizmos/mesh |
| Game content/rules | the game's own crates (outside Bevy) | depends on the public engine crates |
| Benchmarks/examples | crate `examples/` or `examples/large_scenes/*` | deterministic, documented in `docs/open_world/benchmarks.md` |

## Conventions
- A new crate: `Cargo.toml` with workspace lints, `version = "0.20.0-dev"`, license `MIT OR Apache-2.0`, README, `#![doc = include_str!("../README.md")]`. It is automatically a workspace member via `crates/*`.
- Wiring into the umbrella `bevy` crate (`bevy_internal` re-export + root feature) is a **separate, explicit step** — do it only when the crate is ready for upstream review; until then examples depend on `bevy` as a dev-dependency and the crate stays independent.
- Feature flags additive; heavy deps optional. Document in `docs/cargo_features.md` when wired into root.
- Public API: config structs with `Default`, `#[non_exhaustive]` where likely to grow, `Reflect` where Bevy conventions expect it (config resources, components).
- Prefer extending Bevy extension points: plugins, schedules/sets, `ExtractComponent`, `RenderAsset`, `AssetLoader/Saver/Process`, `Core3d` pass systems (`bevy-render-graph`).

## Process for an engine feature
1. Audit: does Bevy already do it? (`docs/open_world/bevy_audit.md`, grep, release notes) — don't duplicate.
2. Design note (short) in `docs/open_world/` stating invariants, budgets, failure modes.
3. Pure core + tests → ECS glue → example → benchmark with baseline numbers → docs/skill update.
4. Update the relevant skill(s) in `.claude/skills/` in the same change.

## Common mistakes
Editing renderer internals for what a plugin can do; shipping without baseline numbers; adding root-level features for experimental crates too early; forgetting that local rustc may lag `rust-version`.

## Testing requirements
`cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo fmt --all -- --check`, compile examples (`cargo check --example …`).

## References
`CONTRIBUTING.md`, `docs/linters.md`, `crates/bevy_internal/Cargo.toml`, `Cargo.toml` (features), `.claude/skills/*`
