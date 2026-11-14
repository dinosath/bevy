//! Deterministic dense-forest benchmark.
//!
//! Builds a procedural forest of high-poly, alpha-tested trees (3 species × 3 mesh LODs), drives a
//! scripted camera through it for a fixed number of frames and reports CPU frame time percentiles,
//! GPU pass times (when timestamp queries are available), visible entities and estimated triangles.
//!
//! Each optimisation is an **independent switch** so it can be measured on its own:
//!
//! * `--lod`          per-tree mesh LOD chain with `VisibilityRange` crossfades
//! * `--hlod`         forest HLOD: distant trees replaced by one merged proxy per region
//! * `--occlusion`    GPU two-phase occlusion culling (`DepthPrepass` + `OcclusionCulling`)
//! * `--no-shadows`   disable shadow maps (isolates shadow cost)
//! * `--no-cpu-culling` rely on GPU culling only
//!
//! ```text
//! cargo run --release -p forest_benchmark -- --label baseline
//! cargo run --release -p forest_benchmark -- --label lod --lod
//! ```
//!
//! The camera advances a fixed distance per frame, so every run sees the same frames regardless of
//! FPS. Results are appended to `--out` (CSV) and printed as a markdown row.
#![allow(
    clippy::print_stdout,
    clippy::disallowed_methods,
    clippy::std_instead_of_alloc,
    clippy::needless_range_loop,
    clippy::collapsible_if,
    reason = "benchmark binary, not engine code"
)]

use argh::FromArgs;
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::{NoCpuCulling, ViewVisibility, VisibilityRange},
    core_pipeline::prepass::DepthPrepass,
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    light::CascadeShadowConfigBuilder,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    render::{
        diagnostic::RenderDiagnosticsPlugin,
        occlusion_culling::OcclusionCulling,
        render_resource::{Extent3d, TextureDimension, TextureFormat},
    },
    window::{PresentMode, WindowResolution},
    winit::WinitSettings,
};
use bevy_world_streaming::hlod::{build_forest_hlod, ForestHlodConfig, TreeInstance};
use std::{collections::BTreeMap, fs::OpenOptions, io::Write as _, time::Instant};

/// Dense procedural forest benchmark.
#[derive(FromArgs, Resource, Clone)]
struct Args {
    /// label written to the results
    #[argh(option, default = "String::from(\"run\")")]
    label: String,
    /// approximate number of trees
    #[argh(option, default = "12000")]
    trees: usize,
    /// tree grid spacing in meters
    #[argh(option, default = "14.0")]
    spacing: f32,
    /// camera path: drive | aerial | static
    #[argh(option, default = "String::from(\"drive\")")]
    path: String,
    /// camera speed in km/h
    #[argh(option, default = "90.0")]
    speed_kmh: f32,
    /// measured frames
    #[argh(option, default = "600")]
    frames: u32,
    /// warm-up frames (not measured)
    #[argh(option, default = "120")]
    warmup: u32,
    /// window width
    #[argh(option, default = "1280")]
    width: u32,
    /// window height
    #[argh(option, default = "720")]
    height: u32,
    /// CSV results file (appended)
    #[argh(option, default = "String::from(\"forest_results.csv\")")]
    out: String,
    /// enable per-tree LOD chain
    #[argh(switch)]
    lod: bool,
    /// enable forest HLOD
    #[argh(switch)]
    hlod: bool,
    /// enable GPU occlusion culling
    #[argh(switch)]
    occlusion: bool,
    /// disable shadows
    #[argh(switch)]
    no_shadows: bool,
    /// disable CPU culling (GPU culling only)
    #[argh(switch)]
    no_cpu_culling: bool,
    /// render only opaque high-poly wood (trunk + branches, ~23k tris/tree) through the standard path
    #[argh(switch)]
    wood: bool,
    /// like --wood but through Bevy's meshlet renderer (needs `--features meshlet` and GPU support)
    #[argh(switch)]
    meshlet: bool,
    /// maximum triangles per meshlet (4..=128, multiple of 4 recommended)
    #[argh(option, default = "128")]
    meshlet_tris: u32,
    /// maximum vertices per meshlet (3..=256)
    #[argh(option, default = "256")]
    meshlet_verts: u32,
    /// number of shadow cascades
    #[argh(option, default = "3")]
    cascades: usize,
    /// maximum shadow distance in meters
    #[argh(option, default = "300.0")]
    shadow_distance: f32,
    /// directional shadow map resolution (per cascade)
    #[argh(option, default = "2048")]
    shadow_size: usize,
    /// per-cascade multiplier step for meshlet shadow LOD (cascade i uses scale * step^i)
    #[argh(option, default = "1.0")]
    cascade_step: f32,
    /// meshlet shadow LOD error scale (1 = same as main view)
    #[argh(option, default = "1.0")]
    shadow_lod_scale: f32,
    /// pretend the GPU lacks 64-bit image atomics (tests the meshlet capability fallback)
    #[argh(switch)]
    disable_int64_atomics: bool,
    /// meshlet cull queue slots (small values force overflow to test the pressure controller)
    #[argh(option, default = "4194304")]
    cluster_slots: u32,
    /// disable queue pressure detection entirely
    #[argh(switch)]
    no_pressure_probe: bool,
    /// disable automatic LOD degradation on queue pressure
    #[argh(switch)]
    no_degrade: bool,
    /// use the retained meshlet instance table (incremental updates)
    #[argh(switch)]
    retained: bool,
    /// validate the retained table against a from-scratch rebuild every frame (slow)
    #[argh(switch)]
    validate: bool,
    /// fraction (0..1) of meshlet trees that move every frame
    #[argh(option, default = "0.0")]
    dynamic_fraction: f32,
    /// meshlet trees despawned and respawned per frame
    #[argh(option, default = "0")]
    churn: u32,
    /// restore the extra shadow depth-pyramid rebuild after the meshlet second pass
    #[argh(switch)]
    rebuild_shadow_hzb: bool,
    /// save a screenshot at this measured frame (0 = off)
    #[argh(option, default = "0")]
    screenshot_frame: u32,
    /// screenshot output path
    #[argh(option, default = "String::from(\"forest.png\")")]
    screenshot_path: String,
    /// number of GPU spans to print
    #[argh(option, default = "12")]
    top_spans: usize,
}

