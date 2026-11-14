//! Streaming and rendering diagnostics: a stats resource and optional gizmo visualisation.
//!
//! [`StreamingStats`] is filled by the plugin each frame from the scheduler and the executor.
//! Renderer integrations can fill the render-side fields (`visible_*`, `lod_distribution`, …) —
//! there is no upstream public counter for visible meshlets yet, see
//! `docs/open_world/bevy_audit.md`.

use crate::{
    lod::Representation,
    scheduler::{CellState, SchedulerStats},
};
use bevy_ecs::resource::Resource;
use bevy_math::DVec3;

/// All numbers shown by the diagnostics overlay.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct StreamingStats {
    /// Cells per [`CellState`] (index = `state as usize`).
    pub cells_by_state: [u32; CellState::COUNT],
    /// CPU bytes reserved by cells (resident + in flight).
    pub cpu_resident_bytes: u64,
    /// GPU bytes reserved by cells (resident + in flight).
    pub gpu_resident_bytes: u64,
    /// Requests currently waiting for or performing IO.
    pub pending_requests: u32,
    /// Smoothed storage read bandwidth, bytes/s.
    pub io_bandwidth_bps: f64,
    /// Smoothed GPU upload bandwidth, bytes/s.
    pub upload_bandwidth_bps: f64,
    /// Cells evicted so far.
    pub evictions: u64,
    /// Requests cancelled so far.
    pub cancels: u64,
    /// Failed loads so far.
    pub failures: u64,
    /// Speculative cells admitted / that proved useful / that were wasted.
    pub speculative: (u64, u64, u64),
    /// Frames in which a cell inside the critical radius was not resident.
    pub critical_miss_frames: u64,
    /// Smoothed focus speed, m/s.
    pub focus_speed: f64,
    /// Predicted focus position at the end of the prediction horizon.
    pub predicted_position: DVec3,
    /// Frame time in milliseconds (filled by the app if desired).
    pub frame_time_ms: f32,
    /// Visible meshlets/clusters/instances this frame (renderer-filled).
    pub visible_clusters: u64,
    /// Culled (frustum/LOD) clusters this frame (renderer-filled).
    pub culled_clusters: u64,
    /// Occluded clusters this frame (renderer-filled).
    pub occluded_clusters: u64,
    /// Histogram of drawn [`Representation`]s (index = `representation as usize`).
    pub lod_distribution: [u32; Representation::COUNT],
    /// Resident geometry pages and their bytes (from `PageTable`).
    pub geometry_pages_resident: (u32, u64),
}

impl StreamingStats {
    /// Updates scheduler-derived fields.
    pub fn apply_scheduler(&mut self, s: &SchedulerStats) {
        self.cells_by_state = s.cells_by_state;
        self.cpu_resident_bytes = s.cpu_bytes;
        self.gpu_resident_bytes = s.gpu_bytes;
        self.pending_requests = s.inflight_io;
        self.evictions = s.evictions;
        self.cancels = s.cancels;
        self.failures = s.failures;
        self.speculative = (
            s.speculative_loaded,
            s.speculative_hits,
            s.speculative_wasted,
        );
    }

    /// Human-readable multi-line summary for an on-screen overlay.
    pub fn summary(&self) -> String {
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        let c = |s: CellState| self.cells_by_state[s as usize];
        format!(
            "cells  loading {} | decoding {} | uploading {} | resident {} | active {}\n\
             GPU {:.0} MiB | CPU {:.0} MiB | pending {}\n\
             IO {:.1} MB/s | upload {:.1} MB/s | speed {:.0} km/h\n\
             evicted {} cancelled {} failed {} | spec {}/{}/{} (loaded/hit/waste)\n\
             visible {} culled {} occluded {} | critical-miss frames {}",
            c(CellState::Loading) + c(CellState::Loaded),
            c(CellState::Decoding) + c(CellState::Prepared),
            c(CellState::Uploading),
            c(CellState::Resident),
            c(CellState::Active),
            mib(self.gpu_resident_bytes),
            mib(self.cpu_resident_bytes),
            self.pending_requests,
            self.io_bandwidth_bps / 1.0e6,
            self.upload_bandwidth_bps / 1.0e6,
            self.focus_speed * 3.6,
            self.evictions,
            self.cancels,
            self.failures,
            self.speculative.0,
            self.speculative.1,
            self.speculative.2,
            self.visible_clusters,
            self.culled_clusters,
            self.occluded_clusters,
            self.critical_miss_frames,
        )
    }
}

