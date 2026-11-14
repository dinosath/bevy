//! Deterministic, headless streaming simulation used by tests, benchmarks and tuning.
//!
//! The simulator drives the real [`StreamingScheduler`] and [`InterestBuilder`] with a synthetic
//! world and an [`IoModel`] (latency, shared bandwidth, decode throughput). It needs no GPU,
//! window or wall clock, so results are reproducible bit-for-bit and can be asserted in tests.
//!
//! Metrics (all frame-based, counted after a warm-up):
//! * **critical-miss frames** — frames in which some cell within `critical_radius` of the camera
//!   was *not* resident: a visible streaming failure (missing near-field world).
//! * **fallback cell-frames** — `Σ over frames` of cells inside `activation_radius` that were not
//!   resident (drawn with a coarser HLOD/impostor fallback instead).
//! * **request latency** — IO request → GPU-resident, per cell (p50/p95/p99/max).
//! * IO bytes, peak CPU/GPU residency, speculative hit/waste counters.

use crate::{
    coords::CellCoord,
    predict::{kmh_to_ms, InterestBuilder, MotionPredictor, Route},
    scheduler::{Action, CellBytes, CellState, StreamingConfig, StreamingScheduler},
};
use bevy_math::DVec3;
use bevy_platform::collections::HashMap;

/// Storage/CPU throughput model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IoModel {
    /// Fixed latency per request (seek/queueing), seconds.
    pub latency_s: f64,
    /// Total read bandwidth shared equally by active requests, bytes/second.
    pub bandwidth_bps: f64,
    /// Decode throughput **per decode job**, bytes/second.
    pub decode_bps: f64,
    /// Every `spike_every`-th request (by request counter) takes an extra `spike_s` (0 = off).
    pub spike_every: u32,
    /// Extra latency of spiked requests, seconds.
    pub spike_s: f64,
}

impl IoModel {
    /// Fast `NVMe` SSD.
    pub const NVME: Self = Self {
        latency_s: 0.000_3,
        bandwidth_bps: 2.0e9,
        decode_bps: 600.0e6,
        spike_every: 0,
        spike_s: 0.0,
    };
    /// SATA SSD.
    pub const SATA_SSD: Self = Self {
        latency_s: 0.001,
        bandwidth_bps: 450.0e6,
        decode_bps: 400.0e6,
        spike_every: 0,
        spike_s: 0.0,
    };
    /// Spinning disk (or a slow console/handheld storage).
    pub const HDD: Self = Self {
        latency_s: 0.012,
        bandwidth_bps: 120.0e6,
        decode_bps: 300.0e6,
        spike_every: 0,
        spike_s: 0.0,
    };
}

/// Simulation parameters.
#[derive(Clone, Debug)]
pub struct SimConfig {
    /// Streaming configuration under test.
    pub streaming: StreamingConfig,
    /// Storage model.
    pub io: IoModel,
    /// Frame time in seconds.
    pub dt: f64,
    /// Cells within this distance of the camera must be resident or the frame is a *critical miss*.
    pub critical_radius: f64,
    /// Average bytes of a cell (CPU, GPU); each cell varies deterministically around this.
    pub mean_cell_bytes: CellBytes,
    /// Seconds spent stationary at the start so the initial ring can load (not measured).
    pub warmup_s: f64,
    /// Seconds to ramp from rest to the target speed (not measured).
    pub ramp_s: f64,
    /// Seed of the synthetic world's cell-size variation.
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            streaming: StreamingConfig::default(),
            io: IoModel::SATA_SSD,
            dt: 1.0 / 60.0,
            critical_radius: 200.0,
            mean_cell_bytes: CellBytes {
                cpu: 3 * 1024 * 1024,
                gpu: 6 * 1024 * 1024,
            },
            warmup_s: 30.0,
            ramp_s: 6.0,
            seed: 0x5EED,
        }
    }
}