/// Distance (m) at which individual trees give way to forest HLOD.
const HLOD_START: f32 = 350.0;
const LOD_SWITCH: [f32; 2] = [40.0, 120.0];
const MARGIN: f32 = 5.0;

#[derive(Component)]
struct TreeLod {
    triangles: u32,
}

#[derive(Resource, Default)]
struct SceneStats {
    entities: usize,
    trees: usize,
    hlod_regions: usize,
    bake_ms: f64,
    scene_tris_lod0: u64,
    meshlet_stats: String,
}

#[derive(Resource)]
struct Recorder {
    frame: u32,
    cpu_ms: Vec<f32>,
    visible: Vec<u32>,
    tris: Vec<u64>,
    gpu: BTreeMap<String, Vec<f64>>,
    started: Option<Instant>,
}

fn main() {
    let args: Args = argh::from_env();
    #[cfg(not(feature = "meshlet"))]
    assert!(
        !args.meshlet,
        "rebuild with `--features meshlet` to use --meshlet"
    );
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins
            .set(bevy::render::RenderPlugin {
                render_creation: bevy::render::settings::RenderCreation::Automatic(Box::new(
                    bevy::render::settings::WgpuSettings {
                        disabled_features: args
                            .disable_int64_atomics
                            .then_some(bevy::render::settings::WgpuFeatures::TEXTURE_INT64_ATOMIC),
                        ..default()
                    },
                )),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    present_mode: PresentMode::AutoNoVsync,
                    resolution: WindowResolution::new(args.width, args.height)
                        .with_scale_factor_override(1.0),
                    title: format!("forest_benchmark: {}", args.label),
                    ..default()
                }),
                ..default()
            }),
        FrameTimeDiagnosticsPlugin::default(),
        RenderDiagnosticsPlugin,
    ))
    .insert_resource(WinitSettings::continuous());
    #[cfg(feature = "meshlet")]
    if args.meshlet {
        app.add_plugins(bevy::pbr::experimental::meshlet::MeshletPlugin {
            cluster_buffer_slots: args.cluster_slots,
        });
        app.insert_resource(bevy::pbr::experimental::meshlet::MeshletSettings {
            rebuild_shadow_depth_pyramid_after_second_pass: args.rebuild_shadow_hzb,
            shadow_lod_error_scale: args.shadow_lod_scale,
            shadow_cascade_lod_error_scale_step: args.cascade_step,
            degrade_on_queue_pressure: !args.no_degrade,
            detect_queue_pressure: !args.no_pressure_probe,
            retained_instances: args.retained,
            validate_retained_instances: args.validate,
        });
    }
    app.insert_resource(bevy::light::DirectionalLightShadowMap {
        size: args.shadow_size,
    });
    app.insert_resource(args)
        .init_resource::<SceneStats>()
        .insert_resource(Recorder {
            frame: 0,
            cpu_ms: Vec::new(),
            visible: Vec::new(),
            tris: Vec::new(),
            gpu: BTreeMap::new(),
            started: None,
        })
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (animate_dynamic, churn_trees, move_camera, record).chain(),
        )
        .run();
}

