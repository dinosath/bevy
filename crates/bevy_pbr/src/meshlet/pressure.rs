//! Queue pressure detection and graceful degradation for the meshlet renderer.
//!
//! The cull queues are fixed-size (`MeshletPlugin::cluster_buffer_slots`). The cull shaders append
//! with `atomicAdd` and do not check capacity, so an undersized queue silently produces missing or
//! blinking meshes. This module:
//!
//! 1. copies the relevant GPU counters (nodes per BVH level, early+late candidate clusters) into a
//!    small *probe* buffer just before they are consumed — 4-byte GPU-side copies, no shader changes;
//! 2. reads the probe back asynchronously (1–2 frames of latency, never stalling);
//! 3. feeds the peak occupancy to [`MeshletPressure`], which raises a LOD error scale
//!    (`lod_error_scale` immediate in the cull shaders) when a queue overflows or nears capacity, and
//!    lowers it again after sustained headroom. Quality degrades (coarser LOD) instead of geometry
//!    disappearing.

use alloc::sync::Arc;
use bevy_ecs::{
    resource::Resource,
    system::{Res, ResMut},
};
use bevy_render::{
    render_resource::{Buffer, BufferDescriptor, BufferUsages, CommandEncoder, MapMode},
    renderer::RenderDevice,
};
use core::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, warn};

use super::resource_manager::ResourceManager;

/// Words in the probe reserved for per-BVH-level node counts.
pub(crate) const PROBE_BVH_WORDS: u32 = 4096;
/// Words in the probe reserved for (early, late) candidate-cluster count pairs.
pub(crate) const PROBE_PAIR_WORDS: u32 = 512;
const PROBE_WORDS: u32 = PROBE_BVH_WORDS + PROBE_PAIR_WORDS;
const STAGING_SLOTS: usize = 3;

/// Maximum multiplier the pressure controller will apply to the LOD error budget.
pub const MESHLET_MAX_PRESSURE_SCALE: f32 = 16.0;

/// Render-world state of the meshlet queue pressure controller.
///
/// `lod_error_scale` multiplies the LOD error budget of every meshlet view (on top of
/// [`MeshletSettings::shadow_lod_error_scale`](super::MeshletSettings) for shadow views). It is `1.0`
/// when queues have headroom.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct MeshletPressure {
    /// Current LOD error multiplier of the controller (`1.0 ..= 16.0`). Applied to all meshlet views
    /// only if [`MeshletSettings::degrade_on_queue_pressure`](super::MeshletSettings) is `true`;
    /// otherwise it only reports what *would* be applied.
    pub lod_error_scale: f32,
    /// Peak queue occupancy (entries) observed in the most recent completed readback.
    pub peak_queue_entries: u32,
    /// Queue capacity in entries (`cluster_buffer_slots`).
    pub queue_capacity: u32,
    /// Number of readbacks in which a queue exceeded its capacity (geometry was dropped).
    pub overflow_events: u64,
    clean_frames: u32,
}

impl MeshletPressure {
    pub(crate) fn new(queue_capacity: u32) -> Self {
        Self {
            lod_error_scale: 1.0,
            peak_queue_entries: 0,
            queue_capacity,
            overflow_events: 0,
            clean_frames: 0,
        }
    }

    /// Feeds one observation of peak queue occupancy and updates the scale.
    ///
    /// * `peak > capacity` — overflow: ×1.25 (geometry was dropped this frame).
    /// * `peak > 90 %` — near capacity: ×1.05, pre-empting overflow.
    /// * `peak < 60 %` for 120 consecutive observations — headroom: ×0.98 back towards 1.0.
    pub fn observe(&mut self, peak: u32) {
        self.peak_queue_entries = peak;
        let cap = u64::from(self.queue_capacity.max(1));
        let peak64 = u64::from(peak);
        if peak64 > cap {
            self.overflow_events += 1;
            self.lod_error_scale = (self.lod_error_scale * 1.25).min(MESHLET_MAX_PRESSURE_SCALE);
            self.clean_frames = 0;
        } else if peak64 * 10 > cap * 9 {
            self.lod_error_scale = (self.lod_error_scale * 1.05).min(MESHLET_MAX_PRESSURE_SCALE);
            self.clean_frames = 0;
        } else if peak64 * 10 < cap * 6 {
            self.clean_frames += 1;
            if self.clean_frames >= 120 {
                self.lod_error_scale = (self.lod_error_scale * 0.98).max(1.0);
            }
        } else {
            self.clean_frames = 0;
        }
    }
}

enum SlotState {
    Idle,
    Mapping(Arc<AtomicBool>),
}

