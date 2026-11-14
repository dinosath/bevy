//! Motion prediction, route corridors and interest assembly.
//!
//! The interest set is the union of
//! 1. a **mandatory** ring around the focus (and around explicit targets such as teleport
//!    destinations) — hard guarantee, never evicted;
//! 2. a **wanted** ring (static `load_radius`) biased towards the direction of travel;
//! 3. **speculative** cells along the kinematically predicted path
//!    `p(t) = p₀ + v·t + ½·a·t²` and along the **route corridor** when a [`Route`] is known.
//!
//! Prediction only ever *adds* speculative interest. See `.claude/skills/predictive-streaming`.

use crate::{coords::CellCoord, scheduler::StreamingConfig};
use bevy_math::DVec3;
use bevy_platform::collections::HashMap;

/// Smoothed kinematic state of the streaming focus.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MotionState {
    /// Position in absolute world space.
    pub position: DVec3,
    /// Smoothed velocity in m/s.
    pub velocity: DVec3,
    /// Smoothed acceleration in m/s².
    pub acceleration: DVec3,
}

impl MotionState {
    /// Horizontal speed in m/s.
    #[inline]
    pub fn speed(&self) -> f64 {
        self.velocity.x.hypot(self.velocity.z)
    }

    /// Predicts the position `t` seconds ahead: `p + v·t + ½·a·t²`.
    ///
    /// The acceleration magnitude is clamped to `max_accel` (m/s²) because measured
    /// acceleration is noisy (collisions, bumps) and explodes quadratically.
    pub fn predict(&self, t: f64, max_accel: f64) -> DVec3 {
        let mut a = self.acceleration;
        let m = a.length();
        if m > max_accel && m > 0.0 {
            a *= max_accel / m;
        }
        self.position + self.velocity * t + a * (0.5 * t * t)
    }
}

/// Estimates [`MotionState`] from a stream of positions using exponential smoothing.
///
/// Deterministic: depends only on the pushed `(position, dt)` sequence.
#[derive(Clone, Debug)]
pub struct MotionPredictor {
    state: MotionState,
    initialized: bool,
    have_velocity: bool,
    /// Time constant of the exponential filters, in seconds. Smaller reacts faster but is
    /// noisier.
    pub smoothing_time: f64,
}

impl Default for MotionPredictor {
    fn default() -> Self {
        Self::new(0.25)
    }
}

impl MotionPredictor {
    /// Creates a predictor with the given smoothing time constant (seconds).
    pub fn new(smoothing_time: f64) -> Self {
        Self {
            state: MotionState::default(),
            initialized: false,
            have_velocity: false,
            smoothing_time,
        }
    }

    /// Current state.
    pub fn state(&self) -> MotionState {
        self.state
    }

    /// Forgets velocity/acceleration (call after a teleport) and snaps to `position`.
    pub fn reset(&mut self, position: DVec3) {
        self.state = MotionState {
            position,
            ..MotionState::default()
        };
        self.initialized = true;
        self.have_velocity = false;
    }

    /// Feeds the next position sample, `dt` seconds after the previous one.
    pub fn push(&mut self, position: DVec3, dt: f64) -> MotionState {
        if !self.initialized {
            self.reset(position);
            return self.state;
        }
        if dt <= 0.0 {
            self.state.position = position;
            return self.state;
        }
        let raw_v = (position - self.state.position) / dt;
        let alpha = 1.0 - (-dt / self.smoothing_time.max(1e-6)).exp();
        let prev_v = self.state.velocity;
        let new_v = if self.have_velocity {
            prev_v + (raw_v - prev_v) * alpha
        } else {
            raw_v
        };
        let raw_a = (new_v - prev_v) / dt;
        let new_a = if self.have_velocity {
            self.state.acceleration + (raw_a - self.state.acceleration) * alpha
        } else {
            DVec3::ZERO
        };
        self.state = MotionState {
            position,
            velocity: new_v,
            acceleration: new_a,
        };
        self.have_velocity = true;
        self.state
    }
}

/// Result of projecting a point onto a [`Route`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteProjection {
    /// Arc length along the route of the closest point, in meters.
    pub s: f64,
    /// Horizontal distance from the point to the route, in meters.
    pub lateral: f64,
    /// The closest point on the route.
    pub point: DVec3,
}