// ---------- tiny deterministic RNG ----------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

// ---------- procedural tree geometry ----------

#[derive(Default)]
struct Builder {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    col: Vec<[f32; 4]>,
    idx: Vec<u32>,
}

impl Builder {
    /// Tapered tube; UVs sample the opaque bark half (u < 0.5) of the atlas.
    fn tube(
        &mut self,
        base: Vec3,
        dir: Vec3,
        len: f32,
        r0: f32,
        r1: f32,
        sides: u32,
        rings: u32,
        color: [f32; 4],
    ) {
        let dir = dir.normalize();
        let helper = if dir.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let right = dir.cross(helper).normalize();
        let up = right.cross(dir);
        let start = self.pos.len() as u32;
        for i in 0..=rings {
            let t = i as f32 / rings as f32;
            let c = base + dir * (len * t);
            let r = r0 + (r1 - r0) * t;
            for j in 0..=sides {
                let a = j as f32 / sides as f32 * std::f32::consts::TAU;
                let n = right * a.cos() + up * a.sin();
                self.pos.push((c + n * r).to_array());
                self.nrm.push(n.to_array());
                self.uv.push([0.05 + 0.4 * (j as f32 / sides as f32), t]);
                self.col.push(color);
            }
        }
        let row = sides + 1;
        for i in 0..rings {
            for j in 0..sides {
                let a = start + i * row + j;
                let b = a + row;
                self.idx.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
    }

    /// Alpha-tested leaf card; UVs sample the leaf half (u >= 0.5) of the atlas.
    fn leaf(&mut self, center: Vec3, right: Vec3, up: Vec3, color: [f32; 4]) {
        let n = right.cross(up).normalize();
        let s = self.pos.len() as u32;
        for (dx, dy, u, v) in [
            (-1.0, -1.0, 0.55, 0.05),
            (1.0, -1.0, 0.95, 0.05),
            (1.0, 1.0, 0.95, 0.95),
            (-1.0, 1.0, 0.55, 0.95),
        ] {
            self.pos.push((center + right * dx + up * dy).to_array());
            self.nrm.push(n.to_array());
            self.uv.push([u, v]);
            self.col.push(color);
        }
        self.idx
            .extend_from_slice(&[s, s + 1, s + 2, s, s + 2, s + 3]);
    }

    fn finish(self) -> (Mesh, u32) {
        let tris = (self.idx.len() / 3) as u32;
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.nrm)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.col)
        .with_inserted_indices(Indices::U32(self.idx));
        (mesh, tris)
    }
}

/// Species parameters: (height, canopy radius, leaf size, leaf colour).
const SPECIES: [(f32, f32, f32, [f32; 4]); 3] = [
    (13.0, 4.5, 0.32, [0.18, 0.45, 0.14, 1.0]),
    (9.0, 3.4, 0.28, [0.30, 0.50, 0.12, 1.0]),
    (16.0, 3.0, 0.36, [0.10, 0.34, 0.16, 1.0]),
];
const BARK: [f32; 4] = [0.30, 0.20, 0.12, 1.0];