/// GPU probe buffer + readback ring (render world).
#[derive(Resource)]
pub(crate) struct PressureProbe {
    probe: Buffer,
    staging: Vec<Buffer>,
    state: Vec<SlotState>,
    /// Slot chosen for this frame's copy (set in `prepare`), if any is idle.
    write_slot: Option<usize>,
    /// Set by the first raster-node invocation of the frame that records probe writes.
    claimed: AtomicBool,
    /// Set when a copy into `write_slot` was recorded this frame.
    copied: AtomicBool,
}

impl PressureProbe {
    pub(crate) fn new(render_device: &RenderDevice) -> Self {
        let size = u64::from(PROBE_WORDS) * 4;
        let mk = |label, usage| {
            render_device.create_buffer(&BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
            probe: mk(
                "meshlet_pressure_probe",
                BufferUsages::COPY_DST | BufferUsages::COPY_SRC,
            ),
            staging: (0..STAGING_SLOTS)
                .map(|_| {
                    mk(
                        "meshlet_pressure_probe_staging",
                        BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                    )
                })
                .collect(),
            state: (0..STAGING_SLOTS).map(|_| SlotState::Idle).collect(),
            write_slot: None,
            claimed: AtomicBool::new(false),
            copied: AtomicBool::new(false),
        }
    }

    /// Called once per raster-node invocation. Returns a writer only for the first invocation of a
    /// frame (later cameras skip probing) and only if a staging slot is free.
    pub(crate) fn begin(&self, encoder: &mut CommandEncoder, enabled: bool) -> ProbeWriter<'_> {
        if !enabled || self.write_slot.is_none() || self.claimed.swap(true, Ordering::AcqRel) {
            return ProbeWriter {
                probe: None,
                bvh: 0,
                pair: 0,
            };
        }
        encoder.clear_buffer(&self.probe, 0, None);
        ProbeWriter {
            probe: Some(&self.probe),
            bvh: 0,
            pair: 0,
        }
    }

    /// Copies the probe into this frame's staging slot. Call once at the end of the node, with the
    /// writer returned by [`PressureProbe::begin`].
    pub(crate) fn finish(&self, encoder: &mut CommandEncoder, writer: &ProbeWriter<'_>) {
        if let (Some(slot), Some(_)) = (self.write_slot, writer.probe) {
            encoder.copy_buffer_to_buffer(
                &self.probe,
                0,
                &self.staging[slot],
                0,
                u64::from(PROBE_WORDS) * 4,
            );
            self.copied.store(true, Ordering::Release);
        }
    }
}

/// Records GPU-side copies of counters into the probe buffer.
pub(crate) struct ProbeWriter<'a> {
    probe: Option<&'a Buffer>,
    bvh: u32,
    pair: u32,
}

impl ProbeWriter<'_> {
    /// Copies a node count (one BVH level) into the next free BVH word.
    pub(crate) fn copy_bvh_level(&mut self, encoder: &mut CommandEncoder, count: &Buffer) {
        let Some(probe) = self.probe else { return };
        if self.bvh >= PROBE_BVH_WORDS {
            return;
        }
        encoder.copy_buffer_to_buffer(count, 0, probe, u64::from(self.bvh) * 4, 4);
        self.bvh += 1;
    }

    /// Copies the (early, late) candidate cluster counters into the next free pair.
    pub(crate) fn copy_pair(
        &mut self,
        encoder: &mut CommandEncoder,
        early: &Buffer,
        late: &Buffer,
    ) {
        let Some(probe) = self.probe else { return };
        if self.pair + 2 > PROBE_PAIR_WORDS {
            return;
        }
        let base = u64::from(PROBE_BVH_WORDS + self.pair) * 4;
        encoder.copy_buffer_to_buffer(early, 0, probe, base, 4);
        encoder.copy_buffer_to_buffer(late, 0, probe, base + 4, 4);
        self.pair += 2;
    }
}

/// `PrepareResources`: choose this frame's staging slot and reset per-frame flags.
pub(crate) fn prepare_pressure_probe(mut probe: ResMut<PressureProbe>) {
    probe.claimed.store(false, Ordering::Release);
    probe.copied.store(false, Ordering::Release);
    probe.write_slot = probe
        .state
        .iter()
        .position(|s| matches!(s, SlotState::Idle));
}

