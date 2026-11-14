#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod coords;
pub mod diagnostics;
pub mod hlod;
pub mod lod;
pub mod pages;
pub mod plugin;
pub mod predict;
pub mod scheduler;
pub mod sim;

/// Commonly used items.
pub mod prelude {
    pub use crate::{
        coords::{CellCoord, CellGrid, FloatingOrigin, WorldPosition},
        diagnostics::{StreamingDebug, StreamingStats},
        lod::{LodSelector, LodThresholds, Representation, ResidencySet},
        pages::{AssetKey, AssetPages, PageDemand, PageTable},
        plugin::{
            CellActivated, CellBlob, CellDeactivated, CellDecoder, CellEvicted, CellFuture,
            CellLoadError, CellSource, PreparedCell, StreamingFocus, StreamingState,
            StreamingSystems, UploadMode, WorldStreamingPlugin,
        },
        predict::{InterestBuilder, MotionPredictor, MotionState, Route},
        scheduler::{CellBytes, CellState, StreamingConfig, StreamingScheduler},
    };
}