/// Builds one tree mesh. `lod` 0 = high poly (~8k tris), 1 = ~1k, 2 = ~60.
fn build_tree(species: usize, lod: u32, seed: u64) -> (Mesh, u32) {
    let (h, cr, ls, lc) = SPECIES[species];
    let mut rng = Rng(seed ^ 0x9E37);
    let mut b = Builder::default();
    let trunk_h = h * 0.35;
    let (sides, rings, branches, leaves) = match lod {
        0 => (12, 16, 40, 2800),
        1 => (8, 4, 10, 180),
        _ => (4, 1, 0, 0),
    };
    b.tube(
        Vec3::ZERO,
        Vec3::Y,
        h * 0.8,
        0.35 * cr / 4.5 + 0.1,
        0.05,
        sides,
        rings,
        BARK,
    );
    if lod < 2 {
        let (bs, br) = if lod == 0 { (6, 4) } else { (4, 1) };
        for _ in 0..branches {
            let y = rng.range(trunk_h * 0.6, h * 0.75);
            let a = rng.range(0.0, std::f32::consts::TAU);
            let d = Vec3::new(a.cos(), rng.range(0.3, 0.9), a.sin());
            b.tube(
                Vec3::Y * y,
                d,
                rng.range(0.4, 0.8) * cr,
                0.07,
                0.01,
                bs,
                br,
                BARK,
            );
        }
        let k = if lod == 0 { 1.0 } else { 3.0 };
        for _ in 0..leaves {
            // Random point in the canopy ellipsoid.
            let p = loop {
                let v = Vec3::new(
                    rng.range(-1.0, 1.0),
                    rng.range(-1.0, 1.0),
                    rng.range(-1.0, 1.0),
                );
                if v.length_squared() <= 1.0 {
                    break v;
                }
            };
            let center = Vec3::new(
                p.x * cr,
                trunk_h + (p.y * 0.5 + 0.5) * (h - trunk_h),
                p.z * cr,
            );
            let a = rng.range(0.0, std::f32::consts::TAU);
            let t = rng.range(0.0, std::f32::consts::PI);
            let right =
                Vec3::new(a.cos() * t.cos(), t.sin(), a.sin() * t.cos()).normalize() * ls * k;
            let up = right
                .cross(Vec3::new(rng.range(-1.0, 1.0), 1.0, rng.range(-1.0, 1.0)).normalize())
                .normalize()
                * ls
                * k;
            let tint = rng.range(0.85, 1.15);
            b.leaf(
                center,
                right,
                up,
                [lc[0] * tint, lc[1] * tint, lc[2] * tint, 1.0],
            );
        }
    } else {
        // Canopy blob as a few big leaf cards (cheap, still alpha tested).
        for i in 0..6 {
            let a = i as f32 / 6.0 * std::f32::consts::PI;
            let right = Vec3::new(a.cos(), 0.0, a.sin()) * cr;
            let up = Vec3::Y * (h - trunk_h) * 0.5;
            b.leaf(Vec3::Y * (trunk_h + up.y), right, up, lc);
        }
    }
    b.finish()
}

/// Opaque high-poly wood: trunk plus many branches, no leaves (~23k triangles).
fn build_wood(species: usize, seed: u64) -> (Mesh, u32) {
    let (h, cr, _, _) = SPECIES[species];
    let mut rng = Rng(seed ^ 0xB00D);
    let mut b = Builder::default();
    b.tube(
        Vec3::ZERO,
        Vec3::Y,
        h * 0.8,
        0.45 * cr / 4.5 + 0.1,
        0.04,
        24,
        48,
        BARK,
    );
    for _ in 0..220 {
        let y = rng.range(h * 0.2, h * 0.75);
        let a = rng.range(0.0, std::f32::consts::TAU);
        let d = Vec3::new(a.cos(), rng.range(0.3, 0.9), a.sin());
        b.tube(
            Vec3::Y * y,
            d,
            rng.range(0.4, 1.0) * cr,
            0.07,
            0.01,
            8,
            6,
            BARK,
        );
    }
    b.finish()
}

