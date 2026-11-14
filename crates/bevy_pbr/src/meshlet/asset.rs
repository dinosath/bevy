use alloc::sync::Arc;
use bevy_asset::{
    io::{Reader, Writer},
    saver::{AssetSaver, SavedAsset},
    Asset, AssetLoader, AssetPath, AsyncReadExt, AsyncWriteExt, LoadContext,
};
use bevy_math::{Vec2, Vec3};
use bevy_reflect::TypePath;
use bevy_render::render_resource::ShaderType;
use bevy_tasks::block_on;
use bytemuck::{Pod, Zeroable};
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use std::io::{Read, Write};
use thiserror::Error;

/// Unique identifier for the [`MeshletMesh`] asset format.
const MESHLET_MESH_ASSET_MAGIC: u64 = 1717551717668;

/// The current version of the [`MeshletMesh`] asset format.
pub const MESHLET_MESH_ASSET_VERSION: u64 = 3;

/// A mesh that has been pre-processed into multiple small clusters of triangles called meshlets.
///
/// A [`bevy_mesh::Mesh`] can be converted to a [`MeshletMesh`] using `MeshletMesh::from_mesh` when the `meshlet_processor` cargo feature is enabled.
/// The conversion step is very slow, and is meant to be ran once ahead of time, and not during runtime. This type of mesh is not suitable for
/// dynamically generated geometry.
///
/// There are restrictions on the [`Material`](`crate::Material`) functionality that can be used with this type of mesh.
/// * Materials have no control over the vertex shader or vertex attributes.
/// * Materials must be opaque. Transparent, alpha masked, and transmissive materials are not supported.
/// * Do not use normal maps baked from higher-poly geometry. Use the high-poly geometry directly and skip the normal map.
///   * If additional detail is needed, a smaller tiling normal map not baked from a mesh is ok.
/// * Material shaders must not use builtin functions that automatically calculate derivatives <https://gpuweb.github.io/gpuweb/wgsl/#derivatives>.
///   * Performing manual arithmetic on texture coordinates (UVs) is forbidden. Use the chain-rule version of arithmetic functions instead (TODO: not yet implemented).
/// * Limited control over [`bevy_render::render_resource::RenderPipelineDescriptor`] attributes.
/// * Materials must use the [`Material::meshlet_mesh_fragment_shader`](`crate::Material::meshlet_mesh_fragment_shader`) method (and similar variants for prepass/deferred shaders)
///   which requires certain shader patterns that differ from the regular material shaders.
///
/// See also [`MeshletMesh3d`](`super::MeshletMesh3d`) and [`MeshletPlugin`](`super::MeshletPlugin`).
#[derive(Asset, TypePath, Clone)]
pub struct MeshletMesh {
    /// Quantized and bitstream-packed vertex positions for meshlet vertices.
    pub(crate) vertex_positions: Arc<[u32]>,
    /// Octahedral-encoded and 2x16snorm packed normals for meshlet vertices.
    pub(crate) vertex_normals: Arc<[u32]>,
    /// Uncompressed vertex texture coordinates for meshlet vertices.
    pub(crate) vertex_uvs: Arc<[Vec2]>,
    /// Triangle indices for meshlets.
    pub(crate) indices: Arc<[u8]>,
    /// The BVH8 used for culling and LOD selection of the meshlets. The root is at index 0.
    pub(crate) bvh: Arc<[BvhNode]>,
    /// The list of meshlets making up this mesh.
    pub(crate) meshlets: Arc<[Meshlet]>,
    /// Spherical bounding volumes.
    pub(crate) meshlet_cull_data: Arc<[MeshletCullData]>,
    /// The tight AABB of the meshlet mesh, used for frustum and occlusion culling at the instance
    /// level.
    pub(crate) aabb: MeshletAabb,
    /// The depth of the culling BVH, used to determine the number of dispatches at runtime.
    pub(crate) bvh_depth: u32,
}

