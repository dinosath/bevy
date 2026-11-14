//! Forest HLOD: aggregate many distant trees into one merged proxy per **forest region**.
//!
//! ```text
//! 10 000 trees → forest regions (grid, e.g. 128 m) → one merged canopy mesh per region (1 draw)
//!              → individual trees when the region's screen-space error exceeds the threshold
//! ```
//!
//! The merged mesh is made of ellipsoid *canopy blobs*. If a region has more trees than
//! `max_blobs_per_region`, trees are merged by a sub-grid so triangle count stays bounded
//! regardless of tree density. Generation is deterministic (stable ordering, no RNG).

use alloc::collections::BTreeMap;
use bevy_math::Vec3;

/// One tree instance (positions are in a consistent local frame, meters).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreeInstance {
    /// Base position of the trunk.
    pub position: Vec3,
    /// Species index.
    pub species: u16,
    /// Trunk height below the canopy, meters.
    pub trunk_height: f32,
    /// Canopy horizontal radius, meters.
    pub canopy_radius: f32,
    /// Canopy vertical extent, meters.
    pub canopy_height: f32,
}

impl TreeInstance {
    /// Canopy bounding sphere radius.
    pub fn bounding_radius(&self) -> f32 {
        let half_h = self.canopy_height * 0.5;
        (self.canopy_radius * self.canopy_radius + half_h * half_h).sqrt()
    }
}

/// Parameters for [`build_forest_hlod`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ForestHlodConfig {
    /// Side length of a region in meters (X/Z grid).
    pub region_size: f32,
    /// Upper bound on canopy blobs per region (bounds triangle count).
    pub max_blobs_per_region: usize,
    /// Longitudinal segments per blob (≥ 3).
    pub sectors: u32,
    /// Latitudinal rings per blob (≥ 2).
    pub stacks: u32,
}

impl Default for ForestHlodConfig {
    fn default() -> Self {
        Self {
            region_size: 128.0,
            max_blobs_per_region: 48,
            sectors: 8,
            stacks: 4,
        }
    }
}

/// Plain merged geometry (CPU side). Convert with [`MergedGeometry::into_mesh`] (feature `mesh`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergedGeometry {
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Vertex normals.
    pub normals: Vec<[f32; 3]>,
    /// UV coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle list indices.
    pub indices: Vec<u32>,
}

impl MergedGeometry {
    /// Triangle count.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Appends an ellipsoid.
    fn push_ellipsoid(&mut self, center: Vec3, radii: Vec3, sectors: u32, stacks: u32) {
        let base = self.positions.len() as u32;
        for i in 0..=stacks {
            let v = i as f32 / stacks as f32;
            let phi = core::f32::consts::PI * v; // 0 (top) → π (bottom)
            let (sp, cp) = bevy_math::ops::sin_cos(phi);
            for j in 0..=sectors {
                let u = j as f32 / sectors as f32;
                let theta = core::f32::consts::TAU * u;
                let (st, ct) = bevy_math::ops::sin_cos(theta);
                let n = Vec3::new(sp * ct, cp, sp * st);
                let p = center + n * radii;
                // Normal of an ellipsoid: gradient of the implicit function.
                let en = Vec3::new(n.x / radii.x, n.y / radii.y, n.z / radii.z).normalize_or_zero();
                self.positions.push(p.to_array());
                self.normals.push(en.to_array());
                self.uvs.push([u, v]);
            }
        }
        let row = sectors + 1;
        for i in 0..stacks {
            for j in 0..sectors {
                let a = base + i * row + j;
                let b = a + row;
                if i != 0 {
                    self.indices.extend_from_slice(&[a, b, a + 1]);
                }
                if i != stacks - 1 {
                    self.indices.extend_from_slice(&[a + 1, b, b + 1]);
                }
            }
        }
    }