impl SimConfig {
    /// A demanding workload: dense 128 m cells (24 MiB CPU / 48 MiB GPU on average, like a
    /// forest/city block with high-poly geometry and textures) and a **tight** load radius, as used
    /// when VRAM is limited (consoles, 8 GB GPUs). Prefetching matters most here because the safety
    /// margin `load_radius − critical_radius` is small relative to the pipeline latency.
    pub fn heavy() -> Self {
        let mut c = Self {
            mean_cell_bytes: CellBytes {
                cpu: 24 * 1024 * 1024,
                gpu: 48 * 1024 * 1024,
            },
            ..Self::default()
        };
        c.streaming.mandatory_radius = 160.0;
        c.streaming.load_radius = 320.0;
        c.streaming.unload_hysteresis = 128.0;
        c.streaming.activation_radius = 256.0;
        c.streaming.ram_budget_bytes = 3 * 1024 * 1024 * 1024;
        c.streaming.vram_budget_bytes = 6 * 1024 * 1024 * 1024;
        c.streaming.upload_bytes_per_frame = 96 * 1024 * 1024;
        c.streaming.max_inflight_io = 6;
        c
    }
}

/// Deterministic per-cell size in `[0.4, 1.6] × mean` (dense forest/city cells are bigger).
pub fn synthetic_cell_bytes(seed: u64, c: CellCoord, mean: CellBytes) -> CellBytes {
    let h = splitmix64(seed ^ (u64::from(c.x as u32) << 32 | u64::from(c.z as u32)));
    let f = 0.4 + 1.2 * ((h >> 11) as f64 / (1u64 << 53) as f64);
    CellBytes {
        cpu: (mean.cpu as f64 * f) as u64,
        gpu: (mean.gpu as f64 * f) as u64,
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Quantile summary in seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quantiles {
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Maximum.
    pub max: f64,
}

fn quantiles(v: &mut [f64]) -> Quantiles {
    if v.is_empty() {
        return Quantiles::default();
    }
    v.sort_by(f64::total_cmp);
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Quantiles {
        p50: q(0.5),
        p95: q(0.95),
        p99: q(0.99),
        max: *v.last().expect("non-empty"),
    }
}

/// Results of one simulation run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SimResult {
    /// Measured frames.
    pub frames: u64,
    /// Frames with at least one non-resident cell inside `critical_radius`.
    pub critical_miss_frames: u64,
    /// Σ over frames of non-resident cells inside `activation_radius`.
    pub fallback_cell_frames: u64,
    /// IO request → GPU-resident latency.
    pub latency: Quantiles,
    /// Total bytes requested from storage.
    pub io_bytes: u64,
    /// Peak reserved CPU bytes.
    pub peak_cpu_bytes: u64,
    /// Peak reserved GPU bytes.
    pub peak_gpu_bytes: u64,
    /// Speculative cells admitted.
    pub speculative_loaded: u64,
    /// Speculative cells later needed.
    pub speculative_hits: u64,
    /// Speculative cells evicted/cancelled unused.
    pub speculative_wasted: u64,
    /// Cells evicted.
    pub evictions: u64,
    /// Mandatory admissions that exceeded a budget.
    pub budget_overruns: u64,
    /// Distance travelled during measurement, meters.
    pub distance_m: f64,
}

impl SimResult {
    /// Fraction of measured frames that were critical misses.
    pub fn critical_miss_ratio(&self) -> f64 {
        self.critical_miss_frames as f64 / self.frames.max(1) as f64
    }

    /// Fraction of speculative cells that were wasted.
    pub fn waste_ratio(&self) -> f64 {
        self.speculative_wasted as f64 / self.speculative_loaded.max(1) as f64
    }

    /// Average IO rate over the measured distance at `speed_mps`, MB/s (decimal).
    pub fn io_mb_per_s(&self, speed_mps: f64) -> f64 {
        if self.distance_m <= 0.0 {
            return 0.0;
        }
        self.io_bytes as f64 / (self.distance_m / speed_mps) / 1.0e6
    }
}

#[derive(Debug)]
struct IoJob {
    coord: CellCoord,
    generation: u32,
    latency_left: f64,
    bytes_left: f64,
    bytes: CellBytes,
}

#[derive(Debug)]
struct DecodeJob {
    coord: CellCoord,
    generation: u32,
    secs_left: f64,
}

/// The headless executor + metrics around a scheduler.
pub struct Simulator {
    cfg: SimConfig,
    scheduler: StreamingScheduler,
    builder: InterestBuilder,
    predictor: MotionPredictor,
    io: Vec<IoJob>,
    decode: Vec<DecodeJob>,
    uploads: Vec<(CellCoord, u32)>,
    started_at: HashMap<(CellCoord, u32), f64>,
    latencies: Vec<f64>,
    request_counter: u32,
    now: f64,
}

impl Simulator {
    /// Creates a simulator.
    pub fn new(cfg: SimConfig) -> Self {
        let mut scheduler = StreamingScheduler::new(cfg.streaming.clone());
        let (seed, mean) = (cfg.seed, cfg.mean_cell_bytes);
        scheduler.set_size_hint(move |c| synthetic_cell_bytes(seed, c, mean));
        Self {
            cfg,
            scheduler,
            builder: InterestBuilder::default(),
            predictor: MotionPredictor::default(),
            io: Vec::new(),
            decode: Vec::new(),
            uploads: Vec::new(),
            started_at: HashMap::default(),
            latencies: Vec::new(),
            request_counter: 0,
            now: 0.0,
        }
    }

    /// The scheduler (for assertions).
    pub fn scheduler(&self) -> &StreamingScheduler {
        &self.scheduler
    }

    /// Simulation time in seconds.
    pub fn now(&self) -> f64 {
        self.now
    }

    /// Teleports the predictor (forgets motion history).
    pub fn teleport(&mut self, to: DVec3) {
        self.predictor.reset(to);
    }

    /// Advances one frame with the focus at `pos`. Returns `(critical_miss, fallback_cells)` for
    /// this frame. Pass `route` to enable route-aware prefetch (also needs
    /// `streaming.route_enabled`) and `targets` for explicit mandatory destinations.
    pub fn step(&mut self, pos: DVec3, route: Option<&Route>, targets: &[DVec3]) -> (bool, u32) {
        let dt = self.cfg.dt;
        self.now += dt;
        let motion = self.predictor.push(pos, dt);
        let interest = self
            .builder
            .build(&self.cfg.streaming, &motion, route, targets);
        let mut actions = Vec::new();
        self.scheduler.tick(self.now, pos, interest, &mut actions);
        for a in actions {
            match a {
                Action::StartIo { coord, generation } => {
                    let bytes =
                        synthetic_cell_bytes(self.cfg.seed, coord, self.cfg.mean_cell_bytes);
                    self.request_counter += 1;
                    let spike = if self.cfg.io.spike_every > 0
                        && self.request_counter.is_multiple_of(self.cfg.io.spike_every)
                    {
                        self.cfg.io.spike_s
                    } else {
                        0.0
                    };
                    self.started_at.insert((coord, generation), self.now);
                    self.io.push(IoJob {
                        coord,
                        generation,
                        latency_left: self.cfg.io.latency_s + spike,
                        bytes_left: bytes.cpu as f64,
                        bytes,
                    });
                }
                Action::CancelIo { coord, generation } => {
                    self.io
                        .retain(|j| !(j.coord == coord && j.generation == generation));
                    self.started_at.remove(&(coord, generation));
                }
                Action::StartDecode { coord, generation } => {
                    let bytes =
                        synthetic_cell_bytes(self.cfg.seed, coord, self.cfg.mean_cell_bytes);
                    self.decode.push(DecodeJob {
                        coord,
                        generation,
                        secs_left: bytes.cpu as f64 / self.cfg.io.decode_bps,
                    });
                }
                Action::StartUpload {
                    coord, generation, ..
                } => self.uploads.push((coord, generation)),
                Action::Activate(_) | Action::Deactivate(_) | Action::Evict(_) => {}
            }
        }

        // Advance the executor by dt.
        let active_reading = self
            .io
            .iter()
            .filter(|j| j.latency_left <= 0.0)
            .count()
            .max(1);
        let share = self.cfg.io.bandwidth_bps / active_reading as f64;
        let mut finished_io = Vec::new();
        for j in &mut self.io {
            if j.latency_left > 0.0 {
                j.latency_left -= dt;
            } else {
                j.bytes_left -= share * dt;
            }
            if j.latency_left <= 0.0 && j.bytes_left <= 0.0 {
                finished_io.push((j.coord, j.generation, j.bytes));
            }
        }
        self.io
            .retain(|j| j.latency_left > 0.0 || j.bytes_left > 0.0);
        for (c, g, b) in finished_io {
            self.scheduler.io_complete(c, g, b);
        }
        let mut finished_dec = Vec::new();
        for j in &mut self.decode {
            j.secs_left -= dt;
            if j.secs_left <= 0.0 {
                finished_dec.push((j.coord, j.generation));
            }
        }
        self.decode.retain(|j| j.secs_left > 0.0);
        for (c, g) in finished_dec {
            self.scheduler.decode_complete(c, g);
        }
        for (c, g) in self.uploads.drain(..) {
            if self.scheduler.upload_complete(c, g)
                && let Some(t0) = self.started_at.remove(&(c, g))
            {
                self.latencies.push(self.now - t0);
            }
        }

        // Per-frame metrics.
        let grid = self.cfg.streaming.grid;
        let mut critical = false;
        grid.for_each_cell_in_radius(pos, self.cfg.critical_radius, |c, _| {
            critical |= !self.scheduler.is_resident(c);
        });
        let mut fallback = 0;
        grid.for_each_cell_in_radius(pos, self.cfg.streaming.activation_radius, |c, _| {
            if !self.scheduler.is_resident(c) {
                fallback += 1;
            }
        });
        (critical, fallback)
    }

    /// Snapshot of scheduler counters and the latency distribution.
    fn finish(&mut self, mut r: SimResult) -> SimResult {
        let s = self.scheduler.stats();
        r.io_bytes = s.io_bytes_requested;
        r.speculative_loaded = s.speculative_loaded;
        r.speculative_hits = s.speculative_hits;
        r.speculative_wasted = s.speculative_wasted;
        r.evictions = s.evictions;
        r.budget_overruns = s.budget_overruns;
        r.latency = quantiles(&mut self.latencies);
        r
    }

    /// Cells currently in `state`.
    pub fn count_in_state(&self, state: CellState) -> usize {
        self.scheduler.iter().filter(|(_, s)| *s == state).count()
    }
}

/// A scripted drive along a [`Route`] at constant speed after a ramp.
#[derive(Clone, Copy, Debug)]
pub struct Drive<'a> {
    /// The road.
    pub route: &'a Route,
    /// Target speed in km/h.
    pub speed_kmh: f64,
    /// Measured driving time in seconds (after warm-up and ramp).
    pub duration_s: f64,
    /// Provide the route to the streaming system (route-aware prefetch).
    pub route_known: bool,
}

