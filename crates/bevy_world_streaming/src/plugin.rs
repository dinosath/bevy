//! Thin ECS glue around the pure scheduler.
//!
//! Systems (all in `Update`, ordered by [`StreamingSystems`]):
//!
//! ```text
//! Focus  read the StreamingFocus transform → MotionPredictor
//! Poll   poll IO/decode tasks with `check_ready` (never blocks) → scheduler completions
//! Plan   build interest, scheduler.tick(), execute actions (spawn tasks, emit messages)
//! Stats  publish StreamingStats
//! ```
//!
//! IO runs on the `IoTaskPool`, decoding on the `AsyncComputeTaskPool`; both are bounded by the
//! scheduler's budgets, and nothing in these systems blocks the main thread.

use crate::{
    coords::CellCoord,
    diagnostics::StreamingStats,
    predict::{InterestBuilder, MotionPredictor, MotionState, Route},
    scheduler::{Action, CellBytes, StreamingConfig, StreamingScheduler},
};
use alloc::sync::Arc;
use bevy_app::{App, Plugin, Update};
use bevy_ecs::{
    component::Component,
    message::{Message, MessageWriter},
    query::With,
    resource::Resource,
    schedule::{IntoScheduleConfigs, SystemSet},
    system::{Query, Res, ResMut},
};
use bevy_math::DVec3;
use bevy_platform::collections::HashMap;
use bevy_tasks::{futures::check_ready, AsyncComputeTaskPool, IoTaskPool, Task};
use bevy_time::Time;
use bevy_transform::components::GlobalTransform;
use core::{any::Any, future::Future, pin::Pin};

/// Marks the entity (camera or vehicle) whose [`GlobalTransform`] drives streaming.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct StreamingFocus;

/// Raw bytes of a cell plus its size accounting, produced by a [`CellSource`].
#[derive(Clone)]
pub struct CellBlob {
    /// Encoded payload.
    pub payload: Arc<[u8]>,
    /// Memory the cell will occupy once resident.
    pub bytes: CellBytes,
}

/// A decoded cell ready for upload/activation. `data` is application-defined.
#[derive(Clone)]
pub struct PreparedCell {
    /// The cell.
    pub coord: CellCoord,
    /// Memory accounting.
    pub bytes: CellBytes,
    /// Application data (instance tables, geometry page lists, …).
    pub data: Arc<dyn Any + Send + Sync>,
}

/// Failure of a load or decode.
#[derive(Debug, Clone)]
pub struct CellLoadError(pub String);

impl core::fmt::Display for CellLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cell load failed: {}", self.0)
    }
}

impl core::error::Error for CellLoadError {}

/// Boxed future returned by [`CellSource::read_cell`].
pub type CellFuture<T> = Pin<Box<dyn Future<Output = Result<T, CellLoadError>> + Send + 'static>>;

/// Abstracts cell storage (loose files, pack files, network, in-memory for tests).
pub trait CellSource: Send + Sync + 'static {
    /// Reads the encoded cell. Runs on the IO task pool; may be slow, must not touch the ECS.
    fn read_cell(&self, coord: CellCoord) -> CellFuture<CellBlob>;

    /// Expected size before loading (from a manifest), if known.
    fn size_hint(&self, _coord: CellCoord) -> Option<CellBytes> {
        None
    }
}

/// Decodes/prepares a loaded cell on the compute task pool (decompress, build instance
/// tables, …).
pub trait CellDecoder: Send + Sync + 'static {
    /// Decodes `blob` into application data.
    fn decode(&self, coord: CellCoord, blob: CellBlob) -> Result<PreparedCell, CellLoadError>;
}

/// How GPU upload is completed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UploadMode {
    /// The plugin completes uploads on the next frame (CPU-only apps, tests, prototyping).
    #[default]
    Immediate,
    /// A renderer integration drains [`StreamingState::take_pending_uploads`] within its own
    /// budget and calls [`StreamingState::complete_upload`].
    External,
}

/// A cell activated (spawn its content now).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellActivated(pub CellCoord);
/// A cell deactivated (despawn its content; data stays resident).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellDeactivated(pub CellCoord);
/// A cell evicted (all its data was freed).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellEvicted(pub CellCoord);

/// System sets for external ordering.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamingSystems {
    /// Reads the focus transform.
    Focus,
    /// Polls tasks and applies completions.
    Poll,
    /// Plans and executes scheduler actions.
    Plan,
    /// Publishes statistics.
    Stats,
}