/// A polyline route (road centerline / path to a destination) with cumulative arc length.
#[derive(Clone, Debug)]
pub struct Route {
    points: Vec<DVec3>,
    cumulative: Vec<f64>,
}

impl Route {
    /// Builds a route from at least two points. Returns `None` for fewer.
    pub fn new(points: Vec<DVec3>) -> Option<Self> {
        if points.len() < 2 {
            return None;
        }
        let mut cumulative = Vec::with_capacity(points.len());
        let mut acc = 0.0;
        cumulative.push(0.0);
        for w in points.windows(2) {
            acc += (w[1] - w[0]).length();
            cumulative.push(acc);
        }
        Some(Self { points, cumulative })
    }

    /// Total length in meters.
    pub fn length(&self) -> f64 {
        *self.cumulative.last().unwrap_or(&0.0)
    }

    /// The route's control points.
    pub fn points(&self) -> &[DVec3] {
        &self.points
    }

    /// Point at arc length `s` (clamped to the route).
    pub fn point_at(&self, s: f64) -> DVec3 {
        let s = s.clamp(0.0, self.length());
        let i = match self.cumulative.binary_search_by(|c| c.total_cmp(&s)) {
            Ok(i) => return self.points[i],
            Err(i) => i.clamp(1, self.points.len() - 1),
        };
        let (s0, s1) = (self.cumulative[i - 1], self.cumulative[i]);
        let t = if s1 > s0 { (s - s0) / (s1 - s0) } else { 0.0 };
        self.points[i - 1].lerp(self.points[i], t)
    }

    /// Projects `p` (horizontally) onto the route. O(segments); callers may pass a hint via
    /// [`Route::project_near`] for long routes.
    pub fn project(&self, p: DVec3) -> RouteProjection {
        self.project_range(p, 0, self.points.len() - 1)
    }

    /// Like [`Route::project`], but only searches segments within `window` meters of arc length
    /// `hint_s` (coherent frame-to-frame queries are O(window) instead of O(route)).
    pub fn project_near(&self, p: DVec3, hint_s: f64, window: f64) -> RouteProjection {
        let lo = self
            .cumulative
            .partition_point(|&c| c < hint_s - window)
            .saturating_sub(1);
        let hi = self
            .cumulative
            .partition_point(|&c| c <= hint_s + window)
            .min(self.points.len() - 1);
        self.project_range(p, lo, hi.max(lo + 1).min(self.points.len() - 1))
    }

    fn project_range(&self, p: DVec3, first_point: usize, last_point: usize) -> RouteProjection {
        let mut best = RouteProjection {
            s: 0.0,
            lateral: f64::INFINITY,
            point: self.points[0],
        };
        for i in first_point..last_point {
            let (a, b) = (self.points[i], self.points[i + 1]);
            let ab = DVec3::new(b.x - a.x, 0.0, b.z - a.z);
            let len2 = ab.length_squared();
            let t = if len2 > 0.0 {
                (DVec3::new(p.x - a.x, 0.0, p.z - a.z).dot(ab) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let q = a.lerp(b, t);
            let d = (q.x - p.x).hypot(q.z - p.z);
            if d < best.lateral {
                best = RouteProjection {
                    s: self.cumulative[i] + (self.cumulative[i + 1] - self.cumulative[i]) * t,
                    lateral: d,
                    point: q,
                };
            }
        }
        best
    }
}

/// How urgently a cell is needed. Lower tier = more urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InterestTier {
    /// Hard guarantee ring; never evicted, always highest priority.
    Mandatory = 0,
    /// Inside the static load radius.
    Wanted = 1,
    /// Predicted/corridor cell loaded ahead of time; cancellable and IO-capped.
    Speculative = 2,
}

/// One cell the focus is interested in this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InterestCell {
    /// The cell.
    pub coord: CellCoord,
    /// Urgency tier.
    pub tier: InterestTier,
    /// Seconds until the cell is expected to be needed (0 for mandatory). Speculative cells are
    /// ordered by this.
    pub time_to_need: f32,
    /// Heading-biased distance from the focus in meters (lower = earlier in its tier).
    pub sort_distance: f32,
}

