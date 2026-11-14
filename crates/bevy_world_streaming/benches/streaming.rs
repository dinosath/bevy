//! Micro-benchmarks of the pure streaming core (no IO, no GPU).
//!
//! * `interest_build` / `scheduler_tick`: per-frame CPU cost at steady state (must be allocation-free
//!   and independent of world size).
//! * `forest_hlod_build`: aggregating 10 000 trees (bake/activation cost).
//! * `page_reconcile`: page residency decisions for 2 000 demanded assets.

use bevy_math::{DVec3, Vec3};
use bevy_world_streaming::{
    hlod::{build_forest_hlod, ForestHlodConfig, TreeInstance},
    pages::{AssetKey, AssetPages, PageDemand, PageTable},
    predict::{kmh_to_ms, InterestBuilder, MotionState, Route},
    scheduler::{Action, CellBytes, StreamingConfig, StreamingScheduler},
};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

fn motion(x: f64) -> MotionState {
    MotionState {
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::new(kmh_to_ms(200.0), 0.0, 0.0),
        acceleration: DVec3::ZERO,
    }
}

fn interest_build(c: &mut Criterion) {
    let cfg = StreamingConfig::default();
    let route = Route::new(vec![DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0)]).unwrap();
    let mut b = InterestBuilder::default();
    let mut x = 0.0;
    c.bench_function("interest_build/route_aware_200kmh", |bench| {
        bench.iter(|| {
            x += 1.0;
            black_box(b.build(&cfg, &motion(x), Some(&route), &[]).len())
        });
    });
}

fn scheduler_tick(c: &mut Criterion) {
    let cfg = StreamingConfig::default();
    let mut s = StreamingScheduler::new(cfg.clone());
    let mut b = InterestBuilder::default();
    let mut out = Vec::new();
    let bytes = CellBytes {
        cpu: 1 << 20,
        gpu: 1 << 20,
    };
    // Reach a steady state: complete everything instantly while moving at 200 km/h.
    let mut x = 0.0;
    let mut step = |s: &mut StreamingScheduler, x: f64, t: f64, out: &mut Vec<Action>| {
        let m = motion(x);
        let interest = b.build(&cfg, &m, None, &[]);
        s.tick(t, m.position, interest, out);
        for a in out.iter().copied() {
            match a {
                Action::StartIo { coord, generation } => {
                    s.io_complete(coord, generation, bytes);
                }
                Action::StartDecode { coord, generation } => {
                    s.decode_complete(coord, generation);
                }
                Action::StartUpload {
                    coord, generation, ..
                } => {
                    s.upload_complete(coord, generation);
                }
                _ => {}
            }
        }
    };
    for i in 0..2_000 {
        x += 0.9;
        step(&mut s, x, f64::from(i) / 60.0, &mut out);
    }
    let mut i = 2_000;
    c.bench_function(
        "scheduler_tick/interest+tick_steady_state_200kmh",
        |bench| {
            bench.iter(|| {
                x += 0.9;
                i += 1;
                step(&mut s, x, f64::from(i) / 60.0, &mut out);
                black_box(out.len())
            });
        },
    );
}

fn forest_hlod(c: &mut Criterion) {
    let trees: Vec<TreeInstance> = (0..10_000)
        .map(|i| TreeInstance {
            position: Vec3::new(
                (i as f32 * 0.618_034).fract() * 2048.0,
                0.0,
                (i as f32 * 0.754_877_7).fract() * 2048.0,
            ),
            species: (i % 4) as u16,
            trunk_height: 3.0,
            canopy_radius: 4.0,
            canopy_height: 9.0,
        })
        .collect();
    let cfg = ForestHlodConfig::default();
    c.bench_function("forest_hlod_build/10k_trees", |bench| {
        bench.iter(|| black_box(build_forest_hlod(black_box(&trees), &cfg).1));
    });
}

fn page_reconcile(c: &mut Criterion) {
    let mut t = PageTable::new(512 * 1024 * 1024);
    for k in 0..2_000 {
        t.register(
            AssetPages::from_lod_chain(
                AssetKey(k),
                6.0,
                &[500, 2_000, 10_000, 50_000, 200_000],
                32,
            )
            .unwrap(),
        );
    }
    let demands: Vec<PageDemand> = (0..2_000)
        .map(|k| PageDemand {
            key: AssetKey(k),
            wanted_pages: 1 + (k as usize % 5),
            nearest_distance: k as f32,
        })
        .collect();
    t.reconcile(&demands);
    c.bench_function("page_reconcile/2000_assets_steady", |bench| {
        bench.iter(|| black_box(t.reconcile(black_box(&demands)).load.len()));
    });
}

criterion_group!(
    benches,
    interest_build,
    scheduler_tick,
    forest_hlod,
    page_reconcile
);
criterion_main!(benches);