/// The streaming runtime state.
#[derive(Resource)]
pub struct StreamingState {
    /// The scheduler.
    pub scheduler: StreamingScheduler,
    /// Optional route (road centerline) enabling route-aware prefetch.
    pub route: Option<Route>,
    /// Extra mandatory targets (teleport destinations).
    pub targets: Vec<DVec3>,
    /// Floating-origin offset: absolute position = `Transform` position + this.
    pub origin_offset: DVec3,
    /// Latest motion estimate.
    pub motion: MotionState,
    /// Upload completion mode.
    pub upload_mode: UploadMode,
    predictor: MotionPredictor,
    builder: InterestBuilder,
    source: Arc<dyn CellSource>,
    decoder: Arc<dyn CellDecoder>,
    io_tasks: HashMap<(CellCoord, u32), Task<Result<CellBlob, CellLoadError>>>,
    loaded: HashMap<CellCoord, (u32, CellBlob)>,
    decode_tasks: HashMap<(CellCoord, u32), Task<Result<PreparedCell, CellLoadError>>>,
    prepared: HashMap<CellCoord, PreparedCell>,
    pending_uploads: Vec<(CellCoord, u32, u64)>,
    ready_uploads: Vec<(CellCoord, u32)>,
    actions: Vec<Action>,
    io_bytes_frame: u64,
    upload_bytes_frame: u64,
    now: f64,
}

impl StreamingState {
    /// Decoded data of a resident cell, for spawning its content on [`CellActivated`].
    pub fn prepared(&self, c: CellCoord) -> Option<&PreparedCell> {
        self.prepared.get(&c)
    }

    /// Drains uploads awaiting the renderer (`UploadMode::External`): `(cell, generation, gpu bytes)`.
    pub fn take_pending_uploads(&mut self) -> Vec<(CellCoord, u32, u64)> {
        core::mem::take(&mut self.pending_uploads)
    }

    /// Marks an upload finished (`UploadMode::External`). Returns `false` if stale.
    pub fn complete_upload(&mut self, c: CellCoord, generation: u32) -> bool {
        self.scheduler.upload_complete(c, generation)
    }
}

/// Adds world streaming. Insert a [`StreamingFocus`] entity to drive it.
pub struct WorldStreamingPlugin {
    /// Streaming configuration.
    pub config: StreamingConfig,
    /// Storage backend.
    pub source: Arc<dyn CellSource>,
    /// Decoder.
    pub decoder: Arc<dyn CellDecoder>,
    /// Upload completion mode.
    pub upload_mode: UploadMode,
}

impl Plugin for WorldStreamingPlugin {
    fn build(&self, app: &mut App) {
        let mut scheduler = StreamingScheduler::new(self.config.clone());
        let source = self.source.clone();
        scheduler.set_size_hint({
            let src = source.clone();
            let default = self.config.default_cell_bytes;
            move |c| src.size_hint(c).unwrap_or(default)
        });
        app.insert_resource(StreamingState {
            scheduler,
            route: None,
            targets: Vec::new(),
            origin_offset: DVec3::ZERO,
            motion: MotionState::default(),
            upload_mode: self.upload_mode,
            predictor: MotionPredictor::default(),
            builder: InterestBuilder::default(),
            source,
            decoder: self.decoder.clone(),
            io_tasks: HashMap::default(),
            loaded: HashMap::default(),
            decode_tasks: HashMap::default(),
            prepared: HashMap::default(),
            pending_uploads: Vec::new(),
            ready_uploads: Vec::new(),
            actions: Vec::new(),
            io_bytes_frame: 0,
            upload_bytes_frame: 0,
            now: 0.0,
        })
        .init_resource::<StreamingStats>()
        .add_message::<CellActivated>()
        .add_message::<CellDeactivated>()
        .add_message::<CellEvicted>()
        .configure_sets(
            Update,
            (
                StreamingSystems::Focus,
                StreamingSystems::Poll,
                StreamingSystems::Plan,
                StreamingSystems::Stats,
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                update_focus.in_set(StreamingSystems::Focus),
                poll_tasks.in_set(StreamingSystems::Poll),
                plan_streaming.in_set(StreamingSystems::Plan),
                publish_stats.in_set(StreamingSystems::Stats),
            ),
        );
    }
}

fn update_focus(
    time: Res<Time>,
    focus: Query<&GlobalTransform, With<StreamingFocus>>,
    mut state: ResMut<StreamingState>,
) {
    let Some(gt) = focus.iter().next() else {
        return;
    };
    let pos = gt.translation().as_dvec3() + state.origin_offset;
    let dt = f64::from(time.delta_secs());
    state.now += dt;
    state.motion = state.predictor.push(pos, dt);
}

