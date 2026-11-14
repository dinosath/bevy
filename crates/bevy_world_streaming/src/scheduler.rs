//! The cell streaming scheduler: a pure, deterministic state machine.
//!
//! The scheduler never performs IO, decoding or GPU work itself. Each [`StreamingScheduler::tick`]
//! it compares the *desired* set of cells (the interest set from [`crate::predict`]) against the
//! *current* set and emits [`Action`]s for an executor (the ECS plugin, or the headless
//! [`crate::sim`]) to carry out. The executor reports results back with
//! [`StreamingScheduler::io_complete`], [`StreamingScheduler::decode_complete`],
//! [`StreamingScheduler::upload_complete`] and [`StreamingScheduler::failed`].
//!
//! ```text
//! (absent) → Loading(IO) → Loaded → Decoding → Prepared → Uploading → Resident ⇄ Active
//!               │ cancel/evict at any point (generation counter discards stale results)
//!               └──────→ Failed(backoff) → (absent)
//! ```
//!
//! Every queue is bounded by [`StreamingConfig`]; every per-frame drain has a budget. See
//! `.claude/skills/level-streaming`.

use crate::{
    coords::{CellCoord, CellGrid},
    predict::{InterestCell, InterestTier},
};
use bevy_math::DVec3;
use bevy_platform::collections::HashMap;

/// Memory a cell occupies once resident.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellBytes {
    /// CPU (RAM) bytes.
    pub cpu: u64,
    /// GPU (VRAM) bytes.
    pub gpu: u64,
}

/// All tunables of the streaming system. No magic numbers live in systems.
#[derive(Clone, Debug)]
pub struct StreamingConfig {
    /// Cell grid (cell size).
    pub grid: CellGrid,
    /// Radius that must always be resident, in meters (hard guarantee, pinned).
    pub mandatory_radius: f64,
    /// Static desired-resident radius in meters.
    pub load_radius: f64,
    /// Extra distance beyond `load_radius` before a cell is evicted (hysteresis), in meters.
    pub unload_hysteresis: f64,
    /// Resident cells within this radius become `Active` (spawned/rendered/simulated).
    pub activation_radius: f64,
    /// Active cells beyond `activation_radius + activation_hysteresis` are deactivated.
    pub activation_hysteresis: f64,
    /// Ordering penalty for cells behind the direction of travel (`0` disables).
    pub behind_penalty: f64,

    /// Enables kinematic motion prediction.
    pub prediction_enabled: bool,
    /// Minimum horizontal speed (m/s) before any speculative interest is generated.
    pub min_prediction_speed: f64,
    /// How far ahead to predict, in seconds.
    pub prediction_horizon_s: f64,
    /// Spacing of predicted samples, in seconds.
    pub prediction_step_s: f64,
    /// Radius around each predicted sample to include, in meters.
    pub prediction_radius: f64,
    /// Clamp on estimated acceleration magnitude (m/s²).
    pub max_prediction_accel: f64,

    /// Enables route-aware (road corridor) prefetch when a route is available.
    pub route_enabled: bool,
    /// Route lookahead in seconds of travel (the larger of this and `prediction_horizon_s` is used).
    pub route_horizon_s: f64,
    /// If the focus is farther than this from the route it is considered off-route (meters).
    pub route_max_lateral: f64,
    /// Corridor half width around the route, in meters.
    pub corridor_half_width: f64,

