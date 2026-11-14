//! Geometry **page** residency: streaming geometry at a finer granularity than a whole mesh.
//!
//! An asset (a tree species, a building, a rock) is stored as `metadata + pages 0..N`, ordered
//! **coarse → fine**. Page `0` (the *root page*) contains the coarsest usable representation, so any
//! prefix `0..=k` is a valid, progressively more detailed model. Distant instances only need a short
//! prefix, hence *VRAM scales with visible screen-space detail, not with instance count*.
//!
//! [`PageTable`] decides which pages are resident under a byte budget. It is the **policy and
//! accounting layer**: it emits [`PageDelta`]s that a renderer integration applies to real GPU
//! buffers (for example via a `PersistentGpuBuffer`-style range allocator).
//!
//! Status: Bevy's meshlet renderer currently uploads a whole `MeshletMesh` at a time, so wiring
//! page residency into meshlet BVH traversal is future upstream work (see
//! `docs/open_world/bevy_audit.md`).

use bevy_platform::collections::HashMap;

/// Identifier of a pageable asset (species, building kit piece, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetKey(pub u32);

/// Description of a single page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageDesc {
    /// GPU bytes occupied by the page.
    pub bytes: u32,
    /// World-space geometric error (meters) of the model when pages `0..=this` are resident.
    /// Must be non-increasing along the page list.
    pub error: f32,
}

/// A pageable asset: pages ordered coarse → fine.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetPages {
    /// Asset id.
    pub key: AssetKey,
    /// Pages, coarse → fine. Never empty.
    pub pages: Vec<PageDesc>,
}

impl AssetPages {
    /// Builds an asset from explicit pages.
    ///
    /// Returns `None` if `pages` is empty or errors increase along the list.
    pub fn new(key: AssetKey, pages: Vec<PageDesc>) -> Option<Self> {
        if pages.is_empty() || pages.windows(2).any(|w| w[1].error > w[0].error) {
            return None;
        }
        Some(Self { key, pages })
    }

    /// Synthesizes an asset from a triangle-count LOD chain (coarse → fine) for benchmarks.
    ///
    /// Geometric error uses `error ≈ 1.5 · radius / √triangles`, a standard rule of thumb for
    /// uniformly tessellated surfaces. Real assets should bake measured errors.
    pub fn from_lod_chain(
        key: AssetKey,
        radius: f32,
        triangles_coarse_to_fine: &[u32],
        bytes_per_triangle: u32,
    ) -> Option<Self> {
        let mut prev = 0u32;
        let pages = triangles_coarse_to_fine
            .iter()
            .map(|&t| {
                let added = t.saturating_sub(prev).max(1);
                prev = t;
                PageDesc {
                    bytes: added * bytes_per_triangle,
                    error: 1.5 * radius / (t.max(1) as f32).sqrt(),
                }
            })
            .collect();
        Self::new(key, pages)
    }

    /// Total bytes of all pages.
    pub fn total_bytes(&self) -> u64 {
        self.pages.iter().map(|p| u64::from(p.bytes)).sum()
    }

    /// Bytes of the prefix of `count` pages.
    pub fn prefix_bytes(&self, count: usize) -> u64 {
        self.pages[..count.min(self.pages.len())]
            .iter()
            .map(|p| u64::from(p.bytes))
            .sum()
    }

    /// Number of pages (≥ 1) needed so the projected error at `distance` meters is at most
    /// `threshold_px`, for a view with `focal_px` (focal length in pixels).
    ///
    /// `projected_px = error · focal_px / distance`. If even the finest page is not enough, all
    /// pages are requested.
    pub fn required_pages(&self, distance: f32, focal_px: f32, threshold_px: f32) -> usize {
        let d = distance.max(1e-3);
        for (i, p) in self.pages.iter().enumerate() {
            if p.error * focal_px / d <= threshold_px {
                return i + 1;
            }
        }
        self.pages.len()
    }
}

/// A demand for an asset by one or more instances this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageDemand {
    /// Asset.
    pub key: AssetKey,
    /// Pages wanted (prefix length, ≥ 1) — typically the max over all visible instances.
    pub wanted_pages: usize,
    /// Distance of the nearest instance in meters (priority: nearer first).
    pub nearest_distance: f32,
}

