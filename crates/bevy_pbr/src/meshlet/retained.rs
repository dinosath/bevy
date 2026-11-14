//! A GPU storage buffer mirrored by a CPU `Vec<T>` that uploads **only the elements that changed**.
//!
//! `StorageBuffer<Vec<T>>` re-encodes and re-uploads the whole vector every `write_buffer`. For a
//! scene with 100 k meshlet instances that is megabytes per frame even when nothing moved.
//! [`RetainedBuffer`] keeps the GPU buffer alive across frames and writes dirty element ranges.
//!
//! The byte layout is *identical* to `StorageBuffer<Vec<T>>` (std430, encase), so shaders and bind
//! groups are unchanged; this is verified by tests that compare range writes to a full encode.

use alloc::vec::Vec;
use bevy_render::{
    render_resource::{
        encase::{internal::WriteInto, StorageBuffer as EncaseStorageBuffer},
        BindingResource, Buffer, BufferBinding, BufferDescriptor, BufferSize, BufferUsages,
        ShaderSize, ShaderType,
    },
    renderer::{RenderDevice, RenderQueue},
};
use core::ops::Range;

/// Dirty elements closer together than this are written as one range (fewer `write_buffer` calls
/// at the price of re-uploading a few unchanged elements).
const MERGE_GAP: usize = 32;

/// Turns a dirty-element bitset into sorted, merged, in-bounds ranges.
///
/// Bit `i` of the set corresponds to element `i`; bits `>= len` are ignored; ranges are merged when
/// separated by at most `merge_gap` elements.
pub(crate) fn plan_ranges(bits: &[u64], len: usize, merge_gap: usize) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (w, &word) in bits.iter().enumerate() {
        let mut x = word;
        while x != 0 {
            let i = w * 64 + x.trailing_zeros() as usize;
            x &= x - 1;
            if i >= len {
                return ranges;
            }
            match ranges.last_mut() {
                Some(last) if i <= last.end + merge_gap => last.end = i + 1,
                _ => ranges.push(i..i + 1),
            }
        }
    }
    ranges
}

/// Encodes `data` (std430 array elements) with a **reused** scratch buffer and returns exactly
/// `data.len() * stride` bytes.
///
/// encase's `Vec<u8>` writer never truncates, so after a large encode a later small one would
/// otherwise expose the stale tail (this overran `write_buffer` destinations before it was fixed).
fn encode_slice<'a, T: ShaderType + ShaderSize + WriteInto>(
    scratch: &'a mut EncaseStorageBuffer<Vec<u8>>,
    data: &[T],
    stride: u64,
) -> &'a [u8] {
    scratch
        .write(data)
        .expect("encase encoding of a slice cannot fail");
    let scratch: &'a EncaseStorageBuffer<Vec<u8>> = scratch;
    &scratch.as_ref()[..data.len() * stride as usize]
}

/// See the [module docs](self).
pub struct RetainedBuffer<T: ShaderType + ShaderSize + WriteInto> {
    data: Vec<T>,
    buffer: Option<Buffer>,
    capacity_elems: usize,
    label: &'static str,
    /// One bit per element: set = needs upload. O(1) to mark, no sorting or deduplication.
    dirty_bits: Vec<u64>,
    has_dirty: bool,
    all_dirty: bool,
    /// Bytes per element in the GPU array (std430 stride), known after the first encode.
    stride: u64,
    bound_len: usize,
    scratch: EncaseStorageBuffer<Vec<u8>>,
    /// Bytes uploaded by the most recent [`write`](Self::write) (diagnostics / tests).
    pub last_bytes_written: u64,
}

impl<T: ShaderType + ShaderSize + WriteInto> RetainedBuffer<T> {
    /// Creates an empty buffer with a debug label.
    pub fn new(label: &'static str) -> Self {
        Self {
            data: Vec::new(),
            buffer: None,
            capacity_elems: 0,
            label,
            dirty_bits: Vec::new(),
            has_dirty: false,
            all_dirty: false,
            stride: 0,
            bound_len: 0,
            scratch: EncaseStorageBuffer::new(Vec::new()),
            last_bytes_written: 0,
        }
    }

    /// The CPU mirror.
    pub fn data(&self) -> &[T] {
        &self.data
    }

    /// Mutable access to the CPU mirror. Callers must mark what they change
    /// ([`mark_dirty`](Self::mark_dirty) / [`mark_all_dirty`](Self::mark_all_dirty)).
    pub fn vec_mut(&mut self) -> &mut Vec<T> {
        &mut self.data
    }

    /// Number of elements.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Marks element `i` as needing upload.
    #[inline]
    pub fn mark_dirty(&mut self, i: usize) {
        if self.all_dirty {
            return;
        }
        let w = i / 64;
        if self.dirty_bits.len() <= w {
            self.dirty_bits.resize(w + 1, 0);
        }
        self.dirty_bits[w] |= 1 << (i % 64);
        self.has_dirty = true;
    }

