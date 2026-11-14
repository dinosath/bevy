use super::{
    meshlet_mesh_manager::MeshletMeshManager, retained::RetainedBuffer, MeshletMesh, MeshletMesh3d,
    MeshletSettings,
};
use crate::{
    meshlet::asset::MeshletAabb, MeshFlags, MeshTransforms, MeshUniform, PreviousGlobalTransform,
    RenderMaterialInstances,
};
use alloc::collections::BTreeMap;
use bevy_asset::{AssetEvent, AssetId, AssetServer, Assets, UntypedAssetId};
use bevy_camera::visibility::RenderLayers;
use bevy_ecs::{
    entity::{Entities, Entity, EntityHashMap},
    lifecycle::RemovedComponents,
    message::MessageReader,
    query::{Added, Changed, Has, Or},
    resource::Resource,
    system::{Local, Query, Res, ResMut, SystemState},
};
use bevy_light::{NotShadowCaster, NotShadowReceiver};
use bevy_platform::collections::{HashMap, HashSet};
use bevy_render::{
    material_bind_groups::{MaterialBindingId, RenderMaterialBindings},
    render_resource::StorageBuffer,
    sync_world::MainEntity,
    MainWorld,
};
use bevy_transform::components::GlobalTransform;
use core::ops::DerefMut;
use tracing::error;

/// Render-world list of entities whose material registration changed this frame
/// (filled by `extract_mesh_materials`), so retained meshlet rows can re-resolve their material.
#[derive(Resource, Default)]
pub struct MeshletMaterialDirty(pub Vec<MainEntity>);

/// Per-row bookkeeping for the retained instance table.
struct RowMeta {
    entity: MainEntity,
    bvh_depth: u32,
    material_asset: Option<UntypedAssetId>,
    material_id: u32,
    material_attempts: u8,
}

/// Everything extracted for one instance.
struct RowData {
    uniform: MeshUniform,
    aabb: MeshletAabb,
    root_bvh_node: u32,
    layers: RenderLayers,
    not_shadow_caster: bool,
    bvh_depth: u32,
}

/// A per-view-signature visibility bit vector maintained incrementally.
struct VisibilityMask {
    buffer: RetainedBuffer<u32>,
    /// Needs a full rebuild (new, or missed dirty rows while unused).
    stale: bool,
    last_used_frame: u64,
}

/// Manages data for each entity with a [`MeshletMesh`].
///
/// Two modes (see [`MeshletSettings::retained_instances`]):
/// * **rebuild** (original): every frame all rows are cleared and re-added from every entity;
/// * **retained**: rows live in stable slots, only changed entities are touched, and only dirty rows
///   are uploaded to the GPU.
#[derive(Resource)]
pub struct InstanceManager {
    /// Amount of instances in the scene.
    pub scene_instance_count: u32,
    /// The max BVH depth of any instance in the scene. This is used to control the number of
    /// dependent dispatches emitted for BVH traversal.
    pub max_bvh_depth: u32,

    /// Per-instance [`MainEntity`], [`RenderLayers`], and [`NotShadowCaster`].
    pub instances: Vec<(MainEntity, RenderLayers, bool)>,
    /// Per-instance [`MeshUniform`].
    pub instance_uniforms: RetainedBuffer<MeshUniform>,
    /// Per-instance model-space AABB.
    pub instance_aabbs: RetainedBuffer<MeshletAabb>,
    /// Per-instance material ID.
    pub instance_material_ids: RetainedBuffer<u32>,
    /// Per-instance index to the root node of the instance's BVH.
    pub instance_bvh_root_nodes: RetainedBuffer<u32>,
    /// (rebuild mode) Per-view per-instance visibility bit. Used for [`RenderLayers`] and
    /// [`NotShadowCaster`] support.
    pub view_instance_visibility: EntityHashMap<StorageBuffer<Vec<u32>>>,

    /// Next material ID available.
    next_material_id: u32,
    /// Map of material asset to material ID.
    material_id_lookup: HashMap<UntypedAssetId, u32>,
    /// (rebuild mode) Set of material IDs used in the scene.
    material_ids_present_in_scene: HashSet<u32>,

    // ---- retained mode state ----
    retained: bool,
    row_meta: Vec<RowMeta>,
    entity_to_row: HashMap<MainEntity, u32>,
    /// `depth_histogram[d]` = number of instances whose BVH has depth `d`.
    depth_histogram: Vec<u32>,
    pending_materials: Vec<MainEntity>,
    material_refcount_by_id: HashMap<u32, u32>,
    material_asset_counts: HashMap<UntypedAssetId, u32>,
    slot_by_asset: HashMap<UntypedAssetId, u32>,
    vis_dirty_rows: Vec<u32>,
    vis_full_rebuild: bool,
    masks: BTreeMap<(RenderLayers, bool), VisibilityMask>,
    frame: u64,
    /// Rows added/updated/removed by the most recent extraction (diagnostics and tests).
    pub last_extract_touched: u32,
    /// Total mismatches found by validation (see `MeshletSettings::validate_retained_instances`).
    pub validation_mismatches: u64,
    validate_after_queue: bool,
}