/// Change in residency produced by [`PageTable::reconcile`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageDelta {
    /// Pages to upload `(asset, page index)`, in priority order.
    pub load: Vec<(AssetKey, usize)>,
    /// Pages to free `(asset, page index)`.
    pub evict: Vec<(AssetKey, usize)>,
    /// Demanded assets whose root page could not fit in the budget (must be 0 in a healthy setup).
    pub root_overflow: u32,
}

/// Lazily built eviction candidates, shared by all `trim` calls of one `reconcile`.
struct TrimCtx {
    demanded: HashMap<AssetKey, usize>,
    idle: Option<Vec<AssetKey>>,
    idle_pos: usize,
    excess: Option<Vec<AssetKey>>,
    excess_pos: usize,
}

impl TrimCtx {
    fn new(demanded: HashMap<AssetKey, usize>) -> Self {
        Self {
            demanded,
            idle: None,
            idle_pos: 0,
            excess: None,
            excess_pos: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct Resident {
    pages: usize,
    last_demand: u64,
}

/// Residency table for pageable assets under a byte budget.
#[derive(Debug)]
pub struct PageTable {
    budget: u64,
    used: u64,
    frame: u64,
    resident: HashMap<AssetKey, Resident>,
    catalog: HashMap<AssetKey, AssetPages>,
}

impl PageTable {
    /// Creates a table with a GPU byte budget.
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget: budget_bytes,
            used: 0,
            frame: 0,
            resident: HashMap::default(),
            catalog: HashMap::default(),
        }
    }

    /// Registers (or replaces) an asset's page layout. Replacing an asset with resident pages
    /// is not supported and panics in debug builds.
    pub fn register(&mut self, asset: AssetPages) {
        debug_assert!(
            !self.resident.contains_key(&asset.key),
            "re-registering a resident asset"
        );
        self.catalog.insert(asset.key, asset);
    }

    /// The registered layout of `key`.
    pub fn asset(&self, key: AssetKey) -> Option<&AssetPages> {
        self.catalog.get(&key)
    }

    /// Bytes currently resident.
    pub fn used_bytes(&self) -> u64 {
        self.used
    }

    /// Byte budget.
    pub fn budget_bytes(&self) -> u64 {
        self.budget
    }

    /// Number of resident pages (prefix length) for `key`.
    pub fn resident_pages(&self, key: AssetKey) -> usize {
        self.resident.get(&key).map_or(0, |r| r.pages)
    }

    /// Geometric error (meters) currently available for `key`, or `None` if nothing is resident.
    pub fn available_error(&self, key: AssetKey) -> Option<f32> {
        let n = self.resident_pages(key);
        (n > 0).then(|| self.catalog[&key].pages[n - 1].error)
    }

    /// Reconciles residency with this tick's demands. Page residency is always a *prefix*, so
    /// growth loads pages in order and trimming frees the finest pages first.
    ///
    /// Policy:
    /// 1. Demanded assets are grown nearest-first while the budget allows. Root pages (page 0) have
    ///    absolute priority over finer pages.
    /// 2. To make room, pages of **undemanded** assets are trimmed finest-first in
    ///    least-recently-demanded order, then excess pages of demanded assets (farthest first).
    pub fn reconcile(&mut self, demands: &[PageDemand]) -> PageDelta {
        self.frame += 1;
        let mut delta = PageDelta::default();
        let mut demanded: HashMap<AssetKey, usize> = HashMap::default();
        for d in demands {
            let Some(asset) = self.catalog.get(&d.key) else {
                continue;
            };
            let wanted = d.wanted_pages.clamp(1, asset.pages.len());
            let e = demanded.entry(d.key).or_insert(0);
            *e = (*e).max(wanted);
            self.resident
                .entry(d.key)
                .or_insert(Resident {
                    pages: 0,
                    last_demand: 0,
                })
                .last_demand = self.frame;
        }

        let mut ctx = TrimCtx::new(demanded.clone());

        // Growth order: roots first (nearest first), then finer pages (nearest first).
        let mut order: Vec<&PageDemand> = demands
            .iter()
            .filter(|d| self.catalog.contains_key(&d.key))
            .collect();
        order.sort_by(|a, b| {
            a.nearest_distance
                .total_cmp(&b.nearest_distance)
                .then_with(|| a.key.cmp(&b.key))
        });
        order.dedup_by_key(|d| d.key);

        // Pass 1: roots.
        for d in &order {
            if self.resident_pages(d.key) == 0 {
                let need = self.catalog[&d.key].prefix_bytes(1);
                self.trim(need, None, &mut ctx, &mut delta);
                if self.used + need > self.budget {
                    delta.root_overflow += 1;
                    self.resident.remove(&d.key);
                    continue;
                }
                self.used += need;
                self.resident.get_mut(&d.key).expect("inserted").pages = 1;
                delta.load.push((d.key, 0));
            }
        }
        // Pass 2: finer pages, one level at a time across assets so nearer assets are served
        // first at every level.
        let max_level = ctx.demanded.values().copied().max().unwrap_or(1);
        for level in 1..max_level {
            for d in &order {
                let wanted = ctx.demanded[&d.key];
                let have = self.resident_pages(d.key);
                if have == 0 || have != level || level >= wanted {
                    continue;
                }
                let need = u64::from(self.catalog[&d.key].pages[level].bytes);
                if self.used + need > self.budget {
                    self.trim(need, Some(d.key), &mut ctx, &mut delta);
                }
                if self.used + need > self.budget {
                    continue; // keep what we have: coarser but valid
                }
                self.used += need;
                self.resident.get_mut(&d.key).expect("present").pages += 1;
                delta.load.push((d.key, level));
            }
        }
        delta
    }

    /// Frees pages until `need` more bytes fit, using the lazily built victim list in `ctx`.
    fn trim(
        &mut self,
        need: u64,
        keep: Option<AssetKey>,
        ctx: &mut TrimCtx,
        delta: &mut PageDelta,
    ) {
        if self.used + need <= self.budget {
            return;
        }
        let demanded = &ctx.demanded;
        // Phase A: undemanded assets, least recently demanded first, finest page first.
        if ctx.idle.is_none() {
            let mut idle: Vec<(u64, AssetKey)> = self
                .resident
                .iter()
                .filter(|(k, r)| r.pages > 0 && !demanded.contains_key(*k))
                .map(|(k, r)| (r.last_demand, *k))
                .collect();
            idle.sort();
            ctx.idle = Some(idle.into_iter().map(|(_, k)| k).collect());
        }
        let idle = ctx.idle.as_ref().expect("built above");
        while ctx.idle_pos < idle.len() && self.used + need > self.budget {
            let key = idle[ctx.idle_pos];
            while self.used + need > self.budget && self.resident_pages(key) > 0 {
                self.evict_finest(key, delta);
            }
            if self.resident_pages(key) == 0 {
                ctx.idle_pos += 1;
            }
        }
        if self.used + need <= self.budget {
            return;
        }
        // Phase B: excess pages of demanded assets (resident > wanted), most excess first.
        if ctx.excess.is_none() {
            let mut excess: Vec<(usize, AssetKey)> = self
                .resident
                .iter()
                .filter_map(|(k, r)| {
                    let w = *demanded.get(k)?;
                    (r.pages > w).then(|| (r.pages - w, *k))
                })
                .collect();
            excess.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            ctx.excess = Some(excess.into_iter().map(|(_, k)| k).collect());
        }
        let excess = ctx.excess.as_ref().expect("built above");
        for &key in &excess[ctx.excess_pos..] {
            if Some(key) == keep {
                continue;
            }
            let w = demanded[&key];
            while self.used + need > self.budget && self.resident_pages(key) > w {
                self.evict_finest(key, delta);
            }
            if self.used + need <= self.budget {
                return;
            }
        }
        // Nothing left to trim for this request: remember so later calls are O(1).
        ctx.excess_pos = excess.len();
    }

    fn evict_finest(&mut self, key: AssetKey, delta: &mut PageDelta) {
        let r = self.resident.get_mut(&key).expect("resident");
        debug_assert!(r.pages > 0);
        let idx = r.pages - 1;
        r.pages -= 1;
        let bytes = u64::from(self.catalog[&key].pages[idx].bytes);
        self.used -= bytes;
        delta.evict.push((key, idx));
        if r.pages == 0 {
            self.resident.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(k: u32) -> AssetPages {
        // 4 pages: 500, 2k, 10k, 50k triangles; 32 B/tri.
        AssetPages::from_lod_chain(AssetKey(k), 6.0, &[500, 2_000, 10_000, 50_000], 32).unwrap()
    }

    #[test]
    fn rejects_bad_layouts() {
        assert!(AssetPages::new(AssetKey(0), vec![]).is_none());
        let up = vec![
            PageDesc {
                bytes: 1,
                error: 0.1,
            },
            PageDesc {
                bytes: 1,
                error: 0.5,
            },
        ];
        assert!(AssetPages::new(AssetKey(0), up).is_none());
    }

    #[test]
    fn required_pages_grows_as_distance_shrinks() {
        let a = tree(1);
        let focal = 1000.0;
        let far = a.required_pages(800.0, focal, 1.0);
        let mid = a.required_pages(100.0, focal, 1.0);
        let near = a.required_pages(8.0, focal, 1.0);
        assert!(far <= mid && mid <= near, "{far} {mid} {near}");
        assert_eq!(far, 1);
        assert_eq!(near, 4);
    }

    #[test]
    fn distant_trees_use_far_less_memory_than_near_trees() {
        let a = tree(1);
        let near = a.prefix_bytes(a.required_pages(8.0, 1000.0, 1.0));
        let far = a.prefix_bytes(a.required_pages(800.0, 1000.0, 1.0));
        assert!(near > 50 * far, "near {near} vs far {far}");
    }

    #[test]
    fn reconcile_loads_prefixes_in_order_and_respects_budget() {
        let mut t = PageTable::new(2_000_000);
        for k in 0..10 {
            t.register(tree(k));
        }
        let demands: Vec<_> = (0..10)
            .map(|k| PageDemand {
                key: AssetKey(k),
                wanted_pages: 4,
                nearest_distance: k as f32 * 10.0,
            })
            .collect();
        let d = t.reconcile(&demands);
        assert!(t.used_bytes() <= t.budget_bytes());
        assert_eq!(d.root_overflow, 0);
        // Prefix property: if page i of an asset is loaded, all earlier pages are too.
        for k in 0..10 {
            let n = t.resident_pages(AssetKey(k));
            let loaded: Vec<_> = d
                .load
                .iter()
                .filter(|(a, _)| a.0 == k)
                .map(|(_, p)| *p)
                .collect();
            assert_eq!(loaded, (0..n).collect::<Vec<_>>());
        }
        // Nearest asset gets at least as much detail as the farthest.
        assert!(t.resident_pages(AssetKey(0)) >= t.resident_pages(AssetKey(9)));
    }

    #[test]
    fn eviction_trims_idle_assets_finest_first_and_keeps_roots_for_demanded() {
        let a = tree(1);
        let b = tree(2);
        let full = a.total_bytes();
        let mut t = PageTable::new(full + a.prefix_bytes(1)); // room for a full + a root
        t.register(a);
        t.register(b);
        t.reconcile(&[PageDemand {
            key: AssetKey(1),
            wanted_pages: 4,
            nearest_distance: 1.0,
        }]);
        assert_eq!(t.resident_pages(AssetKey(1)), 4);
        // Now only asset 2 is demanded; asset 1 is idle and must be trimmed (finest first).
        let d = t.reconcile(&[PageDemand {
            key: AssetKey(2),
            wanted_pages: 4,
            nearest_distance: 1.0,
        }]);
        assert!(t.used_bytes() <= t.budget_bytes());
        assert!(t.resident_pages(AssetKey(2)) >= 1);
        let evicted_a: Vec<_> = d
            .evict
            .iter()
            .filter(|(k, _)| k.0 == 1)
            .map(|(_, p)| *p)
            .collect();
        assert!(
            evicted_a.windows(2).all(|w| w[0] > w[1]),
            "finest page evicted first: {evicted_a:?}"
        );
    }

    #[test]
    fn root_overflow_is_reported_when_even_roots_do_not_fit() {
        let mut t = PageTable::new(10);
        t.register(tree(1));
        let d = t.reconcile(&[PageDemand {
            key: AssetKey(1),
            wanted_pages: 1,
            nearest_distance: 1.0,
        }]);
        assert_eq!(d.root_overflow, 1);
        assert_eq!(t.used_bytes(), 0);
    }

    #[test]
    fn available_error_improves_as_pages_arrive() {
        let mut t = PageTable::new(10_000_000);
        t.register(tree(1));
        assert_eq!(t.available_error(AssetKey(1)), None);
        t.reconcile(&[PageDemand {
            key: AssetKey(1),
            wanted_pages: 1,
            nearest_distance: 1.0,
        }]);
        let e1 = t.available_error(AssetKey(1)).unwrap();
        t.reconcile(&[PageDemand {
            key: AssetKey(1),
            wanted_pages: 4,
            nearest_distance: 1.0,
        }]);
        let e4 = t.available_error(AssetKey(1)).unwrap();
        assert!(e4 < e1);
    }
}
