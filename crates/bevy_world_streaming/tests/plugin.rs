//! Integration test of the ECS glue with real task pools and an in-memory cell source.

use bevy_app::{App, TaskPoolPlugin};
use bevy_ecs::message::Messages;
use bevy_math::Vec3;
use bevy_time::TimePlugin;
use bevy_transform::components::{GlobalTransform, Transform};
use bevy_world_streaming::prelude::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct MemorySource {
    reads: Arc<AtomicUsize>,
}

impl CellSource for MemorySource {
    fn read_cell(&self, _coord: CellCoord) -> CellFuture<CellBlob> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(CellBlob {
                payload: Arc::from(vec![0u8; 16]),
                bytes: CellBytes {
                    cpu: 1 << 20,
                    gpu: 1 << 20,
                },
            })
        })
    }
}

struct PassThrough;
impl CellDecoder for PassThrough {
    fn decode(&self, coord: CellCoord, blob: CellBlob) -> Result<PreparedCell, CellLoadError> {
        Ok(PreparedCell {
            coord,
            bytes: blob.bytes,
            data: Arc::new(blob.payload.len()),
        })
    }
}

#[test]
fn mandatory_cells_become_active_and_messages_are_emitted() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        WorldStreamingPlugin {
            config: StreamingConfig::default(),
            source: Arc::new(MemorySource {
                reads: reads.clone(),
            }),
            decoder: Arc::new(PassThrough),
            upload_mode: UploadMode::Immediate,
        },
    ));
    app.world_mut().spawn((
        StreamingFocus,
        Transform::from_translation(Vec3::new(64.0, 0.0, 64.0)),
        GlobalTransform::from_translation(Vec3::new(64.0, 0.0, 64.0)),
    ));

    let home = CellCoord::new(0, 0);
    let mut activated = false;
    for _ in 0..2_000 {
        app.update();
        std::thread::sleep(std::time::Duration::from_micros(200));
        let mut msgs = app.world_mut().resource_mut::<Messages<CellActivated>>();
        activated |= msgs.drain().any(|m| m.0 == home);
        if activated {
            break;
        }
    }
    assert!(activated, "home cell was never activated");
    let st = app.world().resource::<StreamingState>();
    assert_eq!(st.scheduler.state(home), Some(CellState::Active));
    assert!(
        st.prepared(home).is_some(),
        "decoded data available for spawning"
    );
    // IO is bounded.
    let cfg = st.scheduler.config();
    assert!(st.scheduler.stats().inflight_io as usize <= cfg.max_inflight_io);
    assert!(reads.load(Ordering::SeqCst) > 0);
}