impl InstanceManager {
    pub fn new() -> Self {
        Self {
            scene_instance_count: 0,
            max_bvh_depth: 0,

            instances: Vec::new(),
            instance_uniforms: RetainedBuffer::new("meshlet_instance_uniforms"),
            instance_aabbs: RetainedBuffer::new("meshlet_instance_aabbs"),
            instance_material_ids: RetainedBuffer::new("meshlet_instance_material_ids"),
            instance_bvh_root_nodes: RetainedBuffer::new("meshlet_instance_bvh_root_nodes"),
            view_instance_visibility: EntityHashMap::default(),

            next_material_id: 0,
            material_id_lookup: HashMap::default(),
            material_ids_present_in_scene: HashSet::default(),

            retained: false,
            row_meta: Vec::new(),
            entity_to_row: HashMap::default(),
            depth_histogram: Vec::new(),
            pending_materials: Vec::new(),
            material_refcount_by_id: HashMap::default(),
            material_asset_counts: HashMap::default(),
            slot_by_asset: HashMap::default(),
            vis_dirty_rows: Vec::new(),
            vis_full_rebuild: true,
            masks: BTreeMap::new(),
            frame: 0,
            last_extract_touched: 0,
            validation_mismatches: 0,
            validate_after_queue: false,
        }
    }

    /// `true` while the retained (incremental) mode is active.
    pub fn is_retained(&self) -> bool {
        self.retained
    }

    pub fn add_instance(
        &mut self,
        instance: MainEntity,
        root_bvh_node: u32,
        aabb: MeshletAabb,
        bvh_depth: u32,
        transform: &GlobalTransform,
        previous_transform: Option<&PreviousGlobalTransform>,
        render_layers: Option<&RenderLayers>,
        mesh_material_ids: &RenderMaterialInstances,
        render_material_bindings: &RenderMaterialBindings,
        not_shadow_receiver: bool,
        not_shadow_caster: bool,
    ) {
        let mesh_material = mesh_material_ids.mesh_material(instance);
        let mesh_material_binding_id = if let Some(mesh_material) = mesh_material {
            render_material_bindings
                .get(&mesh_material)
                .cloned()
                .unwrap_or_default()
        } else {
            // Use a dummy binding ID if the mesh has no material
            MaterialBindingId::default()
        };
        let mesh_uniform = build_uniform(
            transform,
            previous_transform,
            not_shadow_receiver,
            u32::from(mesh_material_binding_id.slot),
        );

        // Append instance data
        self.instances.push((
            instance,
            render_layers.cloned().unwrap_or(RenderLayers::default()),
            not_shadow_caster,
        ));
        self.instance_uniforms.vec_mut().push(mesh_uniform);
        self.instance_aabbs.vec_mut().push(aabb);
        self.instance_material_ids.vec_mut().push(0);
        self.instance_bvh_root_nodes.vec_mut().push(root_bvh_node);

        self.scene_instance_count += 1;
        self.max_bvh_depth = self.max_bvh_depth.max(bvh_depth);
    }

    /// Get the material ID for a [`crate::Material`].
    pub fn get_material_id(&mut self, material_asset_id: UntypedAssetId) -> u32 {
        *self
            .material_id_lookup
            .entry(material_asset_id)
            .or_insert_with(|| {
                self.next_material_id += 1;
                self.next_material_id
            })
    }

    pub fn material_present_in_scene(&self, material_id: &u32) -> bool {
        if self.retained {
            self.material_refcount_by_id
                .get(material_id)
                .is_some_and(|c| *c > 0)
        } else {
            self.material_ids_present_in_scene.contains(material_id)
        }
    }

