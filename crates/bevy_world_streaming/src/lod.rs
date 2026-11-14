//! Representation (LOD/HLOD) selection by projected screen-space size, with hysteresis and
//! graceful fallback to whatever is resident.
//!
//! Rendering never *requires* the best data: [`resolve_fallback`] returns the finest representation
//! that is both desired and resident, so streaming latency shows up as a coarser object — never as a
//! hole. Representation chain (coarse → fine):
//!
//! `Culled < ForestHlod < Impostor < Proxy < Medium < Full`

/// Which representation of an object/region is drawn. Ordered coarse → fine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Representation {
    /// Not drawn (too small / out of range).
    Culled = 0,
    /// One merged proxy for a whole region (forest HLOD).
    ForestHlod = 1,
    /// Camera-facing billboard/octahedral impostor.
    Impostor = 2,
    /// Very low-poly proxy mesh.
    Proxy = 3,
    /// Reduced-detail mesh.
    Medium = 4,
    /// Full detail (high-poly, meshlets or LOD0).
    Full = 5,
}

impl Representation {
    /// Number of variants.
    pub const COUNT: usize = 6;

    /// All variants, coarse → fine.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Culled,
        Self::ForestHlod,
        Self::Impostor,
        Self::Proxy,
        Self::Medium,
        Self::Full,
    ];

    fn from_u8(v: u8) -> Self {
        Self::ALL[usize::from(v).min(Self::COUNT - 1)]
    }
}

/// Bit set of resident representations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidencySet(u8);

impl ResidencySet {
    /// Nothing resident.
    pub const EMPTY: Self = Self(0);

    /// Adds a representation.
    #[must_use]
    pub const fn with(self, r: Representation) -> Self {
        Self(self.0 | (1 << r as u8))
    }

    /// `true` if `r` is resident.
    pub const fn contains(self, r: Representation) -> bool {
        self.0 & (1 << r as u8) != 0
    }

    /// Everything from `Impostor` up to and including `finest` is resident (a typical
    /// always-resident-coarse + streamed-fine layout).
    pub fn up_to(finest: Representation) -> Self {
        Representation::ALL
            .iter()
            .filter(|r| **r != Representation::Culled && **r <= finest)
            .fold(Self::EMPTY, |s, r| s.with(*r))
    }
}

/// Minimum projected *object diameter in pixels* for each representation to be chosen.
///
/// Larger on-screen size ⇒ finer representation. All values are in pixels and must be
/// non-increasing from `full_px` to `cull_px`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodThresholds {
    /// At or above: `Full`.
    pub full_px: f32,
    /// At or above: `Medium`.
    pub medium_px: f32,
    /// At or above: `Proxy`.
    pub proxy_px: f32,
    /// At or above: `Impostor`.
    pub impostor_px: f32,
    /// At or above: `ForestHlod` (for objects that belong to a region). Below: `Culled`.
    pub hlod_px: f32,
}

impl Default for LodThresholds {
    fn default() -> Self {
        Self {
            full_px: 220.0,
            medium_px: 90.0,
            proxy_px: 35.0,
            impostor_px: 10.0,
            hlod_px: 2.0,
        }
    }
}

impl LodThresholds {
    fn level_px(&self, r: Representation) -> f32 {
        match r {
            Representation::Full => self.full_px,
            Representation::Medium => self.medium_px,
            Representation::Proxy => self.proxy_px,
            Representation::Impostor => self.impostor_px,
            Representation::ForestHlod => self.hlod_px,
            Representation::Culled => 0.0,
        }
    }

    /// Representation chosen for a projected diameter, without hysteresis.
    pub fn desired(&self, size_px: f32) -> Representation {
        if size_px >= self.full_px {
            Representation::Full
        } else if size_px >= self.medium_px {
            Representation::Medium
        } else if size_px >= self.proxy_px {
            Representation::Proxy
        } else if size_px >= self.impostor_px {
            Representation::Impostor
        } else if size_px >= self.hlod_px {
            Representation::ForestHlod
        } else {
            Representation::Culled
        }
    }
}

/// Projected diameter in pixels of a sphere of `radius` meters at `distance` meters.
///
/// `focal_px` is the focal length in pixels: `viewport_height_px / (2·tan(fov_y/2))`.
#[inline]
pub fn projected_size_px(radius: f32, distance: f32, focal_px: f32) -> f32 {
    2.0 * radius * focal_px / distance.max(1e-3)
}

/// Projected size in pixels of a world-space geometric error (meters) at `distance`.
#[inline]
pub fn projected_error_px(world_error: f32, distance: f32, focal_px: f32) -> f32 {
    world_error * focal_px / distance.max(1e-3)
}

/// Focal length in pixels for a vertical field of view (radians) and viewport height.
#[inline]
pub fn focal_length_px(vertical_fov: f32, viewport_height_px: f32) -> f32 {
    viewport_height_px / (2.0 * bevy_math::ops::tan(vertical_fov * 0.5))
}

/// Stateless selector with hysteresis: to move to a *finer* level the size must exceed the
/// threshold by `(1 + hysteresis)`, to move to a *coarser* level it must drop below the current
/// level's threshold by `(1 − hysteresis)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodSelector {
    /// Thresholds in pixels.
    pub thresholds: LodThresholds,
    /// Fractional hysteresis band (e.g. `0.15`).
    pub hysteresis: f32,
}

