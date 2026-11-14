//! Render high-poly 3d meshes using an efficient GPU-driven method. See [`MeshletPlugin`] and [`MeshletMesh`] for details.

mod asset;
#[cfg(feature = "meshlet_processor")]
mod from_mesh;
mod instance_manager;
mod material_pipeline_prepare;
mod material_shade_nodes;
mod meshlet_mesh_manager;
mod persistent_buffer;
mod persistent_buffer_impls;
mod pipelines;
mod pressure;
mod resource_manager;
mod visibility_buffer_raster_node;

pub(crate) use self::{
    instance_manager::{queue_material_meshlet_meshes, InstanceManager},
    material_pipeline_prepare::{
        prepare_material_meshlet_meshes_main_opaque_pass, prepare_material_meshlet_meshes_prepass,
    },
};

pub use self::asset::{
    MeshletMesh, MeshletMeshLoader, MeshletMeshSaver, MeshletMeshStats, MESHLET_MESH_ASSET_VERSION,
};
#[cfg(feature = "meshlet_processor")]
pub use self::from_mesh::{
    MeshToMeshletMeshConversionError, MeshletBuildConfig,
    MESHLET_DEFAULT_VERTEX_POSITION_QUANTIZATION_FACTOR,
};
pub use self::pressure::{MeshletPressure, MESHLET_MAX_PRESSURE_SCALE};
use self::{
    instance_manager::extract_meshlet_mesh_entities,
    material_pipeline_prepare::{
        MeshletViewMaterialsDeferredGBufferPrepass, MeshletViewMaterialsMainOpaquePass,
        MeshletViewMaterialsPrepass,
    },
    material_shade_nodes::{
        meshlet_deferred_gbuffer_prepass, meshlet_main_opaque_pass, meshlet_prepass,
    },
    meshlet_mesh_manager::perform_pending_meshlet_mesh_writes,
    pipelines::*,
    pressure::{prepare_pressure_probe, readback_pressure, PressureProbe},
    resource_manager::{
        prepare_meshlet_per_frame_resources, prepare_meshlet_view_bind_groups, ResourceManager,
    },
    visibility_buffer_raster_node::meshlet_visibility_buffer_raster,
};
use crate::render::{per_view_shadow_pass, EARLY_SHADOW_PASS};
use crate::{meshlet::meshlet_mesh_manager::init_meshlet_mesh_manager, PreviousGlobalTransform};
use bevy_app::{App, Plugin};
use bevy_asset::{embedded_asset, AssetApp, AssetId, Handle};
use bevy_camera::visibility::{self, Visibility, VisibilityClass};
use bevy_core_pipeline::{
    core_3d::main_opaque_pass_3d,
    prepass::{DeferredPrepass, MotionVectorPrepass, NormalPrepass},
    schedule::{Core3d, Core3dSystems},
};
use bevy_derive::{Deref, DerefMut};
use bevy_ecs::{
    component::Component,
    entity::Entity,
    query::Has,
    reflect::{ReflectComponent, ReflectResource},
    resource::Resource,
    schedule::IntoScheduleConfigs,
    system::{Commands, Query, Res},
    template::FromTemplate,
};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use bevy_render::{
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    renderer::RenderDevice,
    settings::WgpuFeatures,
    view::{prepare_view_targets, Msaa},
    ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
};
use bevy_shader::load_shader_library;
use bevy_transform::components::Transform;
use derive_more::From;
use tracing::error;