    /// Distinct material assets used by meshlet instances.
    ///
    /// Rebuild mode scans every registered material instance (the original behavior, O(all
    /// instances)); retained mode returns the maintained set (O(distinct materials)).
    pub fn scene_material_assets(
        &self,
        render_material_instances: &RenderMaterialInstances,
    ) -> Vec<UntypedAssetId> {
        if self.retained {
            // Materials already resolved, plus those of rows still waiting for resolution (so a
            // freshly spawned instance can be shaded on the very frame it appears).
            let mut assets: HashSet<UntypedAssetId> =
                self.material_asset_counts.keys().copied().collect();
            for entity in &self.pending_materials {
                if let Some(material) = render_material_instances.instances.get(entity) {
                    assets.insert(material.asset_id);
                }
            }
            assets.into_iter().collect()
        } else {
            render_material_instances
                .instances
                .values()
                .map(|instance| instance.asset_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        }
    }

    /// Original per-frame reset (rebuild mode).
    pub fn reset(&mut self, entities: &Entities) {
        self.retained = false;
        self.scene_instance_count = 0;
        self.max_bvh_depth = 0;

        self.instances.clear();
        self.instance_uniforms.vec_mut().clear();
        self.instance_aabbs.vec_mut().clear();
        self.instance_material_ids.vec_mut().clear();
        self.instance_bvh_root_nodes.vec_mut().clear();
        self.view_instance_visibility
            .retain(|view_entity, _| entities.contains(*view_entity));
        self.view_instance_visibility
            .values_mut()
            .for_each(|b| b.get_mut().clear());

        self.next_material_id = 0;
        self.material_id_lookup.clear();
        self.material_ids_present_in_scene.clear();
    }

    /// Marks every buffer fully dirty (rebuild mode writes everything every frame).
    fn mark_all_buffers_dirty(&mut self) {
        self.instance_uniforms.mark_all_dirty();
        self.instance_aabbs.mark_all_dirty();
        self.instance_material_ids.mark_all_dirty();
        self.instance_bvh_root_nodes.mark_all_dirty();
    }

    // ------------------------------------------------------------------------------------------
    // Retained mode
    // ------------------------------------------------------------------------------------------

    /// Drops every retained row (material ids are kept: they are stable across frames).
    fn retained_clear_rows(&mut self) {
        self.retained = true;
        self.instances.clear();
        self.instance_uniforms.vec_mut().clear();
        self.instance_aabbs.vec_mut().clear();
        self.instance_material_ids.vec_mut().clear();
        self.instance_bvh_root_nodes.vec_mut().clear();
        self.row_meta.clear();
        self.entity_to_row.clear();
        self.depth_histogram.clear();
        self.pending_materials.clear();
        self.material_refcount_by_id.clear();
        self.material_asset_counts.clear();
        self.slot_by_asset.clear();
        self.vis_dirty_rows.clear();
        self.vis_full_rebuild = true;
        self.scene_instance_count = 0;
        self.max_bvh_depth = 0;
        self.mark_all_buffers_dirty();
    }

    fn recompute_counts(&mut self) {
        self.scene_instance_count = self.instances.len() as u32;
        self.max_bvh_depth = self
            .depth_histogram
            .iter()
            .rposition(|&c| c > 0)
            .unwrap_or(0) as u32;
    }

    fn hist_add(&mut self, depth: u32) {
        let d = depth as usize;
        if self.depth_histogram.len() <= d {
            self.depth_histogram.resize(d + 1, 0);
        }
        self.depth_histogram[d] += 1;
    }

    fn hist_remove(&mut self, depth: u32) {
        if let Some(c) = self.depth_histogram.get_mut(depth as usize) {
            *c = c.saturating_sub(1);
        }
    }

    /// Adds or updates the row of `entity`.
    fn retained_upsert(&mut self, entity: MainEntity, d: RowData) {
        if let Some(&row) = self.entity_to_row.get(&entity) {
            let r = row as usize;
            // Keep the already-resolved material slot bits of the existing row.
            let old_slot = self.instance_uniforms.data()[r].material_and_lightmap_bind_group_slot;
            let mut uniform = d.uniform;
            uniform.material_and_lightmap_bind_group_slot =
                (uniform.material_and_lightmap_bind_group_slot & 0xFFFF_0000) | (old_slot & 0xFFFF);
            self.instance_uniforms.set(r, uniform);
            // Only touch (and upload) what actually changed; a moving instance usually changes
            // only its transform.
            let old_aabb = self.instance_aabbs.data()[r];
            if old_aabb.center != d.aabb.center || old_aabb.half_extent != d.aabb.half_extent {
                self.instance_aabbs.set(r, d.aabb);
            }
            if self.instance_bvh_root_nodes.data()[r] != d.root_bvh_node {
                self.instance_bvh_root_nodes.set(r, d.root_bvh_node);
            }
            if self.instances[r].1 != d.layers || self.instances[r].2 != d.not_shadow_caster {
                self.instances[r] = (entity, d.layers, d.not_shadow_caster);
                self.vis_dirty_rows.push(row);
            }
            let old_depth = self.row_meta[r].bvh_depth;
            if old_depth != d.bvh_depth {
                self.row_meta[r].bvh_depth = d.bvh_depth;
                self.hist_remove(old_depth);
                self.hist_add(d.bvh_depth);
            }
        } else {
            let row = self.instances.len() as u32;
            self.instances.push((entity, d.layers, d.not_shadow_caster));
            self.instance_uniforms.push(d.uniform);
            self.instance_aabbs.push(d.aabb);
            self.instance_material_ids.push(0);
            self.instance_bvh_root_nodes.push(d.root_bvh_node);
            self.row_meta.push(RowMeta {
                entity,
                bvh_depth: d.bvh_depth,
                material_asset: None,
                material_id: 0,
                material_attempts: 0,
            });
            self.entity_to_row.insert(entity, row);
            self.hist_add(d.bvh_depth);
            self.pending_materials.push(entity);
            self.vis_dirty_rows.push(row);
        }
    }

    fn release_row_material(&mut self, r: usize) {
        let meta = &self.row_meta[r];
        if let Some(asset) = meta.material_asset {
            if let Some(c) = self.material_asset_counts.get_mut(&asset) {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    self.material_asset_counts.remove(&asset);
                    self.slot_by_asset.remove(&asset);
                }
            }
            if let Some(c) = self.material_refcount_by_id.get_mut(&meta.material_id) {
                *c = c.saturating_sub(1);
            }
        }
    }

    /// Removes the row of `entity` (swap-remove keeps the table dense).
    fn retained_remove(&mut self, entity: MainEntity) {
        let Some(row) = self.entity_to_row.remove(&entity) else {
            return;
        };
        let r = row as usize;
        self.release_row_material(r);
        let depth = self.row_meta[r].bvh_depth;
        self.hist_remove(depth);

        let last = self.instances.len() - 1;
        self.instances.swap_remove(r);
        self.instance_uniforms.swap_remove(r);
        self.instance_aabbs.swap_remove(r);
        self.instance_material_ids.swap_remove(r);
        self.instance_bvh_root_nodes.swap_remove(r);
        self.row_meta.swap_remove(r);
        if r != last {
            let moved = self.row_meta[r].entity;
            self.entity_to_row.insert(moved, row);
            self.vis_dirty_rows.push(row);
        }
    }