/// Which debug layers to draw with [`draw_streaming_gizmos`].
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamingDebug {
    /// Draw a rectangle for every tracked cell, colored by state.
    pub cells: bool,
    /// Draw circles for mandatory/load/activation radii around the focus.
    pub radii: bool,
    /// Draw the predicted path and position.
    pub prediction: bool,
}

impl Default for StreamingDebug {
    fn default() -> Self {
        Self {
            cells: true,
            radii: true,
            prediction: true,
        }
    }
}

/// Color (sRGB, linear-ish) used for each cell state in debug drawing.
pub fn state_color(s: CellState) -> [f32; 3] {
    match s {
        CellState::Loading | CellState::Loaded => [1.0, 0.8, 0.1], // loading: yellow
        CellState::Decoding | CellState::Prepared => [1.0, 0.5, 0.1], // decoding: orange
        CellState::Uploading => [0.8, 0.3, 1.0],                   // uploading: purple
        CellState::Resident => [0.2, 0.6, 1.0],                    // resident: blue
        CellState::Active => [0.2, 1.0, 0.3],                      // active: green
        CellState::Failed => [1.0, 0.1, 0.1],                      // failed: red
    }
}

#[cfg(feature = "gizmos")]
pub use gizmos::draw_streaming_gizmos;

#[cfg(feature = "gizmos")]
mod gizmos {
    use super::{state_color, StreamingDebug};
    use crate::plugin::StreamingState;
    use bevy_color::Color;
    use bevy_ecs::system::{Res, ResMut};
    use bevy_gizmos::gizmos::Gizmos;
    use bevy_math::{Isometry3d, Quat, Vec2, Vec3};

    /// Draws cell rectangles (colored by state), radii and the predicted path, in the floating
    /// origin frame (`origin_offset` subtracted from absolute positions).
    pub fn draw_streaming_gizmos(
        mut gizmos: Gizmos,
        state: ResMut<StreamingState>,
        debug: Res<StreamingDebug>,
    ) {
        let cfg = state.scheduler.config().clone();
        let o = state.origin_offset;
        let to_local = |p: bevy_math::DVec3| -> Vec3 { (p - o).as_vec3() };
        let size = cfg.grid.cell_size as f32;
        if debug.cells {
            for (c, s) in state.scheduler.iter() {
                let col = state_color(s);
                let center = to_local(cfg.grid.cell_center(c)) + Vec3::Y * 0.5;
                gizmos.rect(
                    Isometry3d::new(center, Quat::from_rotation_x(core::f32::consts::FRAC_PI_2)),
                    Vec2::splat(size * 0.96),
                    Color::srgb(col[0], col[1], col[2]),
                );
            }
        }
        let focus = to_local(state.motion.position);
        if debug.radii {
            for (r, col) in [
                (cfg.mandatory_radius, Color::srgb(1.0, 0.2, 0.2)),
                (cfg.activation_radius, Color::srgb(0.2, 1.0, 0.3)),
                (cfg.load_radius, Color::srgb(0.3, 0.6, 1.0)),
                (
                    cfg.load_radius + cfg.unload_hysteresis,
                    Color::srgb(0.5, 0.5, 0.5),
                ),
            ] {
                gizmos.circle(
                    Isometry3d::new(
                        focus + Vec3::Y,
                        Quat::from_rotation_x(core::f32::consts::FRAC_PI_2),
                    ),
                    r as f32,
                    col,
                );
            }
        }
        if debug.prediction {
            let mut prev = focus;
            let mut t = cfg.prediction_step_s;
            while t <= cfg.prediction_horizon_s + 1e-9 {
                let p = to_local(state.motion.predict(t, cfg.max_prediction_accel)) + Vec3::Y;
                gizmos.line(prev, p, Color::srgb(1.0, 1.0, 0.0));
                prev = p;
                t += cfg.prediction_step_s;
            }
        }
    }
}