/// Size and structure statistics of a [`MeshletMesh`], for tooling, budgeting and benchmarks.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MeshletMeshStats {
    /// Total meshlets across all LOD levels.
    pub meshlets: u32,
    /// Triangles summed over all meshlets (all LODs).
    pub triangles: u64,
    /// Vertices summed over all meshlets (all LODs; shared vertices counted per meshlet).
    pub meshlet_vertices: u64,
    /// BVH8 node count.
    pub bvh_nodes: u32,
    /// BVH depth (number of cull dispatches needed at runtime).
    pub bvh_depth: u32,
    /// Bytes of bit-packed vertex positions.
    pub position_bytes: u64,
    /// Bytes of packed normals.
    pub normal_bytes: u64,
    /// Bytes of UVs.
    pub uv_bytes: u64,
    /// Bytes of triangle indices.
    pub index_bytes: u64,
    /// Bytes of BVH nodes.
    pub bvh_bytes: u64,
    /// Bytes of meshlet descriptors.
    pub meshlet_descriptor_bytes: u64,
    /// Bytes of per-meshlet cull data.
    pub cull_data_bytes: u64,
    /// Size of the `.meshlet_mesh` file payload after lz4 frame compression (what
    /// [`MeshletMeshSaver`] writes, header excluded).
    pub lz4_file_bytes: u64,
}

impl MeshletMeshStats {
    /// Total uncompressed (in-memory / on-GPU) bytes.
    pub fn total_bytes(&self) -> u64 {
        self.position_bytes
            + self.normal_bytes
            + self.uv_bytes
            + self.index_bytes
            + self.bvh_bytes
            + self.meshlet_descriptor_bytes
            + self.cull_data_bytes
    }
}

impl MeshletMesh {
    /// Size and structure statistics (see [`MeshletMeshStats`]).
    pub fn stats(&self) -> MeshletMeshStats {
        MeshletMeshStats {
            meshlets: self.meshlets.len() as u32,
            triangles: self
                .meshlets
                .iter()
                .map(|m| u64::from(m.triangle_count))
                .sum(),
            meshlet_vertices: self
                .meshlets
                .iter()
                .map(|m| u64::from(m.vertex_count_minus_one) + 1)
                .sum(),
            bvh_nodes: self.bvh.len() as u32,
            bvh_depth: self.bvh_depth,
            position_bytes: size_of_val(&*self.vertex_positions) as u64,
            normal_bytes: size_of_val(&*self.vertex_normals) as u64,
            uv_bytes: size_of_val(&*self.vertex_uvs) as u64,
            index_bytes: size_of_val(&*self.indices) as u64,
            bvh_bytes: size_of_val(&*self.bvh) as u64,
            meshlet_descriptor_bytes: size_of_val(&*self.meshlets) as u64,
            cull_data_bytes: size_of_val(&*self.meshlet_cull_data) as u64,
            lz4_file_bytes: self.lz4_size(),
        }
    }

    /// Compressed payload size, computed with the same layout as [`MeshletMeshSaver`].
    fn lz4_size(&self) -> u64 {
        struct Counter(u64);
        impl Write for Counter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0 += buf.len() as u64;
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut w = FrameEncoder::new(Counter(0));
        let ok = write_slice(&self.vertex_positions, &mut w).is_ok()
            && write_slice(&self.vertex_normals, &mut w).is_ok()
            && write_slice(&self.vertex_uvs, &mut w).is_ok()
            && write_slice(&self.indices, &mut w).is_ok()
            && write_slice(&self.bvh, &mut w).is_ok()
            && write_slice(&self.meshlets, &mut w).is_ok()
            && write_slice(&self.meshlet_cull_data, &mut w).is_ok();
        match (ok, w.finish()) {
            (true, Ok(c)) => c.0,
            _ => 0,
        }
    }
}

/// A single BVH8 node in the BVH used for culling and LOD selection of a [`MeshletMesh`].
#[derive(Copy, Clone, Default, Pod, Zeroable)]
#[repr(C)]
pub struct BvhNode {
    /// The tight AABBs of this node's children, used for frustum and occlusion during BVH
    /// traversal.
    pub aabbs: [MeshletAabbErrorOffset; 8],
    /// The LOD bounding spheres of this node's children, used for LOD selection during BVH
    /// traversal.
    pub lod_bounds: [MeshletBoundingSphere; 8],
    /// If `u8::MAX`, it indicates that the child of each children is a BVH node, otherwise it is the number of meshlets in the group.
    pub child_counts: [u8; 8],
    pub _padding: [u32; 2],
}