    /// Marks every element as needing upload.
    pub fn mark_all_dirty(&mut self) {
        self.all_dirty = true;
        self.dirty_bits.fill(0);
        self.has_dirty = false;
    }

    /// Appends an element (marked dirty).
    pub fn push(&mut self, value: T) {
        self.data.push(value);
        self.mark_dirty(self.data.len() - 1);
    }

    /// Overwrites element `i` (marked dirty).
    pub fn set(&mut self, i: usize, value: T) {
        self.data[i] = value;
        self.mark_dirty(i);
    }

    /// Removes element `i` by moving the last element into its place. Returns `true` if an element
    /// was moved (i.e. `i` was not the last).
    pub fn swap_remove(&mut self, i: usize) -> bool {
        let last = self.data.len() - 1;
        self.data.swap_remove(i);
        if i != last {
            self.mark_dirty(i);
            true
        } else {
            false
        }
    }

    /// The GPU buffer, once written.
    pub fn buffer(&self) -> Option<&Buffer> {
        self.buffer.as_ref()
    }

    /// Binding of exactly `len` elements (capacity beyond `len` is not exposed, so
    /// `arrayLength` in shaders matches the element count like `StorageBuffer` does).
    pub fn binding(&self) -> Option<BindingResource<'_>> {
        Some(BindingResource::Buffer(BufferBinding {
            buffer: self.buffer.as_ref()?,
            offset: 0,
            size: BufferSize::new(self.bound_len as u64 * self.stride),
        }))
    }

    fn encode(&mut self, range: Range<usize>) -> &[u8] {
        encode_slice(&mut self.scratch, &self.data[range], self.stride)
    }

    /// Uploads dirty elements (all of them if `mark_all_dirty` was called or the GPU buffer had to
    /// be (re)allocated).
    pub fn write(&mut self, device: &RenderDevice, queue: &RenderQueue) {
        self.last_bytes_written = 0;
        let len = self.data.len();
        if len == 0 {
            self.dirty_bits.fill(0);
            self.has_dirty = false;
            self.all_dirty = false;
            self.bound_len = 0;
            return;
        }
        if self.stride == 0 {
            // First use: the scratch is empty, so its length is exactly one element.
            self.scratch
                .write(&self.data[0..1])
                .expect("encase encoding of a slice cannot fail");
            self.stride = self.scratch.as_ref().len() as u64;
        }
        let stride = self.stride;

        if self.buffer.is_none() || len > self.capacity_elems {
            self.capacity_elems = len.max(self.capacity_elems * 2).max(256);
            self.buffer = Some(device.create_buffer(&BufferDescriptor {
                label: Some(self.label),
                size: self.capacity_elems as u64 * stride,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.all_dirty = true;
        }

        let ranges = if self.all_dirty {
            alloc::vec![0..len]
        } else if self.has_dirty {
            plan_ranges(&self.dirty_bits, len, MERGE_GAP)
        } else {
            Vec::new()
        };
        self.dirty_bits.fill(0);
        self.has_dirty = false;
        self.all_dirty = false;

        let buffer = self.buffer.clone().expect("allocated above");
        for r in ranges {
            let offset = r.start as u64 * stride;
            let bytes = self.encode(r);
            debug_assert_eq!(bytes.len() as u64 % stride, 0);
            queue.write_buffer(&buffer, offset, bytes);
            self.last_bytes_written += bytes.len() as u64;
        }
        self.bound_len = len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meshlet::asset::MeshletAabb;
    use bevy_math::Vec3;

    fn bits(indices: &[usize]) -> Vec<u64> {
        let mut v = vec![0u64; indices.iter().max().map_or(0, |m| m / 64 + 1)];
        for &i in indices {
            v[i / 64] |= 1 << (i % 64);
        }
        v
    }

    #[test]
    fn plan_ranges_merges_clips_and_handles_word_boundaries() {
        assert_eq!(
            plan_ranges(&bits(&[10, 3, 3, 4, 100, 5, 11, 5000]), 200, 2),
            vec![3..6, 10..12, 100..101]
        );
        // Gap merging.
        assert_eq!(plan_ranges(&bits(&[0, 40]), 100, 32), vec![0..1, 40..41]);
        assert_eq!(plan_ranges(&bits(&[0, 33]), 100, 32), vec![0..34]);
        // Across 64-bit word boundaries.
        assert_eq!(plan_ranges(&bits(&[62, 63, 64, 65]), 100, 0), vec![62..66]);
        assert_eq!(plan_ranges(&bits(&[63, 65]), 100, 0), vec![63..64, 65..66]);
        // Empty and fully out of bounds.
        assert!(plan_ranges(&[], 10, 4).is_empty());
        assert!(plan_ranges(&bits(&[50]), 10, 4).is_empty());
    }

    #[test]
    fn marking_is_idempotent_and_swap_remove_marks_one_slot() {
        let mut b: RetainedBuffer<u32> = RetainedBuffer::new("t");
        for i in 0..70 {
            b.push(i);
        }
        b.mark_dirty(3);
        b.mark_dirty(3);
        b.mark_dirty(69);
        let r = plan_ranges(&b.dirty_bits, b.data.len(), 0);
        assert_eq!(r, vec![0..70], "pushes marked everything dirty");
        b.dirty_bits.fill(0);
        b.has_dirty = false;
        assert!(b.swap_remove(1));
        assert_eq!(
            b.data(),
            &[
                0, 69, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43,
                44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64,
                65, 66, 67, 68
            ]
        );
        assert_eq!(plan_ranges(&b.dirty_bits, b.data.len(), 0), vec![1..2]);
        b.dirty_bits.fill(0);
        b.has_dirty = false;
        let last = b.data.len() - 1;
        assert!(
            !b.swap_remove(last),
            "removing the last element moves nothing"
        );
        assert!(plan_ranges(&b.dirty_bits, b.data.len(), 0).is_empty());
    }

    /// Writing element ranges at `start * stride` must equal the corresponding bytes of a full
    /// encode of the whole vector — this is what makes partial uploads layout-compatible.
    fn assert_ranges_match_full_encode<T: ShaderType + ShaderSize + WriteInto + Clone>(
        data: Vec<T>,
    ) {
        let mut full = EncaseStorageBuffer::new(Vec::<u8>::new());
        full.write(&data).unwrap();
        let full = full.as_ref().to_vec();
        let stride = full.len() / data.len();
        assert_eq!(stride * data.len(), full.len());

        for (a, b) in [
            (0usize, 1usize),
            (3, 4),
            (2, 9),
            (0, data.len()),
            (data.len() - 1, data.len()),
        ] {
            let mut s = EncaseStorageBuffer::new(Vec::<u8>::new());
            s.write(&data[a..b]).unwrap();
            assert_eq!(s.as_ref(), &full[a * stride..b * stride], "range {a}..{b}");
        }
    }

    #[test]
    fn range_encoding_matches_full_encoding_for_u32_and_padded_structs() {
        assert_ranges_match_full_encode((0..64u32).collect());
        assert_ranges_match_full_encode(
            (0..32)
                .map(|i| MeshletAabb {
                    center: Vec3::new(i as f32, 1.0, 2.0),
                    half_extent: Vec3::new(0.5, i as f32, 3.0),
                })
                .collect(),
        );
        // The padded struct really has a stride larger than its Rust size.
        let mut s = EncaseStorageBuffer::new(Vec::<u8>::new());
        s.write(&vec![MeshletAabb::default(); 2]).unwrap();
        assert_eq!(s.as_ref().len(), 64, "std430 stride of MeshletAabb is 32");
        assert!(size_of::<MeshletAabb>() < 32);
    }

    #[test]
    fn mesh_uniform_range_encoding_matches() {
        use crate::{MeshFlags, MeshTransforms, MeshUniform};
        use bevy_math::Affine3A;
        let mk = |i: u32| {
            let t = Affine3A::from_translation(Vec3::splat(i as f32));
            MeshUniform::new(
                &MeshTransforms {
                    world_from_local: t.into(),
                    previous_world_from_local: t.into(),
                    flags: MeshFlags::empty().bits(),
                },
                0,
                bevy_render::material_bind_groups::MaterialBindingId::default().slot,
                None,
                None,
                None,
                None,
                None,
            )
        };
        assert_ranges_match_full_encode((0..16).map(mk).collect());
    }

    /// Regression: reusing one scratch for encodes of different sizes must not leak stale bytes.
    #[test]
    fn scratch_reuse_never_returns_stale_tail() {
        let data: Vec<MeshletAabb> = (0..40)
            .map(|i| MeshletAabb {
                center: Vec3::splat(i as f32),
                half_extent: Vec3::splat(1.0 + i as f32),
            })
            .collect();
        let mut full = EncaseStorageBuffer::new(Vec::<u8>::new());
        full.write(&data).unwrap();
        let stride = (full.as_ref().len() / data.len()) as u64;

        let mut scratch = EncaseStorageBuffer::new(Vec::<u8>::new());
        // Large first, then small, then medium, then single: every result must be exact.
        for (a, b) in [(0usize, 40usize), (3, 4), (10, 25), (39, 40), (0, 1)] {
            let got = encode_slice(&mut scratch, &data[a..b], stride).to_vec();
            assert_eq!(
                got.len() as u64,
                (b - a) as u64 * stride,
                "length of {a}..{b}"
            );
            assert_eq!(
                got,
                full.as_ref()[a * stride as usize..b * stride as usize],
                "bytes of {a}..{b}"
            );
        }
    }
}