    fn set_row_material(&mut self, r: usize, asset: Option<UntypedAssetId>, id: u32) {
        let old_asset = self.row_meta[r].material_asset;
        if old_asset != asset || self.row_meta[r].material_id != id {
            self.release_row_material(r);
            if let Some(a) = asset {
                *self.material_asset_counts.entry(a).or_insert(0) += 1;
                *self.material_refcount_by_id.entry(id).or_insert(0) += 1;
            }
        }
        self.row_meta[r].material_asset = asset;
        self.row_meta[r].material_id = id;
        self.instance_material_ids.set(r, id);
    }

    fn patch_row_slot(&mut self, r: usize, slot: u32) {
        let u = &mut self.instance_uniforms.vec_mut()[r];
        u.material_and_lightmap_bind_group_slot =
            (u.material_and_lightmap_bind_group_slot & 0xFFFF_0000) | (slot & 0xFFFF);
        self.instance_uniforms.mark_dirty(r);
    }

    /// Resolves material ids and binding slots for rows that need it, and re-checks the binding
    /// slot of every distinct material (O(distinct materials) per frame).
    fn retained_resolve_materials(
        &mut self,
        dirty: &mut MeshletMaterialDirty,
        render_material_instances: &RenderMaterialInstances,
        render_material_bindings: &RenderMaterialBindings,
    ) {
        self.pending_materials.append(&mut dirty.0);
        let pending = core::mem::take(&mut self.pending_materials);
        let mut retry = Vec::new();
        let slot_of = |asset: &UntypedAssetId| -> u32 {
            render_material_bindings
                .get(asset)
                .map(|b| u32::from(b.slot))
                .unwrap_or_default()
        };
        for entity in pending {
            let Some(&row) = self.entity_to_row.get(&entity) else {
                continue;
            };
            let r = row as usize;
            match render_material_instances.instances.get(&entity) {
                Some(material) => {
                    let asset = material.asset_id;
                    let id = self.get_material_id(asset);
                    self.set_row_material(r, Some(asset), id);
                    let slot = slot_of(&asset);
                    self.slot_by_asset.insert(asset, slot);
                    self.patch_row_slot(r, slot);
                    self.row_meta[r].material_attempts = 0;
                }
                None => {
                    let meta = &mut self.row_meta[r];
                    meta.material_attempts = meta.material_attempts.saturating_add(1);
                    if meta.material_attempts < 3 {
                        retry.push(entity);
                    } else if meta.material_asset.is_some() {
                        // The material was removed.
                        self.set_row_material(r, None, 0);
                    }
                }
            }
        }
        self.pending_materials = retry;

        // A material's bindless slot can change (e.g. its bind group slab was reallocated).
        let drifted: Vec<(UntypedAssetId, u32)> = self
            .material_asset_counts
            .keys()
            .filter_map(|asset| {
                let slot = slot_of(asset);
                (self.slot_by_asset.get(asset) != Some(&slot)).then_some((*asset, slot))
            })
            .collect();
        for (asset, slot) in drifted {
            self.slot_by_asset.insert(asset, slot);
            for r in 0..self.row_meta.len() {
                if self.row_meta[r].material_asset == Some(asset) {
                    self.patch_row_slot(r, slot);
                }
            }
        }
    }

    // ---- visibility masks ----

    /// Returns the GPU buffer of the visibility mask for a view signature, updating it
    /// incrementally. Call [`finish_visibility_frame`](Self::finish_visibility_frame) once after all
    /// views of the frame.
    pub fn retained_visibility_mask(
        &mut self,
        view_layers: &RenderLayers,
        shadow_view: bool,
        device: &bevy_render::renderer::RenderDevice,
        queue: &bevy_render::renderer::RenderQueue,
    ) -> bevy_render::render_resource::Buffer {
        let n = self.instances.len();
        let words = n.div_ceil(32);
        let frame = self.frame;
        let full = self.vis_full_rebuild;
        let key = (view_layers.clone(), shadow_view);
        let mask = self.masks.entry(key).or_insert_with(|| VisibilityMask {
            buffer: RetainedBuffer::new("meshlet_view_instance_visibility"),
            stale: true,
            last_used_frame: frame,
        });
        mask.last_used_frame = frame;
        let hidden = |layers: &RenderLayers, not_shadow_caster: bool| {
            !view_layers.intersects(layers) || (shadow_view && not_shadow_caster)
        };
        let vec = mask.buffer.vec_mut();
        vec.resize(words, 0);
        if mask.stale || full {
            vec.fill(0);
            for (i, (_, layers, nsc)) in self.instances.iter().enumerate() {
                if hidden(layers, *nsc) {
                    vec[i / 32] |= 1 << (i % 32);
                }
            }
            mask.buffer.mark_all_dirty();
            mask.stale = false;
        } else {
            for &row in &self.vis_dirty_rows {
                let i = row as usize;
                if i >= n {
                    continue;
                }
                let (_, layers, nsc) = &self.instances[i];
                let bit = 1u32 << (i % 32);
                let word = &mut mask.buffer.vec_mut()[i / 32];
                if hidden(layers, *nsc) {
                    *word |= bit;
                } else {
                    *word &= !bit;
                }
                mask.buffer.mark_dirty(i / 32);
            }
        }
        mask.buffer.write(device, queue);
        mask.buffer
            .buffer()
            .expect("mask is non-empty when there are instances")
            .clone()
    }