/// Runs a drive and returns the metrics. Position along the route is parameterized by simulated
/// time only (deterministic).
pub fn simulate_drive(cfg: &SimConfig, d: &Drive<'_>) -> SimResult {
    let mut sim = Simulator::new(cfg.clone());
    let v = kmh_to_ms(d.speed_kmh);
    let dt = cfg.dt;
    let route_opt = d.route_known.then_some(d.route);

    // Warm-up: stationary at the start.
    let start = d.route.point_at(0.0);
    let warm_frames = (cfg.warmup_s / dt) as u64;
    for _ in 0..warm_frames {
        sim.step(start, route_opt, &[]);
    }
    // Ramp (distance = ½ v t²), not measured.
    let ramp_frames = (cfg.ramp_s / dt) as u64;
    let mut s = 0.0;
    let mut speed = 0.0;
    for _ in 0..ramp_frames {
        speed = (speed + v / cfg.ramp_s * dt).min(v);
        s += speed * dt;
        sim.step(d.route.point_at(s), route_opt, &[]);
    }
    // Measured cruise.
    let mut r = SimResult::default();
    let frames = (d.duration_s / dt) as u64;
    sim.latencies.clear();
    let (mut cpu_peak, mut gpu_peak) = (0, 0);
    let s0 = s;
    let io0 = sim.scheduler.stats().io_bytes_requested;
    let ev0 = sim.scheduler.stats().evictions;
    for _ in 0..frames {
        s += v * dt;
        let pos = d.route.point_at(s);
        let (crit, fb) = sim.step(pos, route_opt, &[]);
        r.frames += 1;
        r.critical_miss_frames += u64::from(crit);
        r.fallback_cell_frames += u64::from(fb);
        let st = sim.scheduler.stats();
        cpu_peak = cpu_peak.max(st.cpu_bytes);
        gpu_peak = gpu_peak.max(st.gpu_bytes);
    }
    r.distance_m = s - s0;
    r.peak_cpu_bytes = cpu_peak;
    r.peak_gpu_bytes = gpu_peak;
    let mut out = sim.finish(r);
    let st = sim.scheduler.stats();
    out.io_bytes -= io0;
    out.evictions -= ev0;
    let _ = st;
    out
}