/// Runtime tuning of the meshlet renderer. Inserted with defaults by [`MeshletPlugin`]; modify it
/// in the main world.
#[derive(Resource, Debug, Clone, ExtractResource, Reflect)]
#[reflect(Resource, Debug, Default)]
#[extract_app(RenderApp)]
pub struct MeshletSettings {
    /// Shadow views build their depth pyramid after the first pass (needed for the second-pass
    /// occlusion test). If `true`, they build it **again** after the second pass so next frame's
    /// early pass can use a pyramid that includes newly visible geometry.
    ///
    /// Defaults to `false`: the rebuild only makes next frame's *early* occlusion test more
    /// effective, never more correct (a pyramid with fewer occluders can only cull less), and it
    /// cost ~1.4 ms per shadow cascade at 2048² on the reference GPU — about 15 % of a 3-cascade
    /// frame in `examples/large_scenes/forest_benchmark`. Set to `true` to restore the previous
    /// behavior.
    pub rebuild_shadow_depth_pyramid_after_second_pass: bool,
    /// Multiplier on the screen-space error budget (in pixels of the *shadow map*) used for LOD
    /// selection in shadow views. `1.0` = same 1-pixel budget as the main view (default);
    /// `2.0` allows 2-pixel error, rendering shadows from coarser clusters. Values below `1.0`
    /// select finer shadow geometry (at a cost). Must be positive.
    pub shadow_lod_error_scale: f32,
    /// If a cull queue overflows (or nears capacity), coarsen the LOD of all meshlet views
    /// ([`MeshletPressure::lod_error_scale`]) instead of silently dropping geometry. Defaults to
    /// `true`. Pressure is measured either way.
    pub degrade_on_queue_pressure: bool,
    /// Measure cull-queue occupancy (GPU counter copies + asynchronous readback). Defaults to
    /// `true`. Disabling also disables `degrade_on_queue_pressure`.
    pub detect_queue_pressure: bool,
}

impl Default for MeshletSettings {
    fn default() -> Self {
        Self {
            rebuild_shadow_depth_pyramid_after_second_pass: false,
            shadow_lod_error_scale: 1.0,
            degrade_on_queue_pressure: true,
            detect_queue_pressure: true,
        }
    }
}

/// Provides a plugin for rendering large amounts of high-poly 3d meshes using an efficient GPU-driven method. See also [`MeshletMesh`].
///
/// Rendering dense scenes made of high-poly meshes with thousands or millions of triangles is extremely expensive in Bevy's standard renderer.
/// Once meshes are pre-processed into a [`MeshletMesh`], this plugin can render these kinds of scenes very efficiently.
///
/// In comparison to Bevy's standard renderer:
/// * Much more efficient culling. Meshlets can be culled individually, instead of all or nothing culling for entire meshes at a time.
///   Additionally, occlusion culling can eliminate meshlets that would cause overdraw.
/// * Much more efficient batching. All geometry can be rasterized in a single draw.
/// * Scales better with large amounts of dense geometry and overdraw. Bevy's standard renderer will bottleneck sooner.
/// * Near-seamless level of detail (LOD).
/// * Much greater base overhead. Rendering will be slower and use more memory than Bevy's standard renderer
///   with small amounts of geometry and overdraw.
/// * Requires preprocessing meshes. See [`MeshletMesh`] for details.
/// * Limitations on the kinds of materials you can use. See [`MeshletMesh`] for details.
///
/// This plugin requires a fairly recent GPU that supports [`WgpuFeatures::TEXTURE_INT64_ATOMIC`].
///
/// This plugin currently works only on the Vulkan and Metal backends.
///
/// This plugin is not compatible with [`Msaa`]. Any camera rendering a [`MeshletMesh`] must have
/// [`Msaa`] set to [`Msaa::Off`].
///
/// Mixing forward+prepass and deferred rendering for opaque materials is not currently supported when using this plugin.
/// You must use one or the other by setting [`crate::DefaultOpaqueRendererMethod`].
/// Do not override [`crate::Material::opaque_render_method`] for any material when using this plugin.
///
/// ![A render of the Stanford dragon as a `MeshletMesh`](https://raw.githubusercontent.com/bevyengine/bevy/main/crates/bevy_pbr/src/meshlet/meshlet_preview.png)
pub struct MeshletPlugin {
    /// The maximum amount of clusters that can be processed at once,
    /// used to control the size of a pre-allocated GPU buffer.
    ///
    /// If this number is too low, you'll see rendering artifacts like missing or blinking meshes.
    ///
    /// Each cluster slot costs 4 bytes of VRAM.
    ///
    /// Must not be greater than 2^25.
    pub cluster_buffer_slots: u32,
}