    /// Maximum concurrent IO requests.
    pub max_inflight_io: usize,
    /// Fraction (0..=1) of `max_inflight_io` that speculative requests may occupy.
    pub speculative_io_fraction: f32,
    /// Maximum concurrent decode jobs.
    pub max_inflight_decode: usize,
    /// Bytes of GPU upload that may *start* per tick (a single larger cell may still start alone).
    pub upload_bytes_per_frame: u64,
    /// Maximum cells activated per tick (limits spawn spikes).
    pub max_activations_per_frame: usize,
    /// CPU memory budget for all cells (resident + in flight), in bytes.
    pub ram_budget_bytes: u64,
    /// GPU memory budget for all cells (resident + in flight), in bytes.
    pub vram_budget_bytes: u64,
    /// Size assumed for a cell before its real size is known.
    pub default_cell_bytes: CellBytes,
    /// Seconds to wait before retrying a failed cell.
    pub retry_backoff_s: f64,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            grid: CellGrid::new(128.0),
            mandatory_radius: 160.0,
            load_radius: 512.0,
            unload_hysteresis: 128.0,
            activation_radius: 384.0,
            activation_hysteresis: 64.0,
            behind_penalty: 1.0,
            prediction_enabled: true,
            min_prediction_speed: 2.0,
            prediction_horizon_s: 6.0,
            prediction_step_s: 0.5,
            prediction_radius: 192.0,
            max_prediction_accel: 12.0,
            route_enabled: true,
            route_horizon_s: 5.0,
            route_max_lateral: 60.0,
            corridor_half_width: 160.0,
            max_inflight_io: 8,
            speculative_io_fraction: 0.5,
            max_inflight_decode: 4,
            upload_bytes_per_frame: 32 * 1024 * 1024,
            max_activations_per_frame: 4,
            ram_budget_bytes: 2 * 1024 * 1024 * 1024,
            vram_budget_bytes: 2 * 1024 * 1024 * 1024,
            default_cell_bytes: CellBytes {
                cpu: 2 * 1024 * 1024,
                gpu: 4 * 1024 * 1024,
            },
            retry_backoff_s: 2.0,
        }
    }
}

/// Lifecycle state of a cell known to the scheduler. A cell that is not tracked is *unloaded*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CellState {
    /// IO in flight.
    Loading,
    /// IO finished, waiting for a decode slot.
    Loaded,
    /// Decode in flight.
    Decoding,
    /// Decoded, waiting for upload budget.
    Prepared,
    /// GPU upload in flight.
    Uploading,
    /// Data is resident (CPU + GPU) but not spawned/rendered.
    Resident,
    /// Resident and active (spawned/rendered/simulated).
    Active,
    /// Failed; will be retried after backoff.
    Failed,
}

impl CellState {
    /// Number of states (for fixed-size histograms).
    pub const COUNT: usize = 8;

    /// `true` if the data can be rendered/used now.
    #[inline]
    pub fn is_resident(self) -> bool {
        matches!(self, Self::Resident | Self::Active)
    }
}

/// Work the executor must perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Begin reading the cell from storage.
    StartIo {
        /// The cell.
        coord: CellCoord,
        /// Generation to echo back on completion.
        generation: u32,
    },
    /// Abandon an in-flight read (completion will be ignored anyway).
    CancelIo {
        /// The cell.
        coord: CellCoord,
        /// Generation of the cancelled request.
        generation: u32,
    },
    /// Begin decompressing/preparing the loaded cell.
    StartDecode {
        /// The cell.
        coord: CellCoord,
        /// Generation to echo back.
        generation: u32,
    },
    /// Begin uploading the prepared cell to the GPU.
    StartUpload {
        /// The cell.
        coord: CellCoord,
        /// Generation to echo back.
        generation: u32,
        /// GPU bytes to be uploaded.
        bytes: u64,
    },
    /// Spawn/enable the resident cell.
    Activate(CellCoord),
    /// Despawn/disable the cell but keep its data resident.
    Deactivate(CellCoord),
    /// Free the cell's CPU and GPU data.
    Evict(CellCoord),
}

/// Counters and gauges, reset never (monotonic counters) or recomputed each tick (gauges).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SchedulerStats {
    /// Gauge: number of cells per [`CellState`] (index = `state as usize`).
    pub cells_by_state: [u32; CellState::COUNT],
    /// Gauge: reserved CPU bytes (resident + in flight).
    pub cpu_bytes: u64,
    /// Gauge: reserved GPU bytes (resident + in flight).
    pub gpu_bytes: u64,
    /// Gauge: IO requests in flight.
    pub inflight_io: u32,
    /// Gauge: decode jobs in flight.
    pub inflight_decode: u32,
    /// Counter: cells evicted.
    pub evictions: u64,
    /// Counter: in-flight requests cancelled.
    pub cancels: u64,
    /// Counter: failed loads.
    pub failures: u64,
    /// Counter: mandatory admissions that had to exceed a budget.
    pub budget_overruns: u64,
    /// Counter: speculative cells admitted.
    pub speculative_loaded: u64,
    /// Counter: speculative cells that later became wanted/mandatory/active (useful prefetch).
    pub speculative_hits: u64,
    /// Counter: speculative cells evicted/cancelled without ever being needed (waste).
    pub speculative_wasted: u64,
    /// Counter: bytes requested from IO (hint at admission).
    pub io_bytes_requested: u64,
    /// Counter: GPU bytes whose upload was started.
    pub upload_bytes_started: u64,
}