/// Builds the per-tick interest set with reusable scratch memory (allocation-free in steady
/// state once capacity has grown).
#[derive(Default)]
pub struct InterestBuilder {
    map: HashMap<CellCoord, InterestCell>,
    out: Vec<InterestCell>,
}

impl InterestBuilder {
    /// Builds the interest set. The returned slice is sorted by
    /// `(tier, time_to_need (speculative only), sort_distance, coord)`, which is a total,
    /// deterministic order.
    ///
    /// * `targets` are extra absolute positions (teleport destinations, mission starts) that get a
    ///   mandatory ring.
    pub fn build(
        &mut self,
        cfg: &StreamingConfig,
        motion: &MotionState,
        route: Option<&Route>,
        targets: &[DVec3],
    ) -> &[InterestCell] {
        self.map.clear();
        let grid = &cfg.grid;
        let pos = motion.position;
        let speed = motion.speed();
        let heading = if speed > 0.5 {
            Some(DVec3::new(motion.velocity.x, 0.0, motion.velocity.z) / speed)
        } else {
            None
        };

        // 1. Wanted ring (static radius), heading-biased ordering.
        let map = &mut self.map;
        grid.for_each_cell_in_radius(pos, cfg.load_radius, |c, d| {
            let bias = heading.map_or(1.0, |h| {
                let to = grid.cell_center(c) - pos;
                let len = to.x.hypot(to.z);
                let cos = if len > 1e-6 {
                    (to.x * h.x + to.z * h.z) / len
                } else {
                    1.0
                };
                1.0 + cfg.behind_penalty * (-cos).max(0.0)
            });
            insert_min(
                map,
                InterestCell {
                    coord: c,
                    tier: InterestTier::Wanted,
                    time_to_need: f32::INFINITY,
                    sort_distance: (d * bias) as f32,
                },
            );
        });

        // 2. Mandatory ring around the focus and targets.
        grid.for_each_cell_in_radius(pos, cfg.mandatory_radius, |c, d| {
            insert_min(
                map,
                InterestCell {
                    coord: c,
                    tier: InterestTier::Mandatory,
                    time_to_need: 0.0,
                    sort_distance: d as f32,
                },
            );
        });
        for &t in targets {
            let d0 = (t - pos).length();
            grid.for_each_cell_in_radius(t, cfg.mandatory_radius, |c, d| {
                insert_min(
                    map,
                    InterestCell {
                        coord: c,
                        tier: InterestTier::Mandatory,
                        time_to_need: 0.0,
                        sort_distance: (d0 + d) as f32,
                    },
                );
            });
        }

        // 3. Speculative: kinematic prediction.
        if cfg.prediction_enabled && speed > cfg.min_prediction_speed {
            let mut t = cfg.prediction_step_s;
            while t <= cfg.prediction_horizon_s + 1e-9 {
                let p = motion.predict(t, cfg.max_prediction_accel);
                grid.for_each_cell_in_radius(p, cfg.prediction_radius, |c, d| {
                    insert_min(
                        map,
                        InterestCell {
                            coord: c,
                            tier: InterestTier::Speculative,
                            time_to_need: t as f32,
                            sort_distance: d as f32,
                        },
                    );
                });
                t += cfg.prediction_step_s;
            }
        }

        // 4. Speculative: route corridor.
        if cfg.route_enabled
            && speed > cfg.min_prediction_speed
            && let Some(route) = route
        {
            let proj = route.project(pos);
            if proj.lateral <= cfg.route_max_lateral {
                let length = speed * cfg.prediction_horizon_s.max(cfg.route_horizon_s);
                let step = (grid.cell_size * 0.5).max(1.0);
                let mut ds = 0.0;
                while ds <= length {
                    let p = route.point_at(proj.s + ds);
                    let t = (ds / speed) as f32;
                    grid.for_each_cell_in_radius(p, cfg.corridor_half_width, |c, d| {
                        insert_min(
                            map,
                            InterestCell {
                                coord: c,
                                tier: InterestTier::Speculative,
                                time_to_need: t,
                                sort_distance: d as f32,
                            },
                        );
                    });
                    ds += step;
                }
            }
        }

        self.out.clear();
        self.out.extend(self.map.values().copied());
        self.out.sort_by(|a, b| {
            a.tier
                .cmp(&b.tier)
                .then_with(|| {
                    if a.tier == InterestTier::Speculative {
                        a.time_to_need.total_cmp(&b.time_to_need)
                    } else {
                        core::cmp::Ordering::Equal
                    }
                })
                .then_with(|| a.sort_distance.total_cmp(&b.sort_distance))
                .then_with(|| a.coord.cmp(&b.coord))
        });
        &self.out
    }
}