/// Result of a teleport test.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TeleportResult {
    /// Seconds from the teleport until all cells within `critical_radius` were resident.
    pub seconds_to_critical_resident: f64,
    /// Frames with a critical miss after the teleport.
    pub critical_miss_frames: u64,
    /// Peak number of cells in flight (IO) after the teleport (bounded queue check).
    pub peak_inflight_io: u32,
}

/// Teleports from the route start to `destination`, optionally announcing it as an explicit
/// mandatory target one second *before* the jump (e.g. a fast-travel transition).
pub fn simulate_teleport(
    cfg: &SimConfig,
    destination: DVec3,
    announce_lead_s: Option<f64>,
) -> TeleportResult {
    let mut sim = Simulator::new(cfg.clone());
    let origin = DVec3::ZERO;
    for _ in 0..(cfg.warmup_s / cfg.dt) as u64 {
        sim.step(origin, None, &[]);
    }
    if let Some(lead) = announce_lead_s {
        for _ in 0..(lead / cfg.dt) as u64 {
            sim.step(origin, None, &[destination]);
        }
    }
    sim.teleport(destination);
    let mut res = TeleportResult::default();
    let mut frames = 0u64;
    let max_frames = (120.0 / cfg.dt) as u64;
    loop {
        let (crit, _) = sim.step(destination, None, &[]);
        frames += 1;
        res.peak_inflight_io = res.peak_inflight_io.max(sim.scheduler.stats().inflight_io);
        if crit {
            res.critical_miss_frames += 1;
        } else {
            break;
        }
        if frames > max_frames {
            break;
        }
    }
    res.seconds_to_critical_resident = frames as f64 * cfg.dt;
    res
}