    /// Ends the frame's visibility bookkeeping: unused masks that missed dirty rows become stale,
    /// long-unused masks are dropped, dirty rows are cleared.
    pub fn finish_visibility_frame(&mut self) {
        let frame = self.frame;
        let had_changes = self.vis_full_rebuild || !self.vis_dirty_rows.is_empty();
        self.masks
            .retain(|_, m| frame.saturating_sub(m.last_used_frame) < 600);
        if had_changes {
            for m in self.masks.values_mut() {
                if m.last_used_frame != frame {
                    m.stale = true;
                }
            }
        }
        self.vis_dirty_rows.clear();
        self.vis_full_rebuild = false;
        self.frame += 1;
    }
}

impl InstanceManager {
    /// Compares the retained table with an expected one built from scratch. Returns the number of
    /// mismatching rows (also logged). Material slot bits are ignored (resolved at queue time).
    fn validate_rows(&self, expected: impl Iterator<Item = (MainEntity, RowData)>) -> u32 {
        let mut mismatches = 0u32;
        let mut seen = 0usize;
        for (entity, e) in expected {
            seen += 1;
            let Some(&row) = self.entity_to_row.get(&entity) else {
                error!("retained meshlet instances: entity {entity:?} missing from the table");
                mismatches += 1;
                continue;
            };
            let r = row as usize;
            let got = &self.instance_uniforms.data()[r];
            let u = &e.uniform;
            let same = got.world_from_local == u.world_from_local
                && got.previous_world_from_local == u.previous_world_from_local
                && got.local_from_world_transpose_a == u.local_from_world_transpose_a
                && got.local_from_world_transpose_b == u.local_from_world_transpose_b
                && got.flags == u.flags
                && self.instance_aabbs.data()[r].center == e.aabb.center
                && self.instance_aabbs.data()[r].half_extent == e.aabb.half_extent
                && self.instance_bvh_root_nodes.data()[r] == e.root_bvh_node
                && self.instances[r].1 == e.layers
                && self.instances[r].2 == e.not_shadow_caster
                && self.row_meta[r].bvh_depth == e.bvh_depth;
            if !same {
                error!("retained meshlet instances: row {r} of {entity:?} differs from a fresh rebuild");
                mismatches += 1;
            }
        }
        if seen != self.instances.len() {
            error!(
                "retained meshlet instances: table has {} rows but the world has {} drawable instances",
                self.instances.len(),
                seen
            );
            mismatches += 1;
        }
        mismatches
    }

    /// Validates resolved material ids against the material tables. Returns mismatches.
    fn validate_materials(&self, render_material_instances: &RenderMaterialInstances) -> u32 {
        let mut mismatches = 0;
        for (r, meta) in self.row_meta.iter().enumerate() {
            if let Some(m) = render_material_instances.instances.get(&meta.entity) {
                let expected = self.material_id_lookup.get(&m.asset_id).copied();
                let id = self.instance_material_ids.data()[r];
                if expected != Some(id) || meta.material_asset != Some(m.asset_id) {
                    error!(
                        "retained meshlet instances: row {r} material id {id} != expected {expected:?}"
                    );
                    mismatches += 1;
                }
            }
        }
        mismatches
    }
}

fn build_uniform(
    transform: &GlobalTransform,
    previous_transform: Option<&PreviousGlobalTransform>,
    not_shadow_receiver: bool,
    material_slot: u32,
) -> MeshUniform {
    let transform = transform.affine();
    let previous_transform = previous_transform.map(|t| t.0).unwrap_or(transform);
    let mut flags = if not_shadow_receiver {
        MeshFlags::empty()
    } else {
        MeshFlags::SHADOW_RECEIVER
    };
    if transform.matrix3.determinant().is_sign_positive() {
        flags |= MeshFlags::SIGN_DETERMINANT_MODEL_3X3;
    }
    let transforms = MeshTransforms {
        world_from_local: transform.into(),
        previous_world_from_local: previous_transform.into(),
        flags: flags.bits(),
    };
    let mut uniform = MeshUniform::new(
        &transforms,
        0,
        MaterialBindingId::default().slot,
        None,
        None,
        None,
        None,
        None,
    );
    uniform.material_and_lightmap_bind_group_slot =
        (uniform.material_and_lightmap_bind_group_slot & 0xFFFF_0000) | (material_slot & 0xFFFF);
    uniform
}

type InstanceQueryData = (
    Entity,
    &'static MeshletMesh3d,
    &'static GlobalTransform,
    Option<&'static PreviousGlobalTransform>,
    Option<&'static RenderLayers>,
    Has<NotShadowReceiver>,
    Has<NotShadowCaster>,
);

type ChangedFilter = Or<(
    Changed<MeshletMesh3d>,
    Changed<GlobalTransform>,
    Changed<PreviousGlobalTransform>,
    Changed<RenderLayers>,
    Added<NotShadowCaster>,
    Added<NotShadowReceiver>,
)>;

/// A tiny per-frame cache of the (few) distinct [`MeshletMesh`] assets: resolving an asset costs
/// several hash lookups, which dominated extraction when thousands of instances share one asset.
/// `None` = asset not loaded yet (instance skipped).
struct AssetCache {
    entries: [Option<(AssetId<MeshletMesh>, Option<(u32, MeshletAabb, u32)>)>; Self::SIZE],
    len: usize,
}

impl AssetCache {
    const SIZE: usize = 16;

    fn new() -> Self {
        Self {
            entries: [None; Self::SIZE],
            len: 0,
        }
    }