fn make_atlas() -> Image {
    const W: usize = 128;
    const H: usize = 64;
    let mut data = vec![255u8; W * H * 4];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            if x >= W / 2 {
                // Leaf: ellipse with a pointed look, alpha 0 outside.
                let u = ((x - W / 2) as f32 + 0.5) / (W / 2) as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / H as f32 * 2.0 - 1.0;
                let inside = u * u * 1.0 + v * v * 0.8 < 0.8;
                data[i + 3] = if inside { 255 } else { 0 };
            }
        }
    }
    Image::new(
        Extent3d {
            width: W as u32,
            height: H as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn range_for(lod: usize, args: &Args) -> VisibilityRange {
    // LOD ends: with --lod [0..40, 40..120, 120..HLOD/inf]; without --lod only LOD0 exists.
    let far_end = if args.hlod { HLOD_START } else { 1.0e6 };
    let (start, end) = match (args.lod, lod) {
        (false, _) => (0.0, far_end),
        (true, 0) => (0.0, LOD_SWITCH[0]),
        (true, 1) => (LOD_SWITCH[0], LOD_SWITCH[1]),
        (true, _) => (LOD_SWITCH[1], far_end),
    };
    let start_m = if start == 0.0 {
        0.0..0.0
    } else {
        (start - MARGIN)..start
    };
    let end_m = if end >= 1.0e5 {
        end..end
    } else {
        end..(end + MARGIN)
    };
    VisibilityRange {
        start_margin: start_m,
        end_margin: end_m,
        use_aabb: false,
    }
}

fn setup(
    mut commands: Commands,
    args: Res<Args>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut stats: ResMut<SceneStats>,
    mut meshlet_assets: MeshletAssets,
) {
    let atlas = images.add(make_atlas());
    let tree_mat = materials.add(StandardMaterial {
        base_color_texture: Some(atlas),
        alpha_mode: AlphaMode::Mask(0.5),
        cull_mode: None,
        double_sided: true,
        perceptual_roughness: 0.9,
        ..default()
    });
    let proxy_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.14, 0.36, 0.13),
        perceptual_roughness: 1.0,
        cull_mode: None,
        ..default()
    });

    // Mesh per (species, lod).
    let mut handles: Vec<Vec<(Handle<Mesh>, u32)>> = Vec::new();
    for s in 0..SPECIES.len() {
        let mut row = Vec::new();
        for lod in 0..3 {
            let (m, tris) = build_tree(s, lod, 1000 + s as u64 * 7 + u64::from(lod));
            row.push((meshes.add(m), tris));
        }
        handles.push(row);
    }

    // Deterministic jittered-grid placement with an empty road corridor along +X at z = 0.
    let side = (args.trees as f32).sqrt().ceil() as i32;
    let half = side as f32 * args.spacing * 0.5;
    let mut rng = Rng(0xF0_2E57);
    let mut instances: Vec<(Vec3, f32, f32, usize)> = Vec::new();
    'outer: for gz in 0..side {
        for gx in 0..side {
            if instances.len() >= args.trees {
                break 'outer;
            }
            let x = -half + (gx as f32 + rng.range(0.1, 0.9)) * args.spacing;
            let z = -half + (gz as f32 + rng.range(0.1, 0.9)) * args.spacing;
            if z.abs() < 9.0 {
                continue;
            }
            let s = ((rng.next() * 3.0) as usize).min(2);
            instances.push((
                Vec3::new(x, 0.0, z),
                rng.range(0.0, std::f32::consts::TAU),
                rng.range(0.8, 1.3),
                s,
            ));
        }
    }

    if args.wood || args.meshlet {
        spawn_wood(
            &mut commands,
            &mut meshlet_assets,
            &args,
            &mut meshes,
            &mut materials,
            &instances,
            &mut stats,
        );
        spawn_world(
            &mut commands,
            &args,
            &mut meshes,
            &mut materials,
            &mut stats,
            half,
        );
        return;
    }

    let lods = if args.lod { 3 } else { 1 };
    for &(p, yaw, scale, s) in &instances {
        for lod in 0..lods {
            let (h, tris) = handles[s][lod].clone();
            commands.spawn((
                Mesh3d(h),
                MeshMaterial3d(tree_mat.clone()),
                Transform::from_translation(p)
                    .with_rotation(Quat::from_rotation_y(yaw))
                    .with_scale(Vec3::splat(scale)),
                range_for(lod, &args),
                TreeLod { triangles: tris },
            ));
        }
        stats.scene_tris_lod0 += u64::from(handles[s][0].1);
    }
    stats.trees = instances.len();

    if args.hlod {
        let t0 = Instant::now();
        let trees: Vec<TreeInstance> = instances
            .iter()
            .map(|&(p, _, scale, s)| TreeInstance {
                position: p,
                species: s as u16,
                trunk_height: SPECIES[s].0 * 0.35 * scale,
                canopy_radius: SPECIES[s].1 * scale,
                canopy_height: SPECIES[s].0 * 0.65 * scale,
            })
            .collect();
        let (regions, _) = build_forest_hlod(&trees, &ForestHlodConfig::default());
        stats.hlod_regions = regions.len();
        for mut r in regions {
            let (c, _) = r.bounding_sphere();
            for p in &mut r.geometry.positions {
                p[0] -= c.x;
                p[1] -= c.y;
                p[2] -= c.z;
            }
            let tris = r.geometry.triangle_count() as u32;
            commands.spawn((
                Mesh3d(meshes.add(r.geometry.into_mesh())),
                MeshMaterial3d(proxy_mat.clone()),
                Transform::from_translation(c),
                VisibilityRange {
                    start_margin: (HLOD_START - MARGIN)..HLOD_START,
                    end_margin: 1.0e6..1.0e6,
                    use_aabb: false,
                },
                TreeLod { triangles: tris },
            ));
        }
        stats.bake_ms = t0.elapsed().as_secs_f64() * 1e3;
    }

    spawn_world(
        &mut commands,
        &args,
        &mut meshes,
        &mut materials,
        &mut stats,
        half,
    );
    stats.entities = commands_len_hint(&instances, lods, stats.hlod_regions);
}