/// `Cleanup` (after the frame's commands were submitted): finish completed readbacks, then start
/// mapping this frame's slot.
pub(crate) fn readback_pressure(
    mut probe: ResMut<PressureProbe>,
    mut pressure: ResMut<MeshletPressure>,
    resource_manager: Res<ResourceManager>,
) {
    pressure.queue_capacity = resource_manager.cull_queue_slots();
    let probe = &mut *probe;

    // 1. Complete finished readbacks.
    for (i, state) in probe.state.iter_mut().enumerate() {
        let SlotState::Mapping(ready) = state else {
            continue;
        };
        if !ready.load(Ordering::Acquire) {
            continue;
        }
        let slice = probe.staging[i].slice(..);
        let mut peak = 0u32;
        if let Ok(view) = slice.get_mapped_range() {
            let words: &[u32] = bytemuck::cast_slice(&view);
            let (bvh, pairs) = words.split_at(PROBE_BVH_WORDS as usize);
            peak = bvh.iter().copied().max().unwrap_or(0);
            for p in pairs.as_chunks::<2>().0 {
                peak = peak.max(p[0].saturating_add(p[1]));
            }
        }
        probe.staging[i].unmap();
        *state = SlotState::Idle;
        let before = pressure.overflow_events;
        pressure.observe(peak);
        debug!(
            "meshlet queue pressure: peak {}/{} entries, lod error scale {:.3}, overflow events {}",
            peak, pressure.queue_capacity, pressure.lod_error_scale, pressure.overflow_events
        );
        // Warn on the first overflow and then once per 120 events: an unfixed overflow repeats every frame.
        if pressure.overflow_events > before
            && (pressure.overflow_events == 1 || pressure.overflow_events.is_multiple_of(120))
        {
            warn!(
                "Meshlet cull queue overflow: peak {} entries > capacity {} \
                 (raise `MeshletPlugin::cluster_buffer_slots`). Coarsening LOD (error scale {:.2}) \
                 to avoid dropped geometry.",
                peak, pressure.queue_capacity, pressure.lod_error_scale
            );
        }
    }

    // 2. Start mapping this frame's copy.
    if probe.copied.swap(false, Ordering::AcqRel)
        && let Some(slot) = probe.write_slot.take()
    {
        let ready = Arc::new(AtomicBool::new(false));
        let flag = ready.clone();
        probe.staging[slot]
            .slice(..)
            .map_async(MapMode::Read, move |res| {
                if res.is_ok() {
                    flag.store(true, Ordering::Release);
                }
            });
        probe.state[slot] = SlotState::Mapping(ready);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_raises_scale_geometrically_and_is_capped() {
        let mut p = MeshletPressure::new(1000);
        p.observe(1500);
        assert!((p.lod_error_scale - 1.25).abs() < 1e-6);
        assert_eq!(p.overflow_events, 1);
        for _ in 0..100 {
            p.observe(5000);
        }
        assert_eq!(p.lod_error_scale, MESHLET_MAX_PRESSURE_SCALE);
    }

    #[test]
    fn near_capacity_preempts_without_counting_overflow() {
        let mut p = MeshletPressure::new(1000);
        p.observe(950);
        assert!(p.lod_error_scale > 1.0);
        assert_eq!(p.overflow_events, 0);
    }

    #[test]
    fn headroom_only_relaxes_after_sustained_clean_frames_and_never_below_one() {
        let mut p = MeshletPressure::new(1000);
        p.observe(2000);
        let raised = p.lod_error_scale;
        for _ in 0..119 {
            p.observe(100);
        }
        assert_eq!(
            p.lod_error_scale, raised,
            "hysteresis: no change before 120 clean frames"
        );
        p.observe(100);
        assert!(p.lod_error_scale < raised);
        for _ in 0..10_000 {
            p.observe(100);
        }
        assert_eq!(p.lod_error_scale, 1.0);
    }

    #[test]
    fn mid_range_occupancy_holds_the_scale_and_resets_the_clean_counter() {
        let mut p = MeshletPressure::new(1000);
        p.observe(2000);
        for _ in 0..100 {
            p.observe(100);
        }
        p.observe(700); // between 60 % and 90 %: hold, reset clean counter
        let held = p.lod_error_scale;
        for _ in 0..119 {
            p.observe(100);
        }
        assert_eq!(p.lod_error_scale, held);
    }

    #[test]
    fn a_stable_system_converges_to_no_overflow() {
        // Model: occupancy ∝ 1/scale². Capacity 1000, demand 4000 at scale 1.
        let mut p = MeshletPressure::new(1000);
        let mut overflows_late = 0;
        for frame in 0..200 {
            let occ = (4000.0 / (p.lod_error_scale * p.lod_error_scale)) as u32;
            let before = p.overflow_events;
            p.observe(occ);
            if frame > 50 && p.overflow_events > before {
                overflows_late += 1;
            }
        }
        assert_eq!(
            overflows_late, 0,
            "controller must converge, scale {}",
            p.lod_error_scale
        );
        assert!(p.lod_error_scale >= 2.0);
    }
}