impl MeshletPlugin {
    /// [`WgpuFeatures`] required for this plugin to function.
    pub fn required_wgpu_features() -> WgpuFeatures {
        WgpuFeatures::TEXTURE_INT64_ATOMIC
            | WgpuFeatures::TEXTURE_ATOMIC
            | WgpuFeatures::SHADER_INT64
            | WgpuFeatures::SUBGROUP
            | WgpuFeatures::DEPTH_CLIP_CONTROL
            | WgpuFeatures::IMMEDIATES
    }
}

impl Plugin for MeshletPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(target_endian = "big")]
        compile_error!("MeshletPlugin is only supported on little-endian processors.");

        if self.cluster_buffer_slots > 2_u32.pow(25) {
            error!("MeshletPlugin::cluster_buffer_slots must not be greater than 2^25.");
            std::process::exit(1);
        }

        load_shader_library!(app, "bindings.wesl");
        load_shader_library!(app, "visibility_buffer_resolve.wesl");
        load_shader_library!(app, "cull_shared.wesl");
        embedded_asset!(app, "clear_visibility_buffer.wesl");
        embedded_asset!(app, "cull_instances.wesl");
        embedded_asset!(app, "cull_bvh.wesl");
        embedded_asset!(app, "cull_clusters.wesl");
        embedded_asset!(app, "visibility_buffer_software_raster.wesl");
        embedded_asset!(app, "visibility_buffer_hardware_raster.wesl");
        embedded_asset!(app, "meshlet_mesh_material.wesl");
        embedded_asset!(app, "resolve_render_targets.wesl");
        embedded_asset!(app, "remap_1d_to_2d_dispatch.wesl");
        embedded_asset!(app, "fill_counts.wesl");

        app.init_asset::<MeshletMesh>()
            .register_asset_loader(MeshletMeshLoader)
            .init_resource::<MeshletSettings>()
            .add_plugins(ExtractResourcePlugin::<MeshletSettings>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Create a variable here so we can move-capture it.
        let cluster_buffer_slots = self.cluster_buffer_slots;
        let init_resource_manager_system =
            move |mut commands: Commands, render_device: Res<RenderDevice>| {
                commands
                    .insert_resource(ResourceManager::new(cluster_buffer_slots, &render_device));
                commands.insert_resource(PressureProbe::new(&render_device));
                commands.insert_resource(MeshletPressure::new(cluster_buffer_slots));
            };

        render_app
            .insert_resource(InstanceManager::new())
            .add_systems(
                RenderStartup,
                (
                    check_meshlet_features,
                    (
                        (init_resource_manager_system, init_meshlet_pipelines).chain(),
                        init_meshlet_mesh_manager,
                    )
                        .run_if(meshlet_supported),
                )
                    .chain(),
            )
            .add_systems(
                ExtractSchedule,
                extract_meshlet_mesh_entities.run_if(meshlet_supported),
            )
            .add_systems(
                Render,
                (
                    perform_pending_meshlet_mesh_writes.in_set(RenderSystems::PrepareAssets),
                    configure_meshlet_views
                        .after(prepare_view_targets)
                        .in_set(RenderSystems::PrepareViews),
                    prepare_meshlet_per_frame_resources.in_set(RenderSystems::PrepareResources),
                    prepare_pressure_probe.in_set(RenderSystems::PrepareResources),
                    readback_pressure.in_set(RenderSystems::Cleanup),
                    prepare_meshlet_view_bind_groups.in_set(RenderSystems::PrepareBindGroups),
                    queue_material_meshlet_meshes.in_set(RenderSystems::QueueMeshes),
                    prepare_material_meshlet_meshes_main_opaque_pass
                        .in_set(RenderSystems::QueueMeshes)
                        .before(queue_material_meshlet_meshes),
                )
                    .run_if(meshlet_supported),
            )
            .add_systems(
                Core3d,
                (
                    meshlet_visibility_buffer_raster
                        .before(per_view_shadow_pass::<EARLY_SHADOW_PASS>),
                    meshlet_prepass
                        .after(per_view_shadow_pass::<EARLY_SHADOW_PASS>)
                        .in_set(Core3dSystems::Prepass),
                    meshlet_deferred_gbuffer_prepass
                        .after(meshlet_prepass)
                        .in_set(Core3dSystems::Prepass),
                    meshlet_main_opaque_pass
                        .before(main_opaque_pass_3d)
                        .in_set(Core3dSystems::MainPass),
                )
                    .run_if(meshlet_supported),
            );
    }
}