    // Inlined on purpose: as a call with by-reference `ResMut` arguments and a large `Option`
    // return it cost ~0.9 ms per 100 k instances in the hot extraction loop.
    #[inline(always)]
    fn resolve(
        &mut self,
        asset_id: AssetId<MeshletMesh>,
        asset_server: &AssetServer,
        // `ResMut`s are only dereferenced on a cache miss: every `DerefMut` marks the resource as
        // changed, which is both a cost per instance and a false change signal.
        meshlet_mesh_manager: &mut ResMut<MeshletMeshManager>,
        assets: &mut ResMut<Assets<MeshletMesh>>,
    ) -> Option<(u32, MeshletAabb, u32)> {
        if let Some(found) = self.entries[..self.len]
            .iter()
            .flatten()
            .find(|(id, _)| *id == asset_id)
        {
            return found.1;
        }
        // Skip instances with an unloaded MeshletMesh asset
        let resolved = if asset_server.is_managed(asset_id)
            && !asset_server.is_loaded_with_dependencies(asset_id)
        {
            None
        } else {
            // Upload the asset's MeshletMesh data if not already done
            Some(meshlet_mesh_manager.queue_upload_if_needed(asset_id, assets))
        };
        if self.len < Self::SIZE {
            self.entries[self.len] = Some((asset_id, resolved));
            self.len += 1;
        }
        resolved
    }
}

/// The original per-frame rebuild of the whole instance table. Kept in its own non-inlined function
/// so the hot loop is compiled in isolation from the (much larger) retained path.
#[inline(never)]
fn rebuild_instances(
    instance_manager: &mut InstanceManager,
    all_query: &Query<InstanceQueryData>,
    asset_server: &AssetServer,
    meshlet_mesh_manager: &mut ResMut<MeshletMeshManager>,
    assets: &mut ResMut<Assets<MeshletMesh>>,
    mesh_material_ids: &RenderMaterialInstances,
    render_material_bindings: &RenderMaterialBindings,
    render_entities: &Entities,
) {
    let mut asset_cache = AssetCache::new();
    instance_manager.reset(render_entities);
    for (
        instance,
        meshlet_mesh,
        transform,
        previous_transform,
        render_layers,
        not_shadow_receiver,
        not_shadow_caster,
    ) in all_query
    {
        let Some((root_bvh_node, aabb, bvh_depth)) = asset_cache.resolve(
            meshlet_mesh.id(),
            asset_server,
            meshlet_mesh_manager,
            assets,
        ) else {
            continue;
        };
        instance_manager.add_instance(
            instance.into(),
            root_bvh_node,
            aabb,
            bvh_depth,
            transform,
            previous_transform,
            render_layers,
            mesh_material_ids,
            render_material_bindings,
            not_shadow_receiver,
            not_shadow_caster,
        );
    }
    instance_manager.mark_all_buffers_dirty();
    instance_manager.last_extract_touched = instance_manager.scene_instance_count;
}