/// A single meshlet within a [`MeshletMesh`].
#[derive(Copy, Clone, Pod, Zeroable)]
#[repr(C)]
pub struct Meshlet {
    /// The bit offset within the parent mesh's [`MeshletMesh::vertex_positions`] buffer where the vertex positions for this meshlet begin.
    pub start_vertex_position_bit: u32,
    /// The offset within the parent mesh's [`MeshletMesh::vertex_normals`] and [`MeshletMesh::vertex_uvs`] buffers
    /// where non-position vertex attributes for this meshlet begin.
    pub start_vertex_attribute_id: u32,
    /// The offset within the parent mesh's [`MeshletMesh::indices`] buffer where the indices for this meshlet begin.
    pub start_index_id: u32,
    /// The amount of vertices in this meshlet (minus one to fit 256 in a `u8`).
    pub vertex_count_minus_one: u8,
    /// The amount of triangles in this meshlet.
    pub triangle_count: u8,
    /// Unused.
    pub padding: u16,
    /// Number of bits used to store the X channel of vertex positions within this meshlet.
    pub bits_per_vertex_position_channel_x: u8,
    /// Number of bits used to store the Y channel of vertex positions within this meshlet.
    pub bits_per_vertex_position_channel_y: u8,
    /// Number of bits used to store the Z channel of vertex positions within this meshlet.
    pub bits_per_vertex_position_channel_z: u8,
    /// Power of 2 factor used to quantize vertex positions within this meshlet.
    pub vertex_position_quantization_factor: u8,
    /// Minimum quantized X channel value of vertex positions within this meshlet.
    pub min_vertex_position_channel_x: f32,
    /// Minimum quantized Y channel value of vertex positions within this meshlet.
    pub min_vertex_position_channel_y: f32,
    /// Minimum quantized Z channel value of vertex positions within this meshlet.
    pub min_vertex_position_channel_z: f32,
}

/// Bounding spheres used for culling and choosing level of detail for a [`Meshlet`].
#[derive(Copy, Clone, Pod, Zeroable)]
#[repr(C)]
pub struct MeshletCullData {
    /// Tight bounding box, used for frustum and occlusion culling for this meshlet.
    pub aabb: MeshletAabbErrorOffset,
    /// Bounding sphere used for determining if this meshlet's group is at the correct level of detail for a given view.
    pub lod_group_sphere: MeshletBoundingSphere,
}

/// An axis-aligned bounding box used for a [`Meshlet`].
#[derive(Copy, Clone, Default, Pod, Zeroable, ShaderType)]
#[repr(C)]
pub struct MeshletAabb {
    pub center: Vec3,
    pub half_extent: Vec3,
}

// An axis-aligned bounding box used for a [`Meshlet`].
#[derive(Copy, Clone, Default, Pod, Zeroable, ShaderType)]
#[repr(C)]
pub struct MeshletAabbErrorOffset {
    pub center: Vec3,
    pub error: f32,
    pub half_extent: Vec3,
    pub child_offset: u32,
}

/// A spherical bounding volume used for a [`Meshlet`].
#[derive(Copy, Clone, Default, Pod, Zeroable)]
#[repr(C)]
pub struct MeshletBoundingSphere {
    pub center: Vec3,
    pub radius: f32,
}

/// An [`AssetSaver`] for `.meshlet_mesh` [`MeshletMesh`] assets.
#[derive(TypePath)]
pub struct MeshletMeshSaver;

impl AssetSaver for MeshletMeshSaver {
    type Asset = MeshletMesh;
    type Settings = ();
    type OutputLoader = MeshletMeshLoader;
    type Error = MeshletMeshSaveOrLoadError;

    async fn save(
        &self,
        writer: &mut Writer,
        asset: SavedAsset<'_, '_, MeshletMesh>,
        _settings: &(),
        _asset_path: AssetPath<'_>,
    ) -> Result<(), MeshletMeshSaveOrLoadError> {
        // Write asset magic number
        writer
            .write_all(&MESHLET_MESH_ASSET_MAGIC.to_le_bytes())
            .await?;

        // Write asset version
        writer
            .write_all(&MESHLET_MESH_ASSET_VERSION.to_le_bytes())
            .await?;

        writer.write_all(bytemuck::bytes_of(&asset.aabb)).await?;
        writer
            .write_all(bytemuck::bytes_of(&asset.bvh_depth))
            .await?;

        // Compress and write asset data
        let mut writer = FrameEncoder::new(AsyncWriteSyncAdapter(writer));
        write_slice(&asset.vertex_positions, &mut writer)?;
        write_slice(&asset.vertex_normals, &mut writer)?;
        write_slice(&asset.vertex_uvs, &mut writer)?;
        write_slice(&asset.indices, &mut writer)?;
        write_slice(&asset.bvh, &mut writer)?;
        write_slice(&asset.meshlets, &mut writer)?;
        write_slice(&asset.meshlet_cull_data, &mut writer)?;
        writer.finish()?;

        Ok(())
    }
}