    /// Converts to a Bevy [`Mesh`](bevy_mesh::Mesh).
    #[cfg(feature = "mesh")]
    pub fn into_mesh(self) -> bevy_mesh::Mesh {
        use bevy_asset::RenderAssetUsages;
        use bevy_mesh::{Indices, Mesh, PrimitiveTopology};
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// A forest region and its merged proxy.
#[derive(Clone, Debug, PartialEq)]
pub struct ForestRegion {
    /// Region grid coordinate `(x, z)`.
    pub key: (i32, i32),
    /// Trees aggregated into this region.
    pub tree_count: u32,
    /// Bounding box minimum.
    pub aabb_min: Vec3,
    /// Bounding box maximum.
    pub aabb_max: Vec3,
    /// Merged canopy geometry (one draw call).
    pub geometry: MergedGeometry,
}

impl ForestRegion {
    /// Bounding sphere `(center, radius)` of the region, used for screen-space-error selection.
    pub fn bounding_sphere(&self) -> (Vec3, f32) {
        let c = (self.aabb_min + self.aabb_max) * 0.5;
        (c, (self.aabb_max - c).length())
    }
}

/// Summary of an aggregation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HlodStats {
    /// Input trees.
    pub trees: usize,
    /// Output regions (= draw calls after aggregation).
    pub regions: usize,
    /// Draw calls if every tree were drawn separately.
    pub draw_calls_before: usize,
    /// Draw calls with forest HLOD.
    pub draw_calls_after: usize,
    /// Total triangles in all merged proxies.
    pub triangles_after: usize,
}

/// Aggregates `trees` into [`ForestRegion`]s. Deterministic: output is sorted by region key and
/// independent of input order within a region (blobs are built from index-sorted sub-cells).
pub fn build_forest_hlod(
    trees: &[TreeInstance],
    cfg: &ForestHlodConfig,
) -> (Vec<ForestRegion>, HlodStats) {
    let size = cfg.region_size.max(1e-3);
    let mut by_region: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for (i, t) in trees.iter().enumerate() {
        let key = (
            (t.position.x / size).floor() as i32,
            (t.position.z / size).floor() as i32,
        );
        by_region.entry(key).or_default().push(i);
    }

    let mut regions = Vec::with_capacity(by_region.len());
    let mut stats = HlodStats {
        trees: trees.len(),
        draw_calls_before: trees.len(),
        ..HlodStats::default()
    };
    for (key, idxs) in by_region {
        let region = build_region(trees, &idxs, key, cfg);
        stats.triangles_after += region.geometry.triangle_count();
        regions.push(region);
    }
    stats.regions = regions.len();
    stats.draw_calls_after = regions.len();
    (regions, stats)
}

fn build_region(
    trees: &[TreeInstance],
    idxs: &[usize],
    key: (i32, i32),
    cfg: &ForestHlodConfig,
) -> ForestRegion {
    let size = cfg.region_size.max(1e-3);
    let max_blobs = cfg.max_blobs_per_region.max(1);
    let n = ((idxs.len().min(max_blobs)) as f32).sqrt().ceil().max(1.0) as i32;
    // Sub-grid assignment within the region.
    let mut cells: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    let (ox, oz) = (key.0 as f32 * size, key.1 as f32 * size);
    for &i in idxs {
        let p = trees[i].position;
        let sx = (((p.x - ox) / size) * n as f32).floor() as i32;
        let sz = (((p.z - oz) / size) * n as f32).floor() as i32;
        cells
            .entry((sx.clamp(0, n - 1), sz.clamp(0, n - 1)))
            .or_default()
            .push(i);
    }

    let mut geometry = MergedGeometry::default();
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for (_, members) in cells {
        // Enclosing ellipsoid of the members' canopies.
        let mut centroid = Vec3::ZERO;
        let (mut bottom, mut top) = (f32::INFINITY, f32::NEG_INFINITY);
        for &i in &members {
            let t = &trees[i];
            centroid += t.position;
            bottom = bottom.min(t.position.y + t.trunk_height);
            top = top.max(t.position.y + t.trunk_height + t.canopy_height);
        }
        centroid /= members.len() as f32;
        let mut rxz = 0.0f32;
        for &i in &members {
            let t = &trees[i];
            let d = Vec3::new(t.position.x - centroid.x, 0.0, t.position.z - centroid.z).length();
            rxz = rxz.max(d + t.canopy_radius);
        }
        let center = Vec3::new(centroid.x, (bottom + top) * 0.5, centroid.z);
        let radii = Vec3::new(rxz.max(0.1), ((top - bottom) * 0.5).max(0.1), rxz.max(0.1));
        geometry.push_ellipsoid(center, radii, cfg.sectors.max(3), cfg.stacks.max(2));
        lo = lo.min(center - radii);
        hi = hi.max(center + radii);
    }

    ForestRegion {
        key,
        tree_count: idxs.len() as u32,
        aabb_min: lo,
        aabb_max: hi,
        geometry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forest(n: usize, extent: f32) -> Vec<TreeInstance> {
        // Deterministic low-discrepancy scatter (no RNG dependency).
        (0..n)
            .map(|i| {
                let a = (i as f32 * 0.618_034).fract();
                let b = (i as f32 * 0.754_877_7).fract();
                TreeInstance {
                    position: Vec3::new(a * extent, 0.0, b * extent),
                    species: (i % 4) as u16,
                    trunk_height: 3.0,
                    canopy_radius: 3.0 + (i % 3) as f32,
                    canopy_height: 8.0,
                }
            })
            .collect()
    }

    #[test]
    fn ten_thousand_trees_collapse_to_a_handful_of_draws() {
        let trees = forest(10_000, 1024.0);
        let (regions, stats) = build_forest_hlod(&trees, &ForestHlodConfig::default());
        assert_eq!(stats.trees, 10_000);
        assert_eq!(stats.draw_calls_before, 10_000);
        assert_eq!(stats.draw_calls_after, regions.len());
        assert!(regions.len() <= 8 * 8, "{} regions", regions.len());
        assert_eq!(
            regions.iter().map(|r| r.tree_count as usize).sum::<usize>(),
            10_000
        );
        // Triangles are bounded by blobs/region regardless of density.
        let cfg = ForestHlodConfig::default();
        let per_blob = (cfg.sectors * (cfg.stacks * 2 - 2)) as usize;
        let max_blobs = cfg.max_blobs_per_region.next_power_of_two() * 2; // sqrt-grid rounding slack
        for r in &regions {
            assert!(
                r.geometry.triangle_count() <= max_blobs * per_blob,
                "{}",
                r.geometry.triangle_count()
            );
        }
    }

    #[test]
    fn output_is_deterministic_and_input_order_independent() {
        let trees = forest(2_000, 512.0);
        let (a, _) = build_forest_hlod(&trees, &ForestHlodConfig::default());
        let (b, _) = build_forest_hlod(&trees, &ForestHlodConfig::default());
        assert_eq!(a, b);
        let mut rev = trees.clone();
        rev.reverse();
        let (c, _) = build_forest_hlod(&rev, &ForestHlodConfig::default());
        // Same regions and tree counts and the same bounding boxes.
        assert_eq!(a.len(), c.len());
        for (x, y) in a.iter().zip(&c) {
            assert_eq!(x.key, y.key);
            assert_eq!(x.tree_count, y.tree_count);
            assert!((x.aabb_min - y.aabb_min).length() < 1e-3);
            assert!((x.aabb_max - y.aabb_max).length() < 1e-3);
        }
    }

    #[test]
    fn proxy_encloses_every_tree_canopy_center() {
        let trees = forest(500, 256.0);
        let (regions, _) = build_forest_hlod(&trees, &ForestHlodConfig::default());
        for t in &trees {
            let key = (
                (t.position.x / 128.0).floor() as i32,
                (t.position.z / 128.0).floor() as i32,
            );
            let r = regions.iter().find(|r| r.key == key).unwrap();
            let c = t.position + Vec3::Y * (t.trunk_height + t.canopy_height * 0.5);
            assert!(c.x >= r.aabb_min.x - 1e-3 && c.x <= r.aabb_max.x + 1e-3);
            assert!(c.y >= r.aabb_min.y - 1e-3 && c.y <= r.aabb_max.y + 1e-3);
            assert!(c.z >= r.aabb_min.z - 1e-3 && c.z <= r.aabb_max.z + 1e-3);
        }
    }

    #[test]
    fn geometry_is_well_formed() {
        let (regions, _) = build_forest_hlod(&forest(300, 200.0), &ForestHlodConfig::default());
        for r in regions {
            let g = &r.geometry;
            assert_eq!(g.positions.len(), g.normals.len());
            assert_eq!(g.positions.len(), g.uvs.len());
            assert!(g.indices.len() % 3 == 0 && !g.indices.is_empty());
            assert!(g.indices.iter().all(|&i| (i as usize) < g.positions.len()));
            assert!(g.normals.iter().all(|n| {
                let l = Vec3::from_array(*n).length();
                (l - 1.0).abs() < 1e-3 || l == 0.0
            }));
        }
    }

    #[cfg(feature = "mesh")]
    #[test]
    fn converts_to_a_bevy_mesh() {
        let (mut regions, _) = build_forest_hlod(&forest(50, 100.0), &ForestHlodConfig::default());
        let tris = regions[0].geometry.triangle_count();
        let mesh = regions.remove(0).geometry.into_mesh();
        assert!(mesh.count_vertices() > 0);
        assert_eq!(mesh.indices().map(bevy_mesh::Indices::len), Some(tris * 3));
    }
}