pub fn extract_meshlet_mesh_entities(
    mut meshlet_mesh_manager: ResMut<MeshletMeshManager>,
    mut instance_manager: ResMut<InstanceManager>,
    // TODO: Replace main_world and system_state when Extract<ResMut<Assets<MeshletMesh>>> is possible
    mut main_world: ResMut<MainWorld>,
    mesh_material_ids: Res<RenderMaterialInstances>,
    render_material_bindings: Res<RenderMaterialBindings>,
    mut system_state: Local<
        Option<
            SystemState<(
                Query<InstanceQueryData>,
                Query<InstanceQueryData, ChangedFilter>,
                Res<AssetServer>,
                ResMut<Assets<MeshletMesh>>,
                MessageReader<AssetEvent<MeshletMesh>>,
                (
                    RemovedComponents<MeshletMesh3d>,
                    RemovedComponents<NotShadowCaster>,
                    RemovedComponents<NotShadowReceiver>,
                    RemovedComponents<RenderLayers>,
                    RemovedComponents<PreviousGlobalTransform>,
                ),
                Res<MeshletSettings>,
            )>,
        >,
    >,
    render_entities: &Entities,
) {
    // Get instances query
    if system_state.is_none() {
        *system_state = Some(SystemState::new(&mut main_world));
    }
    let system_state = system_state.as_mut().unwrap();
    let (
        all_query,
        changed_query,
        asset_server,
        mut assets,
        mut asset_events,
        (mut removed_mesh, mut removed_nsc, mut removed_nsr, mut removed_layers, mut removed_prev),
        settings,
    ) = system_state.get_mut(&mut main_world).unwrap();
    let want_retained = settings.retained_instances;
    instance_manager.validate_after_queue = want_retained && settings.validate_retained_instances;

    // Free GPU buffer space for any modified or dropped MeshletMesh assets. Any asset event
    // (including loads) can change which instances are drawable, so it forces a full resync.
    let mut full_resync = want_retained != instance_manager.retained;
    for asset_event in asset_events.read() {
        if let AssetEvent::Unused { id } | AssetEvent::Modified { id } = asset_event {
            meshlet_mesh_manager.remove(id);
        }
        full_resync = true;
    }

    let mut asset_cache = AssetCache::new();

    if !want_retained {
        // ---- original path: rebuild everything every frame ----
        rebuild_instances(
            &mut instance_manager,
            &all_query,
            &asset_server,
            &mut meshlet_mesh_manager,
            &mut assets,
            &mesh_material_ids,
            &render_material_bindings,
            render_entities,
        );
        return;
    }

    // ---- retained path ----
    let mut touched = 0u32;
    let build = |(
        instance,
        meshlet_mesh,
        transform,
        previous_transform,
        render_layers,
        not_shadow_receiver,
        not_shadow_caster,
    ): <InstanceQueryData as bevy_ecs::query::QueryData>::Item<'_, '_>,
                 asset_cache: &mut AssetCache,
                 meshlet_mesh_manager: &mut ResMut<MeshletMeshManager>,
                 assets: &mut ResMut<Assets<MeshletMesh>>|
     -> Option<(MainEntity, RowData)> {
        let (root_bvh_node, aabb, bvh_depth) = asset_cache.resolve(
            meshlet_mesh.id(),
            &asset_server,
            meshlet_mesh_manager,
            assets,
        )?;
        Some((
            instance.into(),
            RowData {
                uniform: build_uniform(transform, previous_transform, not_shadow_receiver, 0),
                aabb,
                root_bvh_node,
                layers: render_layers.cloned().unwrap_or_default(),
                not_shadow_caster,
                bvh_depth,
            },
        ))
    };

    if full_resync {
        instance_manager.retained_clear_rows();
        for item in &all_query {
            if let Some((entity, data)) = build(
                item,
                &mut asset_cache,
                &mut meshlet_mesh_manager,
                &mut assets,
            ) {
                instance_manager.retained_upsert(entity, data);
                touched += 1;
            }
        }
        // Drain removal events: everything was just rebuilt from the live world.
        removed_mesh.read().for_each(drop);
        removed_nsc.read().for_each(drop);
        removed_nsr.read().for_each(drop);
        removed_layers.read().for_each(drop);
        removed_prev.read().for_each(drop);
    } else {
        // Removed instances (also covers despawns).
        for entity in removed_mesh.read() {
            instance_manager.retained_remove(entity.into());
            touched += 1;
        }
        // Entities that lost an optional component but still exist: re-extract them.
        let mut refresh: Vec<Entity> = Vec::new();
        refresh.extend(removed_nsc.read());
        refresh.extend(removed_nsr.read());
        refresh.extend(removed_layers.read());
        refresh.extend(removed_prev.read());
        for entity in refresh {
            if let Ok(item) = all_query.get(entity)
                && let Some((e, data)) = build(
                    item,
                    &mut asset_cache,
                    &mut meshlet_mesh_manager,
                    &mut assets,
                )
            {
                instance_manager.retained_upsert(e, data);
                touched += 1;
            }
        }
        // Added or changed instances.
        for item in &changed_query {
            let entity: MainEntity = item.0.into();
            match build(
                item,
                &mut asset_cache,
                &mut meshlet_mesh_manager,
                &mut assets,
            ) {
                Some((e, data)) => instance_manager.retained_upsert(e, data),
                // Asset not loaded (yet): make sure no stale row exists.
                None => instance_manager.retained_remove(entity),
            }
            touched += 1;
        }
    }
    instance_manager.recompute_counts();
    instance_manager.last_extract_touched = touched;
    if settings.validate_retained_instances {
        let expected: Vec<(MainEntity, RowData)> = all_query
            .iter()
            .filter_map(|item| {
                build(
                    item,
                    &mut asset_cache,
                    &mut meshlet_mesh_manager,
                    &mut assets,
                )
            })
            .collect();
        let bad = instance_manager.validate_rows(expected.into_iter());
        instance_manager.validation_mismatches += u64::from(bad);
    }
    let _ = render_entities;
}