/// An [`AssetLoader`] for `.meshlet_mesh` [`MeshletMesh`] assets.
#[derive(TypePath)]
pub struct MeshletMeshLoader;

impl AssetLoader for MeshletMeshLoader {
    type Asset = MeshletMesh;
    type Settings = ();
    type Error = MeshletMeshSaveOrLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<MeshletMesh, MeshletMeshSaveOrLoadError> {
        // Load and check magic number
        let magic = async_read_u64(reader).await?;
        if magic != MESHLET_MESH_ASSET_MAGIC {
            return Err(MeshletMeshSaveOrLoadError::WrongFileType);
        }

        // Load and check asset version
        let version = async_read_u64(reader).await?;
        if version != MESHLET_MESH_ASSET_VERSION {
            return Err(MeshletMeshSaveOrLoadError::WrongVersion { found: version });
        }

        let mut bytes = [0u8; size_of::<MeshletAabb>()];
        reader.read_exact(&mut bytes).await?;
        let aabb = bytemuck::cast(bytes);
        let mut bytes = [0u8; size_of::<u32>()];
        reader.read_exact(&mut bytes).await?;
        let bvh_depth = u32::from_le_bytes(bytes);

        // Load and decompress asset data
        let reader = &mut FrameDecoder::new(AsyncReadSyncAdapter(reader));
        let vertex_positions = read_slice(reader)?;
        let vertex_normals = read_slice(reader)?;
        let vertex_uvs = read_slice(reader)?;
        let indices = read_slice(reader)?;
        let bvh = read_slice(reader)?;
        let meshlets = read_slice(reader)?;
        let meshlet_cull_data = read_slice(reader)?;

        Ok(MeshletMesh {
            vertex_positions,
            vertex_normals,
            vertex_uvs,
            indices,
            bvh,
            meshlets,
            meshlet_cull_data,
            aabb,
            bvh_depth,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["meshlet_mesh"]
    }
}

#[derive(Error, Debug)]
pub enum MeshletMeshSaveOrLoadError {
    #[error("file was not a MeshletMesh asset")]
    WrongFileType,
    #[error("expected asset version {MESHLET_MESH_ASSET_VERSION} but found version {found}")]
    WrongVersion { found: u64 },
    #[error("failed to compress or decompress asset data")]
    CompressionOrDecompression(#[from] lz4_flex::frame::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

async fn async_read_u64(reader: &mut dyn Reader) -> Result<u64, std::io::Error> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes).await?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_u64(reader: &mut dyn Read) -> Result<u64, std::io::Error> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_slice<T: Pod>(
    field: &[T],
    writer: &mut dyn Write,
) -> Result<(), MeshletMeshSaveOrLoadError> {
    writer.write_all(&(field.len() as u64).to_le_bytes())?;
    writer.write_all(bytemuck::cast_slice(field))?;
    Ok(())
}

fn read_slice<T: Pod>(reader: &mut dyn Read) -> Result<Arc<[T]>, std::io::Error> {
    let len = read_u64(reader)? as usize;

    let mut data: Arc<[T]> = core::iter::repeat_with(T::zeroed).take(len).collect();
    let slice = Arc::get_mut(&mut data).unwrap();
    reader.read_exact(bytemuck::cast_slice_mut(slice))?;

    Ok(data)
}

// TODO: Use async for everything and get rid of this adapter
struct AsyncWriteSyncAdapter<'a>(&'a mut Writer);

impl Write for AsyncWriteSyncAdapter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        block_on(self.0.write(buf))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        block_on(self.0.flush())
    }
}

// TODO: Use async for everything and get rid of this adapter
struct AsyncReadSyncAdapter<'a>(&'a mut dyn Reader);

impl Read for AsyncReadSyncAdapter<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        block_on(self.0.read(buf))
    }
}