#[derive(Clone, Copy, Debug)]
struct Prio {
    tier: u8, // 0 mandatory, 1 wanted, 2 speculative, 3 retained-only (hysteresis)
    time_to_need: f32,
    distance: f32,
}

impl Prio {
    const RETAINED: Self = Self {
        tier: 3,
        time_to_need: f32::INFINITY,
        distance: f32::INFINITY,
    };

    fn from_interest(i: &InterestCell) -> Self {
        Self {
            tier: i.tier as u8,
            time_to_need: if i.tier == InterestTier::Speculative {
                i.time_to_need
            } else {
                0.0
            },
            distance: i.sort_distance,
        }
    }

    /// Total order, most urgent first.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.tier
            .cmp(&other.tier)
            .then_with(|| self.time_to_need.total_cmp(&other.time_to_need))
            .then_with(|| self.distance.total_cmp(&other.distance))
    }
}

#[derive(Clone, Debug)]
struct Entry {
    state: CellState,
    generation: u32,
    bytes: CellBytes,
    prio: Prio,
    last_needed: f64,
    loaded_speculative: bool,
    used: bool,
    retry_at: f64,
}

type SizeHintFn = Box<dyn Fn(CellCoord) -> CellBytes + Send + Sync>;

/// The streaming state machine. See the [module docs](self).
pub struct StreamingScheduler {
    cfg: StreamingConfig,
    cells: HashMap<CellCoord, Entry>,
    next_generation: u32,
    size_hint: Option<SizeHintFn>,
    stats: SchedulerStats,
    // Scratch buffers (retained to keep `tick` allocation-free in steady state).
    needed: HashMap<CellCoord, Prio>,
    keys: Vec<CellCoord>,
    order: Vec<(Prio, CellCoord)>,
}

impl StreamingScheduler {
    /// Creates a scheduler.
    pub fn new(cfg: StreamingConfig) -> Self {
        Self {
            cfg,
            cells: HashMap::default(),
            next_generation: 1,
            size_hint: None,
            stats: SchedulerStats::default(),
            needed: HashMap::default(),
            keys: Vec::new(),
            order: Vec::new(),
        }
    }

    /// Sets a function returning the expected size of a cell before it is loaded (e.g. from the
    /// world manifest). Defaults to [`StreamingConfig::default_cell_bytes`].
    pub fn set_size_hint(&mut self, f: impl Fn(CellCoord) -> CellBytes + Send + Sync + 'static) {
        self.size_hint = Some(Box::new(f));
    }

    /// The active configuration.
    pub fn config(&self) -> &StreamingConfig {
        &self.cfg
    }

    /// Mutable access to the configuration (takes effect on the next tick).
    pub fn config_mut(&mut self) -> &mut StreamingConfig {
        &mut self.cfg
    }

    /// Current statistics.
    pub fn stats(&self) -> &SchedulerStats {
        &self.stats
    }

    /// State of a cell, or `None` if it is unloaded.
    pub fn state(&self, c: CellCoord) -> Option<CellState> {
        self.cells.get(&c).map(|e| e.state)
    }

    /// `true` if the cell's data is resident (`Resident` or `Active`).
    pub fn is_resident(&self, c: CellCoord) -> bool {
        self.state(c).is_some_and(CellState::is_resident)
    }

