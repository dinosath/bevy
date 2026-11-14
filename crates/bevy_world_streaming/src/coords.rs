//! Deterministic world coordinates for very large worlds.
//!
//! Authoritative positions are stored as an integer [`CellCoord`] plus an `f32` offset inside the
//! cell ([`WorldPosition`]), or as `f64` ([`DVec3`]). Rendering and physics work in a *floating
//! origin* frame ([`FloatingOrigin`]) so that `f32` values stay small and precise near the camera.
//!
//! The ground plane is X/Z, with Y up. Cell `(x, z)` covers
//! `[x * size, (x + 1) * size) × [z * size, (z + 1) * size)`.

use bevy_math::{DVec3, Vec3};

/// Integer coordinate of a cell on the X/Z ground plane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellCoord {
    /// Cell index along X.
    pub x: i32,
    /// Cell index along Z.
    pub z: i32,
}

impl CellCoord {
    /// Creates a cell coordinate.
    #[inline]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// Returns this coordinate shifted by `(dx, dz)` cells.
    #[inline]
    pub const fn offset(self, dx: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.z + dz)
    }

    /// Chebyshev (chessboard) distance in cells.
    #[inline]
    pub fn chebyshev(self, other: Self) -> u32 {
        self.x.abs_diff(other.x).max(self.z.abs_diff(other.z))
    }
}

/// Position expressed as a cell plus a local offset from that cell's *minimum corner*.
///
/// `local.x` and `local.z` are in `[0, cell_size)`; `local.y` is the (unbounded) height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldPosition {
    /// The cell containing the position.
    pub cell: CellCoord,
    /// Offset from the cell's minimum corner, in meters.
    pub local: Vec3,
}

/// Grid parameters used to convert between world space and cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellGrid {
    /// Side length of a cell in meters. Must be positive and finite.
    pub cell_size: f64,
}

impl Default for CellGrid {
    fn default() -> Self {
        Self { cell_size: 128.0 }
    }
}

impl CellGrid {
    /// Creates a grid with the given cell size in meters.
    ///
    /// # Panics
    /// Panics if `cell_size` is not positive and finite.
    pub fn new(cell_size: f64) -> Self {
        assert!(
            cell_size.is_finite() && cell_size > 0.0,
            "cell_size must be positive and finite"
        );
        Self { cell_size }
    }

    /// Returns the cell containing a world-space position (exact at cell edges, correct for
    /// negative coordinates thanks to `floor`).
    #[inline]
    pub fn cell_of(&self, p: DVec3) -> CellCoord {
        CellCoord::new(
            (p.x / self.cell_size).floor() as i32,
            (p.z / self.cell_size).floor() as i32,
        )
    }

    /// Minimum corner (X/Z, with `y = 0`) of a cell in world space.
    #[inline]
    pub fn cell_min(&self, c: CellCoord) -> DVec3 {
        DVec3::new(
            f64::from(c.x) * self.cell_size,
            0.0,
            f64::from(c.z) * self.cell_size,
        )
    }

    /// Center (X/Z, with `y = 0`) of a cell in world space.
    #[inline]
    pub fn cell_center(&self, c: CellCoord) -> DVec3 {
        self.cell_min(c) + DVec3::new(self.cell_size * 0.5, 0.0, self.cell_size * 0.5)
    }

    /// Horizontal (X/Z) distance from `p` to the rectangle of cell `c`; `0` inside the cell.
    ///
    /// This is the single distance metric used by streaming for every radius test, so that
    /// "in range" is consistent and circular (not square) at cell corners.
    #[inline]
    pub fn distance_to_cell(&self, p: DVec3, c: CellCoord) -> f64 {
        let min = self.cell_min(c);
        let dx = (min.x - p.x).max(0.0).max(p.x - (min.x + self.cell_size));
        let dz = (min.z - p.z).max(0.0).max(p.z - (min.z + self.cell_size));
        dx.hypot(dz)
    }

    /// Converts a world-space position to a [`WorldPosition`].
    pub fn to_world_position(&self, p: DVec3) -> WorldPosition {
        let cell = self.cell_of(p);
        let min = self.cell_min(cell);
        WorldPosition {
            cell,
            local: Vec3::new((p.x - min.x) as f32, p.y as f32, (p.z - min.z) as f32),
        }
    }

    /// Converts a [`WorldPosition`] back to world space.
    pub fn to_world(&self, p: WorldPosition) -> DVec3 {
        let min = self.cell_min(p.cell);
        DVec3::new(
            min.x + f64::from(p.local.x),
            f64::from(p.local.y),
            min.z + f64::from(p.local.z),
        )
    }

    /// Calls `f` for every cell whose rectangle is within `radius` meters of `center`
    /// (inclusive). Iteration order is row-major (`z` outer, `x` inner) and deterministic.
    pub fn for_each_cell_in_radius(
        &self,
        center: DVec3,
        radius: f64,
        mut f: impl FnMut(CellCoord, f64),
    ) {
        if radius < 0.0 {
            return;
        }
        let lo = self.cell_of(center - DVec3::new(radius, 0.0, radius));
        let hi = self.cell_of(center + DVec3::new(radius, 0.0, radius));
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let c = CellCoord::new(x, z);
                let d = self.distance_to_cell(center, c);
                if d <= radius {
                    f(c, d);
                }
            }
        }
    }
}