/// Builds a winding road: a sine-wave serpentine of total `length` meters.
pub fn serpentine_route(length: f64, amplitude: f64, wavelength: f64, step: f64) -> Route {
    let n = (length / step).ceil() as usize + 1;
    let pts = (0..n)
        .map(|i| {
            let x = i as f64 * step;
            let z = amplitude * (core::f64::consts::TAU * x / wavelength).sin();
            DVec3::new(x, 0.0, z)
        })
        .collect();
    Route::new(pts).expect("n >= 2")
}

/// A straight road along +X.
pub fn straight_route(length: f64) -> Route {
    Route::new(vec![DVec3::ZERO, DVec3::new(length, 0.0, 0.0)]).expect("two points")
}

/// A road with 90° switchback corners every `leg` meters (alternating +X and +Z legs).
pub fn grid_route(legs: usize, leg: f64) -> Route {
    let mut pts = vec![DVec3::ZERO];
    let mut p = DVec3::ZERO;
    for i in 0..legs {
        if i % 2 == 0 {
            p.x += leg;
        } else {
            p.z += leg;
        }
        pts.push(p);
    }
    Route::new(pts).expect("legs >= 1")
}

/// Strategy presets for A/B comparisons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Static radius only; no heading bias, no prediction.
    RadiusOnly,
    /// Static radius inflated isotropically by `v · horizon` (the naive "just load more" fix).
    RadiusInflated,
    /// Kinematic prediction (`p + vt + ½at²`).
    Predictive,
    /// Kinematic prediction plus road-corridor prefetch.
    RouteAware,
}

impl Strategy {
    /// All strategies in comparison order.
    pub const ALL: [Self; 4] = [
        Self::RadiusOnly,
        Self::RadiusInflated,
        Self::Predictive,
        Self::RouteAware,
    ];