/// Keeps the more urgent of the existing/new entry (lower tier, then lower time-to-need, then
/// lower sort distance).
fn insert_min(map: &mut HashMap<CellCoord, InterestCell>, new: InterestCell) {
    map.entry(new.coord)
        .and_modify(|old| {
            let better = new
                .tier
                .cmp(&old.tier)
                .then_with(|| new.time_to_need.total_cmp(&old.time_to_need))
                .then_with(|| new.sort_distance.total_cmp(&old.sort_distance))
                .is_lt();
            if better {
                // Preserve the best (lowest) heading-biased distance across tiers.
                let sd = new.sort_distance.min(old.sort_distance);
                *old = new;
                old.sort_distance = sd;
            } else {
                old.sort_distance = old.sort_distance.min(new.sort_distance);
            }
        })
        .or_insert(new);
}

/// Total lookahead distance (meters) a focus moving at `speed` (m/s) needs, given the pipeline
/// latency budget: `v · (t_io + t_decode + t_upload + t_margin)`.
#[inline]
pub fn required_lookahead(speed: f64, latency_s: f64, margin_s: f64) -> f64 {
    speed * (latency_s + margin_s)
}

/// Converts km/h to m/s.
#[inline]
pub const fn kmh_to_ms(kmh: f64) -> f64 {
    kmh / 3.6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_is_kinematic_and_clamps_accel() {
        let m = MotionState {
            position: DVec3::ZERO,
            velocity: DVec3::new(10.0, 0.0, 0.0),
            acceleration: DVec3::new(2.0, 0.0, 0.0),
        };
        let p = m.predict(3.0, 100.0);
        assert!((p.x - (30.0 + 9.0)).abs() < 1e-9);
        let wild = MotionState {
            acceleration: DVec3::new(1e6, 0.0, 0.0),
            ..m
        };
        let p = wild.predict(2.0, 10.0);
        assert!((p.x - (20.0 + 20.0)).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn predictor_converges_to_constant_velocity() {
        let mut mp = MotionPredictor::new(0.2);
        let dt = 1.0 / 60.0;
        let v = kmh_to_ms(200.0);
        let mut pos = DVec3::new(100.0, 0.0, 0.0);
        let mut s = MotionState::default();
        for _ in 0..240 {
            pos.x += v * dt;
            s = mp.push(pos, dt);
        }
        assert!((s.speed() - v).abs() < 0.05 * v, "{} vs {v}", s.speed());
        assert!(s.acceleration.length() < 1.0);
    }

    #[test]
    fn predictor_is_deterministic() {
        let run = || {
            let mut mp = MotionPredictor::default();
            let mut s = MotionState::default();
            for i in 0..100 {
                s = mp.push(
                    DVec3::new(f64::from(i) * 0.7, 0.0, f64::from(i * i) * 0.01),
                    1.0 / 60.0,
                );
            }
            s
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn route_projection_and_point_at() {
        let r = Route::new(vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(100.0, 0.0, 0.0),
            DVec3::new(100.0, 0.0, 100.0),
        ])
        .unwrap();
        assert!((r.length() - 200.0).abs() < 1e-9);
        let p = r.project(DVec3::new(40.0, 5.0, 7.0));
        assert!((p.s - 40.0).abs() < 1e-9 && (p.lateral - 7.0).abs() < 1e-9);
        let p = r.project(DVec3::new(107.0, 0.0, 30.0));
        assert!((p.s - 130.0).abs() < 1e-9 && (p.lateral - 7.0).abs() < 1e-9);
        assert_eq!(r.point_at(150.0), DVec3::new(100.0, 0.0, 50.0));
        assert_eq!(r.point_at(1e9), DVec3::new(100.0, 0.0, 100.0));
        let near = r.project_near(DVec3::new(107.0, 0.0, 30.0), 120.0, 50.0);
        assert!((near.s - 130.0).abs() < 1e-9);
        assert!(Route::new(vec![DVec3::ZERO]).is_none());
    }

    fn cfg() -> StreamingConfig {
        StreamingConfig::default()
    }

    #[test]
    fn interest_always_contains_mandatory_ring_even_with_prediction() {
        let c = cfg();
        let mut b = InterestBuilder::default();
        let motion = MotionState {
            position: DVec3::new(1000.0, 0.0, 1000.0),
            velocity: DVec3::new(kmh_to_ms(250.0), 0.0, 0.0),
            acceleration: DVec3::ZERO,
        };
        let set = b.build(&c, &motion, None, &[]).to_vec();
        let mut expect = Vec::new();
        c.grid
            .for_each_cell_in_radius(motion.position, c.mandatory_radius, |cc, _| expect.push(cc));
        for cc in expect {
            let e = set
                .iter()
                .find(|i| i.coord == cc)
                .expect("mandatory cell missing");
            assert_eq!(e.tier, InterestTier::Mandatory);
        }
        // Sorted: tiers are non-decreasing.
        assert!(set.windows(2).all(|w| w[0].tier <= w[1].tier));
    }

    #[test]
    fn speculative_cells_extend_ahead_proportional_to_speed() {
        let mut c = cfg();
        c.load_radius = 256.0; // smaller than the lookahead at speed, so prediction is what extends reach
        let mut b = InterestBuilder::default();
        let slow = MotionState {
            position: DVec3::ZERO,
            velocity: DVec3::new(kmh_to_ms(30.0), 0.0, 0.0),
            acceleration: DVec3::ZERO,
        };
        let fast = MotionState {
            velocity: DVec3::new(kmh_to_ms(250.0), 0.0, 0.0),
            ..slow
        };
        let ahead = |set: &[InterestCell]| {
            set.iter()
                .map(|i| c.grid.cell_center(i.coord).x)
                .fold(f64::MIN, f64::max)
        };
        let a_slow = ahead(b.build(&c, &slow, None, &[]));
        let a_fast = ahead(b.build(&c, &fast, None, &[]));
        assert!(a_fast > a_slow + 100.0, "{a_fast} vs {a_slow}");
    }

    #[test]
    fn route_corridor_follows_a_turn_the_kinematic_cone_misses() {
        let mut c = cfg();
        c.prediction_enabled = false; // isolate the route term
        c.load_radius = 200.0;
        c.mandatory_radius = 100.0;
        let route = Route::new(vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(400.0, 0.0, 0.0),
            DVec3::new(400.0, 0.0, 800.0),
        ])
        .unwrap();
        let motion = MotionState {
            position: DVec3::new(300.0, 0.0, 0.0),
            velocity: DVec3::new(kmh_to_ms(250.0), 0.0, 0.0),
            acceleration: DVec3::ZERO,
        };
        let mut b = InterestBuilder::default();
        let with = b.build(&c, &motion, Some(&route), &[]).to_vec();
        let around_turn = c.grid.cell_of(DVec3::new(400.0, 0.0, 300.0));
        let hit = with
            .iter()
            .find(|i| i.coord == around_turn)
            .expect("corridor cell");
        assert_eq!(hit.tier, InterestTier::Speculative);
        let without = b.build(&c, &motion, None, &[]).to_vec();
        assert!(without.iter().all(|i| i.coord != around_turn));
    }

    #[test]
    fn behind_cells_sort_after_ahead_cells_within_wanted_tier() {
        let c = cfg();
        let mut b = InterestBuilder::default();
        let motion = MotionState {
            position: DVec3::new(64.0, 0.0, 64.0),
            velocity: DVec3::new(kmh_to_ms(200.0), 0.0, 0.0),
            acceleration: DVec3::ZERO,
        };
        let set = b.build(&c, &motion, None, &[]);
        let idx = |cc: CellCoord| set.iter().position(|i| i.coord == cc).unwrap();
        let ahead = c.grid.cell_of(DVec3::new(64.0 + 300.0, 0.0, 64.0));
        let behind = c.grid.cell_of(DVec3::new(64.0 - 300.0, 0.0, 64.0));
        assert!(idx(ahead) < idx(behind));
    }
}