/// A floating origin: world space is translated so values near the focus stay small in `f32`.
///
/// Call [`FloatingOrigin::update`] each frame with the focus (camera/vehicle) position. When the
/// focus drifts farther than `rebase_distance` from the origin the origin snaps to the focus'
/// cell center and the shift (in meters) is returned so the caller can move every
/// origin-relative `Transform` by `-shift` in one pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingOrigin {
    /// Current origin in absolute world space (`f64`).
    pub origin: DVec3,
    /// Horizontal distance from the origin at which a rebase happens, in meters.
    pub rebase_distance: f64,
}

impl FloatingOrigin {
    /// Creates a floating origin at `origin`.
    pub fn new(origin: DVec3, rebase_distance: f64) -> Self {
        Self {
            origin,
            rebase_distance,
        }
    }

    /// Converts an absolute world position to the origin-relative `f32` frame used for rendering.
    #[inline]
    pub fn to_render(&self, p: DVec3) -> Vec3 {
        (p - self.origin).as_vec3()
    }

    /// Converts a render-space position back to absolute world space.
    #[inline]
    pub fn to_absolute(&self, p: Vec3) -> DVec3 {
        self.origin + p.as_dvec3()
    }

    /// Rebases if `focus` drifted too far. Returns the translation applied to the origin
    /// (new − old), or `None` if no rebase happened.
    pub fn update(&mut self, focus: DVec3, grid: &CellGrid) -> Option<DVec3> {
        let d = DVec3::new(focus.x - self.origin.x, 0.0, focus.z - self.origin.z).length();
        if d <= self.rebase_distance {
            return None;
        }
        let new_origin = grid.cell_center(grid.cell_of(focus));
        let shift = new_origin - self.origin;
        self.origin = new_origin;
        Some(shift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_of_is_exact_at_edges_and_negative() {
        let g = CellGrid::new(128.0);
        assert_eq!(g.cell_of(DVec3::new(0.0, 0.0, 0.0)), CellCoord::new(0, 0));
        assert_eq!(
            g.cell_of(DVec3::new(127.999, 0.0, 128.0)),
            CellCoord::new(0, 1)
        );
        assert_eq!(
            g.cell_of(DVec3::new(-0.001, 0.0, -128.0)),
            CellCoord::new(-1, -1)
        );
        assert_eq!(
            g.cell_of(DVec3::new(-128.001, 0.0, 0.0)),
            CellCoord::new(-2, 0)
        );
    }

    #[test]
    fn world_position_round_trips_far_from_origin() {
        let g = CellGrid::new(256.0);
        for &p in &[
            DVec3::new(1.0e6 + 0.123, 12.5, -2.5e6 + 0.456),
            DVec3::new(-77_777.25, 0.0, 99_999.75),
        ] {
            let wp = g.to_world_position(p);
            assert!(wp.local.x >= 0.0 && wp.local.x < 256.0);
            assert!(wp.local.z >= 0.0 && wp.local.z < 256.0);
            let back = g.to_world(wp);
            // Local f32 offsets within 256 m are precise to ~1e-5 m.
            assert!((back - p).length() < 1.0e-3, "{p:?} -> {back:?}");
        }
    }

    #[test]
    fn distance_to_cell_is_zero_inside_and_circular_at_corners() {
        let g = CellGrid::new(100.0);
        let c = CellCoord::new(0, 0);
        assert_eq!(g.distance_to_cell(DVec3::new(50.0, 0.0, 50.0), c), 0.0);
        let d = g.distance_to_cell(DVec3::new(130.0, 0.0, 140.0), c);
        assert!((d - 50.0).abs() < 1e-9, "{d}"); // 3-4-5 triangle: (30,40) -> 50
    }

    #[test]
    fn radius_iteration_matches_brute_force() {
        let g = CellGrid::new(64.0);
        let center = DVec3::new(-100.0, 0.0, 333.0);
        let r = 300.0;
        let mut got = Vec::new();
        g.for_each_cell_in_radius(center, r, |c, _| got.push(c));
        let mut want = Vec::new();
        for z in -20..=30 {
            for x in -20..=20 {
                let c = CellCoord::new(x, z);
                if g.distance_to_cell(center, c) <= r {
                    want.push(c);
                }
            }
        }
        want.sort();
        let mut got_sorted = got.clone();
        got_sorted.sort();
        assert_eq!(got_sorted, want);
        // Row-major deterministic order.
        assert_eq!(got, {
            let mut s = got.clone();
            s.sort_by_key(|c| (c.z, c.x));
            s
        });
    }

    #[test]
    fn floating_origin_rebase_preserves_world_position() {
        let g = CellGrid::new(128.0);
        let mut fo = FloatingOrigin::new(DVec3::ZERO, 1_000.0);
        let focus = DVec3::new(5_000.25, 3.0, -7_000.5);
        let before = fo.to_absolute(fo.to_render(focus));
        assert!((before - focus).length() < 1e-2);
        let shift = fo.update(focus, &g).expect("must rebase");
        assert!(shift.length() > 1_000.0);
        // Focus is now near the origin in f32...
        assert!(fo.to_render(focus).length() < 128.0);
        // ...and the absolute position is preserved to < 1 mm.
        let after = fo.to_absolute(fo.to_render(focus));
        assert!((after - focus).length() < 1e-3, "{after:?}");
        assert!(fo.update(focus, &g).is_none());
    }
}