    /// Iterates over all tracked cells and their states (unspecified order).
    pub fn iter(&self) -> impl Iterator<Item = (CellCoord, CellState)> + '_ {
        self.cells.iter().map(|(c, e)| (*c, e.state))
    }

    /// Generation of the current request for `c` (for stale-result filtering in executors).
    pub fn generation(&self, c: CellCoord) -> Option<u32> {
        self.cells.get(&c).map(|e| e.generation)
    }

    fn hint(&self, c: CellCoord) -> CellBytes {
        self.size_hint
            .as_ref()
            .map_or(self.cfg.default_cell_bytes, |f| f(c))
    }

    fn entry_matching(
        &mut self,
        c: CellCoord,
        generation: u32,
        state: CellState,
    ) -> Option<&mut Entry> {
        self.cells
            .get_mut(&c)
            .filter(|e| e.generation == generation && e.state == state)
    }

    /// IO finished. `actual` is the real size. Returns `false` if the result was stale.
    pub fn io_complete(&mut self, c: CellCoord, generation: u32, actual: CellBytes) -> bool {
        match self.entry_matching(c, generation, CellState::Loading) {
            Some(e) => {
                e.state = CellState::Loaded;
                e.bytes = actual;
                true
            }
            None => false,
        }
    }

    /// Decode finished. Returns `false` if stale.
    pub fn decode_complete(&mut self, c: CellCoord, generation: u32) -> bool {
        match self.entry_matching(c, generation, CellState::Decoding) {
            Some(e) => {
                e.state = CellState::Prepared;
                true
            }
            None => false,
        }
    }

    /// Upload finished (cell is now GPU resident). Returns `false` if stale.
    pub fn upload_complete(&mut self, c: CellCoord, generation: u32) -> bool {
        match self.entry_matching(c, generation, CellState::Uploading) {
            Some(e) => {
                e.state = CellState::Resident;
                true
            }
            None => false,
        }
    }

    /// A pipeline stage failed; the cell is retried after the backoff. Returns `false` if stale.
    pub fn failed(&mut self, c: CellCoord, generation: u32, now: f64) -> bool {
        let backoff = self.cfg.retry_backoff_s;
        match self.cells.get_mut(&c) {
            Some(e) if e.generation == generation && e.state < CellState::Resident => {
                e.state = CellState::Failed;
                e.retry_at = now + backoff;
                e.bytes = CellBytes::default();
                self.stats.failures += 1;
                true
            }
            _ => false,
        }
    }

    fn totals(&self) -> (u64, u64) {
        self.cells
            .values()
            .fold((0, 0), |(c, g), e| (c + e.bytes.cpu, g + e.bytes.gpu))
    }

    fn remove_entry(&mut self, c: CellCoord, out: &mut Vec<Action>) {
        let Some(e) = self.cells.remove(&c) else {
            return;
        };
        if e.loaded_speculative && !e.used {
            self.stats.speculative_wasted += 1;
        }
        match e.state {
            CellState::Loading => {
                self.stats.cancels += 1;
                out.push(Action::CancelIo {
                    coord: c,
                    generation: e.generation,
                });
            }
            CellState::Decoding | CellState::Uploading => {
                // Executor discards the stale completion via the generation check.
                self.stats.cancels += 1;
            }
            CellState::Active => {
                out.push(Action::Deactivate(c));
                out.push(Action::Evict(c));
                self.stats.evictions += 1;
            }
            CellState::Resident => {
                out.push(Action::Evict(c));
                self.stats.evictions += 1;
            }
            CellState::Loaded | CellState::Prepared | CellState::Failed => {}
        }
    }

    /// Runs one scheduling step.
    ///
    /// * `now` — monotonically increasing time in seconds (pass the simulation/frame clock; never
    ///   read the wall clock inside the scheduler).
    /// * `focus` — absolute position used for hysteresis, activation and tie-breaking.
    /// * `interest` — output of [`crate::predict::InterestBuilder::build`].
    /// * `out` — receives the actions to execute (cleared first).
    pub fn tick(
        &mut self,
        now: f64,
        focus: DVec3,
        interest: &[InterestCell],
        out: &mut Vec<Action>,
    ) {
        out.clear();
        let grid = self.cfg.grid;

        // 0. Record this tick's interest.
        self.needed.clear();
        for i in interest {
            self.needed.insert(i.coord, Prio::from_interest(i));
        }
        for (c, p) in &self.needed {
            if let Some(e) = self.cells.get_mut(c) {
                e.last_needed = now;
                e.prio = *p;
                if e.loaded_speculative && !e.used && p.tier < 2 {
                    e.used = true;
                    self.stats.speculative_hits += 1;
                }
            }
        }

        // 1. Evict / cancel everything no longer retained (outside interest AND beyond the
        //    hysteresis radius). Sorted for deterministic action order.
        let keep_radius = self.cfg.load_radius + self.cfg.unload_hysteresis;
        self.keys.clear();
        for (c, e) in &mut self.cells {
            if !self.needed.contains_key(c) {
                e.prio = Prio::RETAINED;
                if grid.distance_to_cell(focus, *c) > keep_radius {
                    self.keys.push(*c);
                }
            }
        }
        self.keys.sort();
        for i in 0..self.keys.len() {
            let c = self.keys[i];
            self.remove_entry(c, out);
        }

        // Failed cells whose backoff elapsed become absent again (so they can be re-admitted).
        self.keys.clear();
        self.keys.extend(
            self.cells
                .iter()
                .filter(|(_, e)| e.state == CellState::Failed && e.retry_at <= now)
                .map(|(c, _)| *c),
        );
        self.keys.sort();
        for &c in &self.keys {
            self.cells.remove(&c);
        }

        // 2. Activation / deactivation.
        let act_r = self.cfg.activation_radius;
        let deact_r = act_r + self.cfg.activation_hysteresis;
        self.order.clear();
        for (c, e) in &self.cells {
            let d = grid.distance_to_cell(focus, *c);
            match e.state {
                CellState::Resident if d <= act_r || e.prio.tier == 0 => {
                    self.order.push((
                        Prio {
                            distance: d as f32,
                            ..e.prio
                        },
                        *c,
                    ));
                }
                _ => {}
            }
        }
        self.order
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for &(_, c) in self.order.iter().take(self.cfg.max_activations_per_frame) {
            if let Some(e) = self.cells.get_mut(&c) {
                e.state = CellState::Active;
                e.used = true;
                out.push(Action::Activate(c));
            }
        }
        self.keys.clear();
        for (c, e) in &self.cells {
            if e.state == CellState::Active
                && grid.distance_to_cell(focus, *c) > deact_r
                && e.prio.tier != 0
            {
                self.keys.push(*c);
            }
        }
        self.keys.sort();
        for &c in &self.keys {
            if let Some(e) = self.cells.get_mut(&c) {
                e.state = CellState::Resident;
                out.push(Action::Deactivate(c));
            }
        }

        // 3a. Decode slots.
        let decoding = self
            .cells
            .values()
            .filter(|e| e.state == CellState::Decoding)
            .count();
        self.order.clear();
        self.order.extend(
            self.cells
                .iter()
                .filter(|(_, e)| e.state == CellState::Loaded)
                .map(|(c, e)| (e.prio, *c)),
        );
        self.order
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let free_decode_slots = self.cfg.max_inflight_decode.saturating_sub(decoding);
        for i in 0..self.order.len().min(free_decode_slots) {
            let c = self.order[i].1;
            let e = self.cells.get_mut(&c).expect("tracked");
            e.state = CellState::Decoding;
            out.push(Action::StartDecode {
                coord: c,
                generation: e.generation,
            });
        }

        // 3b. Upload budget.
        self.order.clear();
        self.order.extend(
            self.cells
                .iter()
                .filter(|(_, e)| e.state == CellState::Prepared)
                .map(|(c, e)| (e.prio, *c)),
        );
        self.order
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut started = 0u64;
        for i in 0..self.order.len() {
            let c = self.order[i].1;
            let e = self.cells.get_mut(&c).expect("tracked");
            if started > 0 && started + e.bytes.gpu > self.cfg.upload_bytes_per_frame {
                break;
            }
            e.state = CellState::Uploading;
            started += e.bytes.gpu;
            self.stats.upload_bytes_started += e.bytes.gpu;
            out.push(Action::StartUpload {
                coord: c,
                generation: e.generation,
                bytes: e.bytes.gpu,
            });
        }

        // 4. Admission of new IO, in interest order.
        let mut inflight_io = self
            .cells
            .values()
            .filter(|e| e.state == CellState::Loading)
            .count();
        let mut spec_inflight = self
            .cells
            .values()
            .filter(|e| e.state == CellState::Loading && e.loaded_speculative)
            .count();
        let spec_cap =
            ((self.cfg.max_inflight_io as f32) * self.cfg.speculative_io_fraction).ceil() as usize;
        let (mut cpu_total, mut gpu_total) = self.totals();
        for i in interest {
            if inflight_io >= self.cfg.max_inflight_io {
                break;
            }
            if self.cells.contains_key(&i.coord) {
                continue;
            }
            let speculative = i.tier == InterestTier::Speculative;
            if speculative && spec_inflight >= spec_cap {
                continue;
            }
            let bytes = self.hint(i.coord);
            let prio = Prio::from_interest(i);
            let fits = |cpu: u64, gpu: u64, cfg: &StreamingConfig| {
                cpu + bytes.cpu <= cfg.ram_budget_bytes && gpu + bytes.gpu <= cfg.vram_budget_bytes
            };
            if !fits(cpu_total, gpu_total, &self.cfg) {
                self.make_room(prio, bytes, &mut cpu_total, &mut gpu_total, out);
                if !fits(cpu_total, gpu_total, &self.cfg) {
                    if i.tier == InterestTier::Mandatory {
                        self.stats.budget_overruns += 1; // correctness over budget
                    } else {
                        continue;
                    }
                }
            }
            let generation = self.next_generation;
            self.next_generation = self.next_generation.wrapping_add(1).max(1);
            self.cells.insert(
                i.coord,
                Entry {
                    state: CellState::Loading,
                    generation,
                    bytes,
                    prio,
                    last_needed: now,
                    loaded_speculative: speculative,
                    used: !speculative,
                    retry_at: 0.0,
                },
            );
            cpu_total += bytes.cpu;
            gpu_total += bytes.gpu;
            inflight_io += 1;
            if speculative {
                spec_inflight += 1;
                self.stats.speculative_loaded += 1;
            }
            self.stats.io_bytes_requested += bytes.cpu;
            out.push(Action::StartIo {
                coord: i.coord,
                generation,
            });
        }

        // 5. Gauges.
        self.stats.cells_by_state = [0; CellState::COUNT];
        let (mut dec, mut io) = (0, 0);
        for e in self.cells.values() {
            self.stats.cells_by_state[e.state as usize] += 1;
            match e.state {
                CellState::Loading => io += 1,
                CellState::Decoding => dec += 1,
                _ => {}
            }
        }
        let (cpu, gpu) = self.totals();
        self.stats.cpu_bytes = cpu;
        self.stats.gpu_bytes = gpu;
        self.stats.inflight_io = io;
        self.stats.inflight_decode = dec;
    }

    /// Evicts strictly-less-urgent cells to fit `need`. Victim order: unneeded retained cells by
    /// least-recently-needed, then lower tiers, farthest time-to-need first. Mandatory cells are
    /// never victims.
    fn make_room(
        &mut self,
        candidate: Prio,
        need: CellBytes,
        cpu_total: &mut u64,
        gpu_total: &mut u64,
        out: &mut Vec<Action>,
    ) {
        self.order.clear();
        for (c, e) in &self.cells {
            let evictable_state = matches!(
                e.state,
                CellState::Resident | CellState::Active | CellState::Loaded | CellState::Prepared
            ) || (e.state == CellState::Loading && e.loaded_speculative);
            if !evictable_state || e.prio.tier == 0 {
                continue;
            }
            if e.prio.tier < 3 && e.prio.cmp(&candidate).is_le() {
                continue; // not strictly less urgent than the candidate
            }
            self.order.push((e.prio, *c));
        }
        // Least urgent first; ties by oldest `last_needed`, then coord.
        let cells = &self.cells;
        self.order.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| cells[&a.1].last_needed.total_cmp(&cells[&b.1].last_needed))
                .then_with(|| a.1.cmp(&b.1))
        });
        for i in 0..self.order.len() {
            if *cpu_total + need.cpu <= self.cfg.ram_budget_bytes
                && *gpu_total + need.gpu <= self.cfg.vram_budget_bytes
            {
                break;
            }
            let c = self.order[i].1;
            let b = self.cells[&c].bytes;
            self.remove_entry(c, out);
            *cpu_total -= b.cpu;
            *gpu_total -= b.gpu;
        }
    }
}