fn spawn_world(
    commands: &mut Commands,
    args: &Args,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    _stats: &mut SceneStats,
    half: f32,
) {
    let ground_mat = materials.add(Color::srgb(0.12, 0.25, 0.10));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(half * 4.0, half * 4.0))),
        MeshMaterial3d(ground_mat),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: light_consts::lux::FULL_DAYLIGHT,
            shadow_maps_enabled: !args.no_shadows,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::ZYX, 0.0, 0.8, -0.9)),
        CascadeShadowConfigBuilder {
            num_cascades: args.cascades,
            maximum_distance: args.shadow_distance,
            first_cascade_far_bound: 25.0,
            ..default()
        }
        .build(),
    ));
    let mut cam = commands.spawn((
        Camera3d::default(),
        Msaa::Off,
        Transform::from_xyz(-half + 20.0, 1.8, 0.0).looking_to(Vec3::X, Vec3::Y),
    ));
    if args.occlusion {
        cam.insert((DepthPrepass, OcclusionCulling));
    }
    if args.no_cpu_culling {
        cam.insert(NoCpuCulling);
    }
}

#[cfg(feature = "meshlet")]
type MeshletAssets<'w> = Option<ResMut<'w, Assets<bevy::pbr::experimental::meshlet::MeshletMesh>>>;
#[cfg(not(feature = "meshlet"))]
type MeshletAssets<'w> = ();

fn spawn_wood(
    commands: &mut Commands,
    meshlet_assets: &mut MeshletAssets,
    args: &Args,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    instances: &[(Vec3, f32, f32, usize)],
    stats: &mut SceneStats,
) {
    let wood_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.30, 0.20, 0.12),
        perceptual_roughness: 0.9,
        ..default()
    });
    let mut std_handles = Vec::new();
    let mut tris = Vec::new();
    #[cfg(feature = "meshlet")]
    let mut meshlet_handles = Vec::new();
    for s in 0..SPECIES.len() {
        let (mut mesh, t) = build_wood(s, 77 + s as u64);
        // Meshlet baking accepts exactly position/normal/uv0; use the same data on both paths.
        mesh.remove_attribute(Mesh::ATTRIBUTE_COLOR);
        tris.push(t);
        if args.meshlet {
            #[cfg(feature = "meshlet")]
            {
                let t0 = Instant::now();
                let mm = bevy::pbr::experimental::meshlet::MeshletMesh::from_mesh_with_config(
                    &mesh,
                    bevy::pbr::experimental::meshlet::MESHLET_DEFAULT_VERTEX_POSITION_QUANTIZATION_FACTOR,
                    bevy::pbr::experimental::meshlet::MeshletBuildConfig {
                        max_vertices: args.meshlet_verts,
                        max_triangles: args.meshlet_tris,
                    },
                )
                .expect("meshlet bake");
                let st = mm.stats();
                stats.meshlet_stats.push_str(&format!(
                    "species{s}: src_tris={t} meshlets={} all_lod_tris={} bvh_nodes={} bvh_depth={} raw_bytes={} ({:.1} B/src-tri) lz4_bytes={} ({:.1} B/src-tri)\n",
                    st.meshlets, st.triangles, st.bvh_nodes, st.bvh_depth, st.total_bytes(),
                    st.total_bytes() as f64 / f64::from(t), st.lz4_file_bytes, st.lz4_file_bytes as f64 / f64::from(t)
                ));
                stats.bake_ms += t0.elapsed().as_secs_f64() * 1e3;
                meshlet_handles.push(
                    meshlet_assets
                        .as_mut()
                        .expect("MeshletPlugin added")
                        .add(mm),
                );
            }
        } else {
            std_handles.push(meshes.add(mesh));
        }
    }
    let mut churn_entities: Vec<Entity> = Vec::new();
    for (i, &(p, yaw, scale, s)) in instances.iter().enumerate() {
        let tf = Transform::from_translation(p)
            .with_rotation(Quat::from_rotation_y(yaw))
            .with_scale(Vec3::splat(scale));
        // Evenly spread subset of instances that move every frame.
        let f = args.dynamic_fraction.clamp(0.0, 1.0);
        let dynamic = f > 0.0 && ((i as f32 + 1.0) * f).floor() > (i as f32 * f).floor();
        let mut e = if args.meshlet {
            #[cfg(feature = "meshlet")]
            {
                commands.spawn((
                    bevy::pbr::experimental::meshlet::MeshletMesh3d(meshlet_handles[s].clone()),
                    MeshMaterial3d(wood_mat.clone()),
                    tf,
                ))
            }
            #[cfg(not(feature = "meshlet"))]
            {
                unreachable!("--meshlet requires the meshlet feature")
            }
        } else {
            commands.spawn((
                Mesh3d(std_handles[s].clone()),
                MeshMaterial3d(wood_mat.clone()),
                tf,
                TreeLod { triangles: tris[s] },
            ))
        };
        if dynamic {
            e.insert(Dynamic {
                base: p,
                phase: i as f32 * 0.37,
            });
        }
        if args.churn > 0 {
            churn_entities.push(e.id());
        }
        stats.scene_tris_lod0 += u64::from(tris[s]);
    }
    #[cfg(feature = "meshlet")]
    if args.meshlet && args.churn > 0 {
        commands.insert_resource(ChurnState {
            entities: churn_entities,
            meshes: meshlet_handles.clone(),
            material: wood_mat.clone(),
            rng: Rng(0xC4_0123),
            half: instances
                .iter()
                .map(|i| i.0.x.abs().max(i.0.z.abs()))
                .fold(0.0, f32::max),
        });
    }
    stats.trees = instances.len();
    stats.entities = instances.len() + 3;
}