/// For each entity in the scene, record what material ID its material was assigned in the `prepare_material_meshlet_meshes` systems,
/// and note that the material is used by at least one entity in the scene.
pub fn queue_material_meshlet_meshes(
    mut instance_manager: ResMut<InstanceManager>,
    render_material_instances: Res<RenderMaterialInstances>,
    render_material_bindings: Res<RenderMaterialBindings>,
    mut dirty: ResMut<MeshletMaterialDirty>,
) {
    let instance_manager = instance_manager.deref_mut();

    if instance_manager.retained {
        instance_manager.retained_resolve_materials(
            &mut dirty,
            &render_material_instances,
            &render_material_bindings,
        );
        if instance_manager.validate_after_queue {
            let bad = instance_manager.validate_materials(&render_material_instances);
            instance_manager.validation_mismatches += u64::from(bad);
        }
        return;
    }
    dirty.0.clear();

    for (i, (instance, _, _)) in instance_manager.instances.iter().enumerate() {
        if let Some(material_instance) = render_material_instances.instances.get(instance)
            && let Some(material_id) = instance_manager
                .material_id_lookup
                .get(&material_instance.asset_id)
        {
            instance_manager
                .material_ids_present_in_scene
                .insert(*material_id);
            instance_manager.instance_material_ids.vec_mut()[i] = *material_id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_math::{Affine3A, Vec3};

    fn entity(i: u32) -> MainEntity {
        Entity::from_raw_u32(i).unwrap().into()
    }

    fn row(i: u32, depth: u32) -> RowData {
        RowData {
            uniform: build_uniform(
                &GlobalTransform::from(Affine3A::from_translation(Vec3::splat(i as f32))),
                None,
                false,
                0,
            ),
            aabb: MeshletAabb::default(),
            root_bvh_node: i,
            layers: RenderLayers::default(),
            not_shadow_caster: false,
            bvh_depth: depth,
        }
    }

    fn invariants(m: &InstanceManager) {
        let n = m.instances.len();
        assert_eq!(m.instance_uniforms.len(), n);
        assert_eq!(m.instance_aabbs.len(), n);
        assert_eq!(m.instance_material_ids.len(), n);
        assert_eq!(m.instance_bvh_root_nodes.len(), n);
        assert_eq!(m.row_meta.len(), n);
        assert_eq!(m.entity_to_row.len(), n);
        for (e, &r) in &m.entity_to_row {
            assert_eq!(m.row_meta[r as usize].entity, *e);
            assert_eq!(m.instances[r as usize].0, *e);
        }
        assert_eq!(
            m.depth_histogram.iter().sum::<u32>() as usize,
            n,
            "histogram counts every row"
        );
        let max = m.row_meta.iter().map(|r| r.bvh_depth).max().unwrap_or(0);
        let mut m2 = InstanceManager::new();
        m2.depth_histogram = m.depth_histogram.clone();
        m2.instances = m.instances.clone();
        m2.recompute_counts();
        assert_eq!(m2.max_bvh_depth, max);
    }

    #[test]
    fn upsert_remove_keep_the_table_dense_and_consistent() {
        let mut m = InstanceManager::new();
        m.retained = true;
        for i in 0..10 {
            m.retained_upsert(entity(i + 1), row(i, i % 4));
        }
        invariants(&m);
        // Remove from the middle: the last row moves into its slot.
        m.retained_remove(entity(3));
        invariants(&m);
        assert_eq!(m.instances.len(), 9);
        assert!(m.entity_to_row.get(&entity(3)).is_none());
        // The moved entity's data travelled with it.
        let moved_row = m.entity_to_row[&entity(10)] as usize;
        assert_eq!(m.instance_bvh_root_nodes.data()[moved_row], 9);
        // Update an existing entity in place (no new row).
        m.retained_upsert(entity(5), row(77, 3));
        invariants(&m);
        assert_eq!(m.instances.len(), 9);
        assert_eq!(
            m.instance_bvh_root_nodes.data()[m.entity_to_row[&entity(5)] as usize],
            77
        );
        // Remove everything.
        for i in 1..=10 {
            m.retained_remove(entity(i));
            invariants(&m);
        }
        assert!(m.instances.is_empty());
        // Removing a missing entity is a no-op.
        m.retained_remove(entity(99));
    }

    #[test]
    fn max_bvh_depth_tracks_removals() {
        let mut m = InstanceManager::new();
        m.retained = true;
        m.retained_upsert(entity(1), row(0, 2));
        m.retained_upsert(entity(2), row(1, 7));
        m.recompute_counts();
        assert_eq!(m.max_bvh_depth, 7);
        m.retained_remove(entity(2));
        m.recompute_counts();
        assert_eq!(m.max_bvh_depth, 2);
        m.retained_remove(entity(1));
        m.recompute_counts();
        assert_eq!(m.max_bvh_depth, 0);
        assert_eq!(m.scene_instance_count, 0);
    }

    #[test]
    fn material_accounting_follows_assignment_changes() {
        let mut m = InstanceManager::new();
        m.retained = true;
        m.retained_upsert(entity(1), row(0, 1));
        m.retained_upsert(entity(2), row(1, 1));
        let a = UntypedAssetId::Uuid {
            type_id: core::any::TypeId::of::<u8>(),
            uuid: bevy_asset::uuid::Uuid::from_u128(1),
        };
        let b = UntypedAssetId::Uuid {
            type_id: core::any::TypeId::of::<u8>(),
            uuid: bevy_asset::uuid::Uuid::from_u128(2),
        };
        let (ia, ib) = (m.get_material_id(a), m.get_material_id(b));
        assert_ne!(ia, ib);
        m.set_row_material(0, Some(a), ia);
        m.set_row_material(1, Some(a), ia);
        assert!(m.material_present_in_scene(&ia));
        assert!(!m.material_present_in_scene(&ib));
        m.set_row_material(1, Some(b), ib);
        assert!(m.material_present_in_scene(&ia) && m.material_present_in_scene(&ib));
        assert_eq!(m.material_asset_counts[&a], 1);
        m.retained_remove(entity(1));
        assert!(
            !m.material_present_in_scene(&ia),
            "last user of material a removed"
        );
        assert!(!m.material_asset_counts.contains_key(&a));
        m.retained_remove(entity(2));
        assert!(!m.material_present_in_scene(&ib));
        assert!(m.material_asset_counts.is_empty());
    }

    #[test]
    fn slot_bits_are_patched_without_touching_lightmap_bits() {
        let mut m = InstanceManager::new();
        m.retained = true;
        m.retained_upsert(entity(1), row(0, 1));
        m.instance_uniforms.vec_mut()[0].material_and_lightmap_bind_group_slot = 0xABCD_0005;
        m.patch_row_slot(0, 0x1_0007);
        assert_eq!(
            m.instance_uniforms.data()[0].material_and_lightmap_bind_group_slot,
            0xABCD_0007
        );
        // Re-upserting (e.g. a transform change) keeps the resolved slot.
        m.retained_upsert(entity(1), row(0, 1));
        assert_eq!(
            m.instance_uniforms.data()[0].material_and_lightmap_bind_group_slot & 0xFFFF,
            7
        );
    }
}