#[cfg(test)]
#[expect(clippy::field_reassign_with_default, reason = "readable test setup")]
mod tests {
    use super::*;
    use crate::predict::{InterestBuilder, MotionState};

    fn interest_at(cfg: &StreamingConfig, p: DVec3) -> Vec<InterestCell> {
        let mut b = InterestBuilder::default();
        b.build(
            cfg,
            &MotionState {
                position: p,
                ..Default::default()
            },
            None,
            &[],
        )
        .to_vec()
    }

    /// Runs the scheduler against an instant executor (everything completes immediately).
    fn pump(
        s: &mut StreamingScheduler,
        now: f64,
        p: DVec3,
        interest: &[InterestCell],
    ) -> Vec<Action> {
        let mut all = Vec::new();
        let mut out = Vec::new();
        for _ in 0..8 {
            s.tick(now, p, interest, &mut out);
            all.extend(out.iter().copied());
            for a in out.clone() {
                match a {
                    Action::StartIo { coord, generation } => {
                        let b = s.config().default_cell_bytes;
                        s.io_complete(coord, generation, b);
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
        }
        all
    }

    #[test]
    fn cells_reach_resident_then_active_and_respect_inflight_cap() {
        let mut cfg = StreamingConfig::default();
        cfg.max_inflight_io = 3;
        let mut s = StreamingScheduler::new(cfg.clone());
        let p = DVec3::new(64.0, 0.0, 64.0);
        let interest = interest_at(&cfg, p);
        let mut out = Vec::new();
        s.tick(0.0, p, &interest, &mut out);
        let io = out
            .iter()
            .filter(|a| matches!(a, Action::StartIo { .. }))
            .count();
        assert_eq!(io, 3, "bounded IO queue");
        for a in out.clone() {
            if let Action::StartIo { coord, generation } = a {
                s.io_complete(coord, generation, s.config().default_cell_bytes);
            }
        }
        pump(&mut s, 0.1, p, &interest);
        for _ in 0..200 {
            pump(&mut s, 0.2, p, &interest);
        }
        let home = cfg.grid.cell_of(p);
        assert_eq!(s.state(home), Some(CellState::Active));
        assert!(s.stats().cpu_bytes <= cfg.ram_budget_bytes);
    }

    #[test]
    fn stale_completion_is_ignored_after_cancel() {
        let cfg = StreamingConfig::default();
        let mut s = StreamingScheduler::new(cfg.clone());
        let p = DVec3::new(64.0, 0.0, 64.0);
        let interest = interest_at(&cfg, p);
        let mut out = Vec::new();
        s.tick(0.0, p, &interest, &mut out);
        let (coord, generation) = out
            .iter()
            .find_map(|a| match a {
                Action::StartIo { coord, generation } => Some((*coord, *generation)),
                _ => None,
            })
            .unwrap();
        // Move far away: the request is cancelled.
        let far = DVec3::new(1.0e6, 0.0, 1.0e6);
        let far_interest = interest_at(&cfg, far);
        s.tick(1.0, far, &far_interest, &mut out);
        assert!(out
            .iter()
            .any(|a| matches!(a, Action::CancelIo { coord: c, .. } if *c == coord)));
        assert!(
            !s.io_complete(coord, generation, CellBytes::default()),
            "stale"
        );
        assert_eq!(s.state(coord), None);
    }

    #[test]
    fn hysteresis_keeps_cells_just_outside_load_radius() {
        let mut cfg = StreamingConfig::default();
        cfg.prediction_enabled = false;
        let mut s = StreamingScheduler::new(cfg.clone());
        let p = DVec3::new(0.0, 0.0, 0.0);
        let interest = interest_at(&cfg, p);
        for _ in 0..50 {
            pump(&mut s, 0.0, p, &interest);
        }
        // Step so that a loaded cell falls outside load_radius but inside +hysteresis.
        let edge_cell = interest
            .iter()
            .filter(|i| i.tier == InterestTier::Wanted)
            .max_by(|a, b| a.sort_distance.total_cmp(&b.sort_distance))
            .unwrap()
            .coord;
        let p2 = DVec3::new(-(cfg.unload_hysteresis * 0.5), 0.0, 0.0);
        let i2 = interest_at(&cfg, p2);
        let d = cfg.grid.distance_to_cell(p2, edge_cell);
        if d > cfg.load_radius && d <= cfg.load_radius + cfg.unload_hysteresis {
            let mut out = Vec::new();
            s.tick(1.0, p2, &i2, &mut out);
            assert!(s.state(edge_cell).is_some(), "kept by hysteresis");
        }
        // Far enough: evicted.
        let p3 = DVec3::new(-(cfg.unload_hysteresis + cfg.load_radius) * 3.0, 0.0, 0.0);
        let i3 = interest_at(&cfg, p3);
        let mut out = Vec::new();
        s.tick(2.0, p3, &i3, &mut out);
        assert_eq!(s.state(edge_cell), None);
    }

    #[test]
    fn budgets_are_never_exceeded_except_for_mandatory_and_mandatory_is_never_evicted() {
        let mut cfg = StreamingConfig::default();
        cfg.ram_budget_bytes = 10 * cfg.default_cell_bytes.cpu;
        cfg.prediction_enabled = false;
        let mut s = StreamingScheduler::new(cfg.clone());
        let p = DVec3::new(64.0, 0.0, 64.0);
        let interest = interest_at(&cfg, p);
        for t in 0..400 {
            pump(&mut s, f64::from(t) * 0.01, p, &interest);
            assert!(
                s.stats().cpu_bytes <= cfg.ram_budget_bytes || s.stats().budget_overruns > 0,
                "budget exceeded without a recorded mandatory overrun"
            );
        }
        for i in interest
            .iter()
            .filter(|i| i.tier == InterestTier::Mandatory)
        {
            assert!(
                s.is_resident(i.coord),
                "mandatory cell {:?} not resident",
                i.coord
            );
        }
    }

    #[test]
    fn higher_tier_candidate_evicts_speculative_under_pressure() {
        let mut cfg = StreamingConfig::default();
        cfg.ram_budget_bytes = 3 * cfg.default_cell_bytes.cpu;
        cfg.prediction_enabled = false;
        cfg.mandatory_radius = 0.0;
        cfg.load_radius = 0.0;
        let mut s = StreamingScheduler::new(cfg.clone());
        let spec = |x: i32| InterestCell {
            coord: CellCoord::new(x, 0),
            tier: InterestTier::Speculative,
            time_to_need: x as f32,
            sort_distance: 0.0,
        };
        let spec_set = [spec(1), spec(2), spec(3)];
        let focus = DVec3::new(0.0, 0.0, 0.0);
        for t in 0..20 {
            pump(&mut s, f64::from(t) * 0.1, focus, &spec_set);
        }
        assert_eq!(s.stats().cells_by_state.iter().sum::<u32>(), 3);
        let urgent = InterestCell {
            coord: CellCoord::new(9, 9),
            tier: InterestTier::Wanted,
            time_to_need: 0.0,
            sort_distance: 1.0,
        };
        let mut set = vec![urgent];
        set.extend_from_slice(&spec_set);
        for t in 0..20 {
            pump(&mut s, 5.0 + f64::from(t) * 0.1, focus, &set);
        }
        assert!(
            s.state(urgent.coord).is_some(),
            "urgent cell admitted by evicting speculative"
        );
        assert!(s.stats().cpu_bytes <= cfg.ram_budget_bytes);
    }

    #[test]
    fn scheduler_is_deterministic() {
        let run = || {
            let cfg = StreamingConfig::default();
            let mut s = StreamingScheduler::new(cfg.clone());
            let mut trace = Vec::new();
            for step in 0..100 {
                let p = DVec3::new(f64::from(step) * 9.0, 0.0, f64::from(step) * 2.0);
                let interest = interest_at(&cfg, p);
                trace.extend(pump(&mut s, f64::from(step) * 0.1, p, &interest));
            }
            trace
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn failed_cells_back_off_then_retry() {
        let mut cfg = StreamingConfig::default();
        cfg.max_inflight_io = 10_000;
        let mut s = StreamingScheduler::new(cfg.clone());
        let p = DVec3::new(64.0, 0.0, 64.0);
        let interest = interest_at(&cfg, p);
        let mut out = Vec::new();
        s.tick(0.0, p, &interest, &mut out);
        let (c, g) = out
            .iter()
            .find_map(|a| match a {
                Action::StartIo { coord, generation } => Some((*coord, *generation)),
                _ => None,
            })
            .unwrap();
        assert!(s.failed(c, g, 0.0));
        s.tick(0.5, p, &interest, &mut out);
        assert_eq!(s.state(c), Some(CellState::Failed));
        s.tick(cfg.retry_backoff_s + 0.1, p, &interest, &mut out);
        assert!(
            out.iter()
                .any(|a| matches!(a, Action::StartIo { coord, .. } if *coord == c)),
            "retried after backoff"
        );
    }
}