fn commands_len_hint(instances: &[(Vec3, f32, f32, usize)], lods: usize, regions: usize) -> usize {
    instances.len() * lods + regions + 3
}

fn move_camera(args: Res<Args>, rec: Res<Recorder>, mut q: Query<&mut Transform, With<Camera3d>>) {
    let Ok(mut t) = q.single_mut() else { return };
    let side = (args.trees as f32).sqrt().ceil();
    let half = side * args.spacing * 0.5;
    let step = args.speed_kmh / 3.6 / 60.0; // fixed distance per frame
    let travelled = rec.frame as f32 * step;
    let x = -half + 20.0 + travelled;
    match args.path.as_str() {
        "static" => {
            *t = Transform::from_xyz(-half + 220.0, 1.8, 0.0).looking_to(Vec3::X, Vec3::Y);
        }
        "aerial" => {
            *t = Transform::from_xyz(x, 150.0, 0.0).looking_to(Vec3::new(1.0, -0.35, 0.0), Vec3::Y);
        }
        _ => {
            *t = Transform::from_xyz(x, 1.8, 0.0).looking_to(
                Vec3::new(1.0, 0.0, 0.25 * (travelled * 0.05).sin()),
                Vec3::Y,
            );
        }
    }
}

fn record(
    args: Res<Args>,
    time: Res<Time<Real>>,
    diag: Res<DiagnosticsStore>,
    vis: Query<(&ViewVisibility, &TreeLod)>,
    stats: Res<SceneStats>,
    mut rec: ResMut<Recorder>,
    mut exit: MessageWriter<AppExit>,
    mut commands: Commands,
) {
    rec.frame += 1;
    if args.screenshot_frame > 0 && rec.frame == args.warmup + args.screenshot_frame {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(
                args.screenshot_path.clone(),
            ));
    }
    if rec.frame <= args.warmup {
        return;
    }
    if rec.started.is_none() {
        rec.started = Some(Instant::now());
    }
    rec.cpu_ms.push(time.delta_secs() * 1000.0);
    let (mut n, mut tris) = (0u32, 0u64);
    for (v, t) in &vis {
        if v.get() {
            n += 1;
            tris += u64::from(t.triangles);
        }
    }
    rec.visible.push(n);
    rec.tris.push(tris);
    for d in diag.iter() {
        let path = d.path().to_string();
        if path.ends_with("elapsed_gpu") {
            if let Some(v) = d.value() {
                rec.gpu.entry(path).or_default().push(v);
            }
        }
    }

    if rec.cpu_ms.len() as u32 >= args.frames {
        report(&args, &stats, &rec);
        exit.write(AppExit::Success);
    }
}

fn pct(v: &mut [f32], p: f32) -> f32 {
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * p).round() as usize]
}