fn poll_tasks(mut state: ResMut<StreamingState>) {
    let state = &mut *state;
    state.io_bytes_frame = 0;
    state.upload_bytes_frame = 0;

    // Uploads promised last frame (Immediate mode).
    for (c, g) in state.ready_uploads.drain(..) {
        state.scheduler.upload_complete(c, g);
    }

    let now = state.now;
    let mut done = Vec::new();
    for (key, task) in &mut state.io_tasks {
        if let Some(res) = check_ready(task) {
            done.push((*key, res));
        }
    }
    for ((c, g), res) in done {
        state.io_tasks.remove(&(c, g));
        match res {
            Ok(blob) => {
                if state.scheduler.io_complete(c, g, blob.bytes) {
                    state.io_bytes_frame += blob.bytes.cpu;
                    state.loaded.insert(c, (g, blob));
                }
            }
            Err(_) => {
                state.scheduler.failed(c, g, now);
            }
        }
    }

    let mut done = Vec::new();
    for (key, task) in &mut state.decode_tasks {
        if let Some(res) = check_ready(task) {
            done.push((*key, res));
        }
    }
    for ((c, g), res) in done {
        state.decode_tasks.remove(&(c, g));
        match res {
            Ok(cell) => {
                if state.scheduler.decode_complete(c, g) {
                    state.prepared.insert(c, cell);
                }
            }
            Err(_) => {
                state.scheduler.failed(c, g, now);
            }
        }
    }
}

fn plan_streaming(
    mut state: ResMut<StreamingState>,
    mut activated: MessageWriter<CellActivated>,
    mut deactivated: MessageWriter<CellDeactivated>,
    mut evicted: MessageWriter<CellEvicted>,
) {
    let state = &mut *state;
    let cfg = state.scheduler.config().clone();
    let interest = state
        .builder
        .build(&cfg, &state.motion, state.route.as_ref(), &state.targets);
    let mut actions = core::mem::take(&mut state.actions);
    state
        .scheduler
        .tick(state.now, state.motion.position, interest, &mut actions);

    for a in actions.drain(..) {
        match a {
            Action::StartIo { coord, generation } => {
                let src = state.source.clone();
                let task = IoTaskPool::get().spawn(async move { src.read_cell(coord).await });
                state.io_tasks.insert((coord, generation), task);
            }
            Action::CancelIo { coord, generation } => {
                state.io_tasks.remove(&(coord, generation));
                state.loaded.remove(&coord);
            }
            Action::StartDecode { coord, generation } => {
                if let Some((g, blob)) = state.loaded.remove(&coord) {
                    debug_assert_eq!(g, generation);
                    let dec = state.decoder.clone();
                    let task =
                        AsyncComputeTaskPool::get().spawn(async move { dec.decode(coord, blob) });
                    state.decode_tasks.insert((coord, generation), task);
                }
            }
            Action::StartUpload {
                coord,
                generation,
                bytes,
            } => {
                state.upload_bytes_frame += bytes;
                match state.upload_mode {
                    UploadMode::Immediate => state.ready_uploads.push((coord, generation)),
                    UploadMode::External => state.pending_uploads.push((coord, generation, bytes)),
                }
            }
            Action::Activate(c) => {
                activated.write(CellActivated(c));
            }
            Action::Deactivate(c) => {
                deactivated.write(CellDeactivated(c));
            }
            Action::Evict(c) => {
                state.prepared.remove(&c);
                evicted.write(CellEvicted(c));
            }
        }
    }
    state.actions = actions;
}

fn publish_stats(time: Res<Time>, state: Res<StreamingState>, mut stats: ResMut<StreamingStats>) {
    let dt = f64::from(time.delta_secs()).max(1e-6);
    let a = 0.1;
    let io = state.io_bytes_frame as f64 / dt;
    let up = state.upload_bytes_frame as f64 / dt;
    stats.io_bandwidth_bps += (io - stats.io_bandwidth_bps) * a;
    stats.upload_bandwidth_bps += (up - stats.upload_bandwidth_bps) * a;
    stats.apply_scheduler(state.scheduler.stats());
    stats.focus_speed = state.motion.speed();
    let cfg = state.scheduler.config();
    stats.predicted_position = state
        .motion
        .predict(cfg.prediction_horizon_s, cfg.max_prediction_accel);
    // A frame is a critical miss when any cell inside the mandatory ring is not resident.
    let mut miss = false;
    cfg.grid
        .for_each_cell_in_radius(state.motion.position, cfg.mandatory_radius, |c, _| {
            miss |= !state.scheduler.is_resident(c);
        });
    stats.critical_miss_frames += u64::from(miss);
}