    /// Short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::RadiusOnly => "radius-only",
            Self::RadiusInflated => "radius-inflated",
            Self::Predictive => "predictive",
            Self::RouteAware => "route-aware",
        }
    }

    /// Applies the strategy to a config for a drive at `speed_kmh`. Returns whether the route is
    /// provided to the streaming system.
    pub fn apply(self, cfg: &mut SimConfig, speed_kmh: f64) -> bool {
        let s = &mut cfg.streaming;
        match self {
            Self::RadiusOnly => {
                s.prediction_enabled = false;
                s.route_enabled = false;
                s.behind_penalty = 0.0;
                false
            }
            Self::RadiusInflated => {
                s.prediction_enabled = false;
                s.route_enabled = false;
                s.behind_penalty = 0.0;
                s.load_radius += kmh_to_ms(speed_kmh) * s.prediction_horizon_s;
                false
            }
            Self::Predictive => {
                s.prediction_enabled = true;
                s.route_enabled = false;
                false
            }
            Self::RouteAware => {
                s.prediction_enabled = true;
                s.route_enabled = true;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SimConfig {
        SimConfig {
            warmup_s: 20.0,
            ..SimConfig::default()
        }
    }

    #[test]
    fn standing_still_converges_with_zero_critical_misses_after_warmup() {
        let r = straight_route(10_000.0);
        let res = simulate_drive(
            &cfg(),
            &Drive {
                route: &r,
                speed_kmh: 0.001,
                duration_s: 10.0,
                route_known: false,
            },
        );
        assert_eq!(res.critical_miss_frames, 0);
        assert_eq!(res.budget_overruns, 0);
    }

    #[test]
    fn simulation_is_bit_for_bit_deterministic() {
        let r = serpentine_route(8_000.0, 150.0, 900.0, 10.0);
        let run = || {
            let mut c = cfg();
            let known = Strategy::RouteAware.apply(&mut c, 150.0);
            simulate_drive(
                &c,
                &Drive {
                    route: &r,
                    speed_kmh: 150.0,
                    duration_s: 20.0,
                    route_known: known,
                },
            )
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn prediction_is_never_worse_than_radius_only_at_any_speed() {
        let road = serpentine_route(30_000.0, 120.0, 1_200.0, 10.0);
        for kmh in [30.0, 100.0, 200.0] {
            let mut results = Vec::new();
            for strat in [
                Strategy::RadiusOnly,
                Strategy::Predictive,
                Strategy::RouteAware,
            ] {
                let mut c = cfg();
                c.io = IoModel::HDD; // slow storage makes ordering matter
                let known = strat.apply(&mut c, kmh);
                results.push(simulate_drive(
                    &c,
                    &Drive {
                        route: &road,
                        speed_kmh: kmh,
                        duration_s: 40.0,
                        route_known: known,
                    },
                ));
            }
            assert!(
                results[1].fallback_cell_frames <= results[0].fallback_cell_frames,
                "{kmh} km/h: predictive {} > radius-only {}",
                results[1].fallback_cell_frames,
                results[0].fallback_cell_frames
            );
            assert!(
                results[1].critical_miss_frames <= results[0].critical_miss_frames,
                "{kmh} km/h critical"
            );
        }
    }

    #[test]
    fn memory_budgets_hold_during_a_long_fast_drive() {
        let road = straight_route(60_000.0);
        let mut c = cfg();
        c.streaming.ram_budget_bytes = 400 * 1024 * 1024;
        c.streaming.vram_budget_bytes = 800 * 1024 * 1024;
        let res = simulate_drive(
            &c,
            &Drive {
                route: &road,
                speed_kmh: 250.0,
                duration_s: 120.0,
                route_known: false,
            },
        );
        assert!(res.peak_cpu_bytes <= c.streaming.ram_budget_bytes || res.budget_overruns > 0);
        assert!(res.peak_gpu_bytes <= c.streaming.vram_budget_bytes || res.budget_overruns > 0);
        // The world streamed through memory many times over (a 60 km straight road).
        assert!(
            res.io_bytes > 4 * c.streaming.ram_budget_bytes,
            "{}",
            res.io_bytes
        );
    }

    #[test]
    fn teleport_with_lead_time_has_fewer_misses_and_bounded_queues() {
        let c = cfg();
        let dest = DVec3::new(40_000.0, 0.0, 40_000.0);
        let cold = simulate_teleport(&c, dest, None);
        let warm = simulate_teleport(&c, dest, Some(3.0));
        assert!(warm.critical_miss_frames <= cold.critical_miss_frames);
        assert!(
            warm.critical_miss_frames < cold.critical_miss_frames || cold.critical_miss_frames == 0
        );
        assert!(cold.peak_inflight_io as usize <= c.streaming.max_inflight_io);
    }
}
