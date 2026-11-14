//! Headless streaming benchmark: drives the real scheduler through scripted routes at
//! 30–250 km/h under different storage models and prefetch strategies and prints markdown tables.
//!
//! ```text
//! cargo run -p bevy_world_streaming --release --example streaming_sim [-- --quick]
//! ```
//!
//! Deterministic (no wall clock in the simulation); `tick µs` columns are measured wall time of
//! `InterestBuilder::build + StreamingScheduler::tick` and therefore machine dependent.
#![allow(clippy::print_stdout, reason = "this example prints a report")]

use bevy_math::DVec3;
use bevy_world_streaming::{
    predict::kmh_to_ms,
    sim::{
        serpentine_route, simulate_drive, simulate_teleport, straight_route, Drive, IoModel,
        SimConfig, Strategy, TeleportResult,
    },
};

const SPEEDS: [f64; 7] = [30.0, 60.0, 100.0, 150.0, 200.0, 250.0, 300.0];

fn main() {
    let quick = std::env::args().any(|a| a == "--quick");
    let duration = if quick { 20.0 } else { 60.0 };
    let road = serpentine_route(60_000.0, 140.0, 1_400.0, 10.0);
    let straight = straight_route(60_000.0);

    for (name, base, io) in [
        (
            "light cells (3/6 MiB), SATA SSD",
            SimConfig::default(),
            IoModel::SATA_SSD,
        ),
        (
            "heavy cells (24/48 MiB), tight 320 m radius, SATA SSD",
            SimConfig::heavy(),
            IoModel::SATA_SSD,
        ),
        (
            "heavy cells (24/48 MiB), tight 320 m radius, HDD / slow storage",
            SimConfig::heavy(),
            IoModel::HDD,
        ),
    ] {
        for (road_name, route) in [("winding road", &road), ("straight road", &straight)] {
            println!("\n### {name} — {road_name}, {duration:.0} s measured per row\n");
            println!(
                "| km/h | strategy | critical-miss frames | fallback cell-frames | req latency p50 / p99 / max (ms) | IO MB/s | peak GPU MiB | spec hit% | waste% | budget overruns |"
            );
            println!("|---:|---|---:|---:|---|---:|---:|---:|---:|---:|");
            for kmh in SPEEDS {
                for strat in Strategy::ALL {
                    let mut cfg = base.clone();
                    cfg.io = io;
                    let known = strat.apply(&mut cfg, kmh);
                    let r = simulate_drive(
                        &cfg,
                        &Drive {
                            route,
                            speed_kmh: kmh,
                            duration_s: duration,
                            route_known: known,
                        },
                    );
                    let hits =
                        r.speculative_hits as f64 / r.speculative_loaded.max(1) as f64 * 100.0;
                    println!(
                        "| {kmh:.0} | {} | {} ({:.1}%) | {} | {:.0} / {:.0} / {:.0} | {:.0} | {:.0} | {} | {:.0} | {} |",
                        strat.label(),
                        r.critical_miss_frames,
                        r.critical_miss_ratio() * 100.0,
                        r.fallback_cell_frames,
                        r.latency.p50 * 1e3,
                        r.latency.p99 * 1e3,
                        r.latency.max * 1e3,
                        r.io_mb_per_s(kmh_to_ms(kmh)),
                        r.peak_gpu_bytes as f64 / 1048576.0,
                        if r.speculative_loaded == 0 { "-".into() } else { format!("{hits:.0}") },
                        r.waste_ratio() * 100.0,
                        r.budget_overruns,
                    );
                }
            }
        }
    }

    println!("\n### Teleport (SATA SSD): seconds until the 200 m critical ring is resident\n");
    println!("| scenario | seconds | critical-miss frames | peak in-flight IO |");
    println!("|---|---:|---:|---:|");
    let cfg = SimConfig::default();
    let dest = DVec3::new(40_000.0, 0.0, 40_000.0);
    let row = |name: &str, r: TeleportResult| {
        println!(
            "| {name} | {:.2} | {} | {} |",
            r.seconds_to_critical_resident, r.critical_miss_frames, r.peak_inflight_io
        );
    };
    row("cold teleport", simulate_teleport(&cfg, dest, None));
    row(
        "destination announced 1 s ahead",
        simulate_teleport(&cfg, dest, Some(1.0)),
    );
    row(
        "destination announced 3 s ahead",
        simulate_teleport(&cfg, dest, Some(3.0)),
    );
}