/// Render-world resource recording whether the GPU supports [`MeshletPlugin`]
/// (see [`MeshletPlugin::required_wgpu_features`]).
///
/// If it is `supported == false`, the plugin logs an error and **disables every meshlet system**:
/// [`MeshletMesh3d`] entities are simply not rendered, and the app keeps running so it can fall
/// back to the standard renderer (previously the process exited).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshletSupport {
    /// `true` if the device exposes all required features.
    pub supported: bool,
}

pub(crate) fn meshlet_supported(support: Option<Res<MeshletSupport>>) -> bool {
    support.is_some_and(|s| s.supported)
}

fn check_meshlet_features(mut commands: Commands, render_device: Res<RenderDevice>) {
    let features = render_device.features();
    let required = MeshletPlugin::required_wgpu_features();
    let supported = features.contains(required);
    if !supported {
        error!(
            "MeshletPlugin disabled: GPU lacks support for required features: {:?}. \
             `MeshletMesh3d` entities will not be rendered.",
            required.difference(features)
        );
    }
    commands.insert_resource(MeshletSupport { supported });
}

/// The meshlet mesh equivalent of [`bevy_mesh::Mesh3d`].
#[derive(
    Component, FromTemplate, Clone, Debug, Default, Deref, DerefMut, Reflect, PartialEq, Eq, From,
)]
#[reflect(Component, Default, Clone, PartialEq)]
#[require(Transform, PreviousGlobalTransform, Visibility, VisibilityClass)]
#[component(on_add = visibility::add_visibility_class::<MeshletMesh3d>)]
pub struct MeshletMesh3d(pub Handle<MeshletMesh>);

impl From<MeshletMesh3d> for AssetId<MeshletMesh> {
    fn from(mesh: MeshletMesh3d) -> Self {
        mesh.id()
    }
}

impl From<&MeshletMesh3d> for AssetId<MeshletMesh> {
    fn from(mesh: &MeshletMesh3d) -> Self {
        mesh.id()
    }
}

fn configure_meshlet_views(
    mut views_3d: Query<(
        Entity,
        &Msaa,
        Has<NormalPrepass>,
        Has<MotionVectorPrepass>,
        Has<DeferredPrepass>,
    )>,
    mut commands: Commands,
) {
    for (entity, msaa, normal_prepass, motion_vector_prepass, deferred_prepass) in &mut views_3d {
        if *msaa != Msaa::Off {
            error!("MeshletPlugin can't be used with MSAA. Add Msaa::Off to your camera to use this plugin.");
            std::process::exit(1);
        }

        if !(normal_prepass || motion_vector_prepass || deferred_prepass) {
            commands
                .entity(entity)
                .insert(MeshletViewMaterialsMainOpaquePass::default());
        } else {
            // TODO: Should we add both Prepass and DeferredGBufferPrepass materials here, and in other systems/nodes?
            commands.entity(entity).insert((
                MeshletViewMaterialsMainOpaquePass::default(),
                MeshletViewMaterialsPrepass::default(),
                MeshletViewMaterialsDeferredGBufferPrepass::default(),
            ));
        }
    }
}