impl Default for LodSelector {
    fn default() -> Self {
        Self {
            thresholds: LodThresholds::default(),
            hysteresis: 0.15,
        }
    }
}

impl LodSelector {
    /// Chooses the representation given the previous one (if any) and the projected size.
    pub fn select(&self, current: Option<Representation>, size_px: f32) -> Representation {
        let Some(cur) = current else {
            return self.thresholds.desired(size_px);
        };
        let raw = self.thresholds.desired(size_px);
        match raw.cmp(&cur) {
            core::cmp::Ordering::Equal => cur,
            core::cmp::Ordering::Greater => {
                // Finer wanted: require clearing the *next finer* threshold by the margin; step as
                // far up as the margin allows.
                let mut level = cur;
                while level < raw {
                    let next = Representation::from_u8(level as u8 + 1);
                    if size_px >= self.thresholds.level_px(next) * (1.0 + self.hysteresis) {
                        level = next;
                    } else {
                        break;
                    }
                }
                level
            }
            core::cmp::Ordering::Less => {
                let mut level = cur;
                while level > raw {
                    if size_px < self.thresholds.level_px(level) * (1.0 - self.hysteresis) {
                        level = Representation::from_u8(level as u8 - 1);
                    } else {
                        break;
                    }
                }
                level
            }
        }
    }
}

/// Outcome of resolving a desired representation against what is resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// What will actually be drawn.
    pub drawn: Representation,
    /// `true` if `drawn` is coarser than desired (streaming has not caught up).
    pub fallback: bool,
    /// `true` if something should be drawn but **nothing** resident can be (a *hard miss*).
    pub hard_miss: bool,
}

/// Picks the finest resident representation not finer than `desired`.
pub fn resolve_fallback(desired: Representation, resident: ResidencySet) -> Resolved {
    if desired == Representation::Culled {
        return Resolved {
            drawn: Representation::Culled,
            fallback: false,
            hard_miss: false,
        };
    }
    for r in Representation::ALL.iter().rev() {
        if *r <= desired && *r != Representation::Culled && resident.contains(*r) {
            return Resolved {
                drawn: *r,
                fallback: *r < desired,
                hard_miss: false,
            };
        }
    }
    Resolved {
        drawn: Representation::Culled,
        fallback: true,
        hard_miss: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_is_monotonic_in_size() {
        let t = LodThresholds::default();
        let mut prev = Representation::Culled;
        let mut s = 0.0;
        while s < 600.0 {
            let r = t.desired(s);
            assert!(r >= prev);
            prev = r;
            s += 0.5;
        }
        assert_eq!(t.desired(1000.0), Representation::Full);
        assert_eq!(t.desired(0.1), Representation::Culled);
    }

    #[test]
    fn hysteresis_prevents_flicker_at_a_threshold() {
        let sel = LodSelector::default();
        let th = sel.thresholds.medium_px;
        let mut cur = Some(sel.thresholds.desired(th * 1.5)); // Medium
        let mut switches = 0;
        // Jitter around the Medium/Proxy threshold within the hysteresis band.
        for i in 0..1000 {
            let jitter = if i % 2 == 0 { 1.05 } else { 0.95 };
            let next = sel.select(cur, th * jitter);
            if Some(next) != cur {
                switches += 1;
            }
            cur = Some(next);
        }
        assert!(switches <= 1, "flickered {switches} times");
    }

    #[test]
    fn selector_moves_when_clearly_beyond_the_band() {
        let sel = LodSelector::default();
        let far = sel.select(Some(Representation::Full), 5.0);
        assert!(far < Representation::Full);
        let near = sel.select(Some(Representation::Impostor), 500.0);
        assert_eq!(near, Representation::Full);
    }

    #[test]
    fn fallback_never_draws_finer_than_resident_and_flags_hard_misses() {
        let res = ResidencySet::up_to(Representation::Proxy);
        let r = resolve_fallback(Representation::Full, res);
        assert_eq!(r.drawn, Representation::Proxy);
        assert!(r.fallback && !r.hard_miss);
        let r = resolve_fallback(Representation::Impostor, res);
        assert_eq!(r.drawn, Representation::Impostor);
        assert!(!r.fallback);
        let none = resolve_fallback(Representation::Medium, ResidencySet::EMPTY);
        assert!(none.hard_miss);
        assert!(!resolve_fallback(Representation::Culled, ResidencySet::EMPTY).hard_miss);
        // Always-resident coarse chain => no hard miss for anything drawable.
        let coarse = ResidencySet::EMPTY
            .with(Representation::ForestHlod)
            .with(Representation::Impostor);
        for d in Representation::ALL {
            assert!(!resolve_fallback(d, coarse).hard_miss);
        }
    }

    #[test]
    fn projection_helpers() {
        let f = focal_length_px(core::f32::consts::FRAC_PI_2, 1080.0);
        assert!((f - 540.0).abs() < 0.5);
        // 10 m radius at 100 m with 540 px focal: 2*10*540/100 = 108 px.
        assert!((projected_size_px(10.0, 100.0, 540.0) - 108.0).abs() < 1e-3);
        assert!((projected_error_px(0.1, 10.0, 540.0) - 5.4).abs() < 1e-3);
    }
}