fn report(args: &Args, stats: &SceneStats, rec: &Recorder) {
    let mut cpu = rec.cpu_ms.clone();
    let mean = cpu.iter().sum::<f32>() / cpu.len() as f32;
    let (p50, p95, p99, max) = (
        pct(&mut cpu, 0.5),
        pct(&mut cpu, 0.95),
        pct(&mut cpu, 0.99),
        pct(&mut cpu, 1.0),
    );
    let vis = rec.visible.iter().map(|&v| v as f64).sum::<f64>() / rec.visible.len() as f64;
    let tris = rec.tris.iter().map(|&v| v as f64).sum::<f64>() / rec.tris.len() as f64;
    let mut gpu_avg: Vec<(String, f64)> = rec
        .gpu
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().sum::<f64>() / v.len() as f64))
        .collect();
    gpu_avg.sort_by(|a, b| b.1.total_cmp(&a.1));

    println!("\n=== forest_benchmark: {} ===", args.label);
    println!(
        "flags: lod={} hlod={} occlusion={} shadows={} cpu_culling={} path={} speed={}km/h trees={} res={}x{}",
        args.lod, args.hlod, args.occlusion, !args.no_shadows, !args.no_cpu_culling, args.path,
        args.speed_kmh, stats.trees, args.width, args.height
    );
    println!(
        "entities≈{} hlod_regions={} hlod_bake_ms={:.1} LOD0-tris-in-scene={:.1}M",
        stats.entities,
        stats.hlod_regions,
        stats.bake_ms,
        stats.scene_tris_lod0 as f64 / 1e6
    );
    println!(
        "| {} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.0} | {:.2}M |",
        args.label,
        1000.0 / mean,
        mean,
        p50,
        p95,
        p99,
        max,
        vis,
        tris / 1e6
    );
    if !stats.meshlet_stats.is_empty() {
        println!(
            "meshlet config: max_verts={} max_tris={}",
            args.meshlet_verts, args.meshlet_tris
        );
        print!("{}", stats.meshlet_stats);
    }
    println!("GPU spans (avg ms, top {}):", args.top_spans);
    for (k, v) in gpu_avg.iter().take(args.top_spans) {
        println!("  {k}: {v:.3}");
    }
    if gpu_avg.is_empty() {
        println!("  (no GPU timestamp diagnostics available)");
    }

    let new = !std::path::Path::new(&args.out).exists();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&args.out) {
        if new {
            let _ = writeln!(f, "label,fps,cpu_mean_ms,cpu_p50_ms,cpu_p95_ms,cpu_p99_ms,cpu_max_ms,visible_entities,tris_millions,gpu_spans");
        }
        let spans = gpu_avg
            .iter()
            .map(|(k, v)| format!("{k}={v:.3}"))
            .collect::<Vec<_>>()
            .join(";");
        let _ = writeln!(
            f,
            "{},{:.1},{:.3},{:.3},{:.3},{:.3},{:.3},{:.0},{:.3},\"{}\"",
            args.label,
            1000.0 / mean,
            mean,
            p50,
            p95,
            p99,
            max,
            vis,
            tris / 1e6,
            spans
        );
    }
}

/// Marks a tree that moves every frame (tests incremental instance updates).
#[derive(Component)]
struct Dynamic {
    base: Vec3,
    phase: f32,
}

fn animate_dynamic(rec: Res<Recorder>, mut q: Query<(&Dynamic, &mut Transform)>) {
    let t = rec.frame as f32;
    for (d, mut tf) in &mut q {
        tf.translation = d.base
            + Vec3::new(
                (t * 0.07 + d.phase).sin() * 1.5,
                0.0,
                (t * 0.05 + d.phase).cos() * 1.5,
            );
    }
}

#[cfg(feature = "meshlet")]
#[derive(Resource)]
struct ChurnState {
    entities: Vec<Entity>,
    meshes: Vec<Handle<bevy::pbr::experimental::meshlet::MeshletMesh>>,
    material: Handle<StandardMaterial>,
    rng: Rng,
    half: f32,
}

#[cfg(feature = "meshlet")]
fn churn_trees(args: Res<Args>, mut commands: Commands, state: Option<ResMut<ChurnState>>) {
    let Some(mut st) = state else { return };
    for _ in 0..args.churn {
        let i = ((st.rng.next() * st.entities.len() as f32) as usize).min(st.entities.len() - 1);
        commands.entity(st.entities[i]).despawn();
        let half = st.half;
        let p = Vec3::new(st.rng.range(-half, half), 0.0, st.rng.range(-half, half));
        let sp = ((st.rng.next() * 3.0) as usize).min(2);
        let tf = Transform::from_translation(p).with_scale(Vec3::splat(st.rng.range(0.8, 1.3)));
        let e = commands
            .spawn((
                bevy::pbr::experimental::meshlet::MeshletMesh3d(st.meshes[sp].clone()),
                MeshMaterial3d(st.material.clone()),
                tf,
            ))
            .id();
        st.entities[i] = e;
    }
}

#[cfg(not(feature = "meshlet"))]
fn churn_trees() {}
