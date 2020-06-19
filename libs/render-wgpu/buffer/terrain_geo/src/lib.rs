// This file is part of OpenFA.
//
// OpenFA is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// OpenFA is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with OpenFA.  If not, see <http://www.gnu.org/licenses/>.
mod debug_vertex;
mod icosahedron;
mod index_dependency_lut;
mod patch;
mod patch_tree;
mod patch_winding;
mod queue;
mod terrain_vertex;

pub use crate::{
    debug_vertex::DebugVertex, patch_winding::PatchWinding, terrain_vertex::TerrainVertex,
};
use crate::{index_dependency_lut::*, patch_tree::PatchTree};

use absolute_unit::Kilometers;
use camera::Camera;
use failure::Fallible;
use frame_graph::FrameStateTracker;
use gpu::GPU;
use nalgebra::{Matrix4, Point3};
use std::{cell::RefCell, mem, ops::Range, sync::Arc};
use zerocopy::{AsBytes, FromBytes};

const DBG_VERT_COUNT: usize = 4096;

const DBG_COLORS_BY_LEVEL: [[f32; 3]; 19] = [
    [0.75, 0.25, 0.25],
    [0.25, 0.75, 0.75],
    [0.75, 0.42, 0.25],
    [0.25, 0.58, 0.75],
    [0.75, 0.58, 0.25],
    [0.25, 0.42, 0.75],
    [0.75, 0.75, 0.25],
    [0.25, 0.25, 0.75],
    [0.58, 0.75, 0.25],
    [0.42, 0.25, 0.75],
    [0.58, 0.25, 0.75],
    [0.42, 0.75, 0.25],
    [0.25, 0.75, 0.25],
    [0.75, 0.25, 0.75],
    [0.25, 0.75, 0.42],
    [0.75, 0.25, 0.58],
    [0.25, 0.75, 0.58],
    [0.75, 0.25, 0.42],
    [0.10, 0.75, 0.72],
];

pub enum CpuDetailLevel {
    Low,
    Medium,
}

impl CpuDetailLevel {
    // max-level, target-refinement, buffer-size
    fn parameters(&self) -> (usize, f64, usize) {
        match self {
            Self::Low => (8, 150.0, 256),
            Self::Medium => (14, 150.0, 768),
        }
    }
}

pub enum GpuDetailLevel {
    Low,
    Medium,
    High,
}

impl GpuDetailLevel {
    // subdivisions
    fn parameters(&self) -> usize {
        match self {
            Self::Low => 3,
            Self::Medium => 5,
            Self::High => 7,
        }
    }

    fn vertices_per_subdivision(d: usize) -> usize {
        (((2f64.powf(d as f64) + 1.0) * (2f64.powf(d as f64) + 2.0)) / 2.0).floor() as usize
    }
}

#[repr(C)]
#[derive(AsBytes, FromBytes, Debug, Copy, Clone)]
pub struct SubdivisionContext {
    // Number of unique vertices in a patch. Skip past this many vertices in a buffer to
    // get to the next patch.
    target_stride: u32,
    pad: [u32; 3],
}

pub struct TerrainGeoBuffer {
    // Maximum number of patches for the patch buffer.
    desired_patch_count: usize,

    patch_tree: PatchTree,
    patch_windings: Vec<PatchWinding>,

    patch_upload_buffer: Arc<Box<wgpu::Buffer>>,
    patch_debug_index_buffer: wgpu::Buffer,

    subdivide_context: SubdivisionContext,
    subdivide_context_buffer: Arc<Box<wgpu::Buffer>>,
    target_vertex_buffer: Arc<Box<wgpu::Buffer>>,

    subdivide_prepare_pipeline: wgpu::ComputePipeline,
    subdivide_prepare_bind_group: wgpu::BindGroup,
    // subdivide_expand_pipeline: wgpu::ComputePipeline,
    // subdivide_expand_bind_group: wgpu::BindGroup,
    dbg_vertex_buffer: Arc<Box<wgpu::Buffer>>,
    dbg_index_buffer: Arc<Box<wgpu::Buffer>>,
    dbg_vertex_count: u32,
}

impl TerrainGeoBuffer {
    pub fn new(
        cpu_detail_level: CpuDetailLevel,
        gpu_detail_level: GpuDetailLevel,
        gpu: &GPU,
    ) -> Fallible<Arc<RefCell<Self>>> {
        let (max_level, target_refinement, desired_patch_count) = cpu_detail_level.parameters();
        let subdivisions = gpu_detail_level.parameters();
        let subdivisions = 0;

        let patch_tree = PatchTree::new(max_level, target_refinement, desired_patch_count);

        let mut patch_windings = Vec::with_capacity(desired_patch_count);
        patch_windings.resize(desired_patch_count, PatchWinding::Full);

        println!(
            "dbg_vertex_buffer: {:08X}",
            mem::size_of::<DebugVertex>() * DBG_VERT_COUNT
        );
        let dbg_vertex_buffer = Arc::new(Box::new(gpu.device().create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("terrain-geo-debug-vertices"),
                size: (mem::size_of::<DebugVertex>() * DBG_VERT_COUNT) as wgpu::BufferAddress,
                usage: wgpu::BufferUsage::all(),
            },
        )));
        let mut dbg_indices = Vec::new();
        dbg_indices.push(0);
        for i in 1u32..DBG_VERT_COUNT as u32 {
            dbg_indices.push(i);
            dbg_indices.push(i);
            dbg_indices.push(i);
        }
        let dbg_index_buffer = Arc::new(Box::new(gpu.push_slice(
            "terrain-geo-debug-indices",
            &dbg_indices,
            wgpu::BufferUsage::all(),
        )));

        let patch_upload_stride = 3; // 3 vertices per patch in the upload buffer.
        let patch_upload_byte_size = TerrainVertex::mem_size() * patch_upload_stride;
        println!(
            "patch_upload_buffer: {:08X}",
            patch_upload_byte_size * desired_patch_count
        );
        let patch_upload_buffer_size =
            (patch_upload_byte_size * desired_patch_count) as wgpu::BufferAddress;
        let patch_upload_buffer = Arc::new(Box::new(gpu.device().create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("terrain-geo-patch-vertex-buffer"),
                size: patch_upload_buffer_size,
                // TODO: remove vertex usage
                usage: wgpu::BufferUsage::STORAGE_READ
                    | wgpu::BufferUsage::COPY_DST
                    | wgpu::BufferUsage::VERTEX,
            },
        )));

        let mut patch_debug_indices = Vec::new();
        patch_debug_indices.push(0u32);
        patch_debug_indices.push(1u32);
        patch_debug_indices.push(1u32);
        patch_debug_indices.push(2u32);
        patch_debug_indices.push(2u32);
        patch_debug_indices.push(0u32);
        let patch_debug_index_buffer = gpu.push_slice(
            "terrain-geo-patch-indices",
            &patch_debug_indices,
            wgpu::BufferUsage::INDEX,
        );

        // Create the context buffer for uploading uniform data to our subdivision process.
        let subdivide_context = SubdivisionContext {
            //target_stride: GpuDetailLevel::vertices_per_subdivision(subdivisions) as u32,
            target_stride: 6,
            pad: [0; 3],
        };
        let subdivide_context_buffer_size =
            mem::size_of::<SubdivisionContext>() as wgpu::BufferAddress;
        let subdivide_context_buffer = Arc::new(Box::new(gpu.push_data(
            "subdivision-context",
            &subdivide_context,
            wgpu::BufferUsage::UNIFORM,
        )));

        // Create target vertex buffer.
        let target_patch_byte_size =
            mem::size_of::<TerrainVertex>() * subdivide_context.target_stride as usize;
        let target_vertex_buffer_size =
            (target_patch_byte_size * desired_patch_count) as wgpu::BufferAddress;
        let target_vertex_buffer = Arc::new(Box::new(gpu.device().create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("terrain-geo-sub-vertex-buffer"),
                size: target_vertex_buffer_size,
                usage: wgpu::BufferUsage::STORAGE
                    | wgpu::BufferUsage::COPY_DST
                    | wgpu::BufferUsage::VERTEX,
            },
        )));

        let subdivide_prepare_bind_group_layout =
            gpu.device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("terrain-geo-subdivide-bind-group-layout"),
                    bindings: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::UniformBuffer { dynamic: false },
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::StorageBuffer {
                                dynamic: false,
                                readonly: false,
                            },
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::StorageBuffer {
                                dynamic: false,
                                readonly: true,
                            },
                        },
                    ],
                });

        let subdivide_prepare_shader =
            gpu.create_shader_module(include_bytes!("../target/subdivide_prepare.comp.spirv"))?;
        let subdivide_prepare_pipeline =
            gpu.device()
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    layout: &gpu
                        .device()
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            bind_group_layouts: &[&subdivide_prepare_bind_group_layout],
                        }),
                    compute_stage: wgpu::ProgrammableStageDescriptor {
                        module: &subdivide_prepare_shader,
                        entry_point: "main",
                    },
                });

        let subdivide_prepare_bind_group =
            gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("terrain-geo-subdivide-bind-group"),
                layout: &subdivide_prepare_bind_group_layout,
                bindings: &[
                    wgpu::Binding {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer {
                            buffer: &subdivide_context_buffer,
                            range: 0..subdivide_context_buffer_size,
                        },
                    },
                    wgpu::Binding {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer {
                            buffer: &target_vertex_buffer,
                            range: 0..target_vertex_buffer_size,
                        },
                    },
                    wgpu::Binding {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer {
                            buffer: &patch_upload_buffer,
                            range: 0..patch_upload_buffer_size,
                        },
                    },
                ],
            });

        // Create the index dependence lut.
        let index_dependency_lut_buffer_size = (mem::size_of::<u32>()
            * Self::get_index_dependency_lut(subdivisions).len())
            as wgpu::BufferAddress;
        let index_dependency_lut_buffer = gpu.push_slice(
            "index-dependency-lut",
            Self::get_index_dependency_lut(subdivisions),
            wgpu::BufferUsage::STORAGE,
        );

        let subdivide_expand_bind_group_layout =
            gpu.device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("terrain-geo-subdivide-bind-group-layout"),
                    bindings: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::UniformBuffer { dynamic: false },
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::StorageBuffer {
                                dynamic: false,
                                readonly: true,
                            },
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::StorageBuffer {
                                dynamic: false,
                                readonly: false,
                            },
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStage::COMPUTE,
                            ty: wgpu::BindingType::StorageBuffer {
                                dynamic: false,
                                readonly: true,
                            },
                        },
                    ],
                });

        let subdivide_expand_shader =
            gpu.create_shader_module(include_bytes!("../target/subdivide_expand.comp.spirv"))?;
        let subdivide_expand_pipeline =
            gpu.device()
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    layout: &gpu
                        .device()
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            bind_group_layouts: &[&subdivide_expand_bind_group_layout],
                        }),
                    compute_stage: wgpu::ProgrammableStageDescriptor {
                        module: &subdivide_expand_shader,
                        entry_point: "main",
                    },
                });

        Ok(Arc::new(RefCell::new(Self {
            desired_patch_count,
            patch_tree,
            patch_windings,

            patch_upload_buffer,
            patch_debug_index_buffer,

            target_vertex_buffer,

            subdivide_context,
            subdivide_context_buffer,
            subdivide_prepare_pipeline,
            subdivide_prepare_bind_group,

            dbg_vertex_buffer,
            dbg_index_buffer,
            dbg_vertex_count: 0,
        })))
    }

    pub fn make_upload_buffer(
        &mut self,
        camera: &Camera,
        gpu: &GPU,
        tracker: &mut FrameStateTracker,
    ) -> Fallible<()> {
        let mut dbg_verts = Vec::with_capacity(3 * self.desired_patch_count);
        let mut verts = Vec::with_capacity(3 * self.desired_patch_count);
        let mut dbg_indices = Vec::with_capacity(3 * self.desired_patch_count);
        let mut live_patches = Vec::with_capacity(self.desired_patch_count);
        self.patch_tree.optimize_for_view(camera, &mut live_patches);
        assert!(live_patches.len() <= self.desired_patch_count);

        let scale = Matrix4::new_scaling(1_000.0);
        let view = camera.view::<Kilometers>();
        for (offset, (i, winding)) in live_patches.iter().enumerate() {
            self.patch_windings[offset] = *winding;
            let patch = self.patch_tree.get_patch(*i);
            if offset >= self.desired_patch_count {
                continue;
            }
            let [v0, v1, v2] = patch.points();
            let n0 = v0.coords.normalize();
            let n1 = v1.coords.normalize();
            let n2 = v2.coords.normalize();

            // project patch verts from global coordinates into view space.
            let v0 = scale * view.to_homogeneous() * v0.to_homogeneous();
            let v1 = scale * view.to_homogeneous() * v1.to_homogeneous();
            let v2 = scale * view.to_homogeneous() * v2.to_homogeneous();

            verts.push(TerrainVertex::new(&Point3::from(v0.xyz()), &n0));
            verts.push(TerrainVertex::new(&Point3::from(v1.xyz()), &n1));
            verts.push(TerrainVertex::new(&Point3::from(v2.xyz()), &n2));

            dbg_indices.push(dbg_verts.len() as u32);
            dbg_indices.push(dbg_verts.len() as u32 + 1);
            dbg_indices.push(dbg_verts.len() as u32 + 2);
            let level = self.patch_tree.level_of_patch(*i);
            let clr = DBG_COLORS_BY_LEVEL[level];
            dbg_verts.push(DebugVertex::new(&Point3::from(v0.xyz()), &n0, &clr));
            dbg_verts.push(DebugVertex::new(&Point3::from(v1.xyz()), &n1, &clr));
            dbg_verts.push(DebugVertex::new(&Point3::from(v2.xyz()), &n2, &clr));
        }
        self.dbg_vertex_count = dbg_verts.len() as u32;
        //println!("verts: {}: {:?}", cnt, Instant::now() - loop_start);

        while verts.len() < 3 * self.desired_patch_count {
            verts.push(TerrainVertex::empty());
        }
        gpu.upload_slice_to(
            "terrain-geo-patch-vertex-upload-buffer",
            &verts,
            self.patch_upload_buffer.clone(),
            wgpu::BufferUsage::all(),
            tracker,
        );

        while dbg_verts.len() < DBG_VERT_COUNT {
            dbg_verts.push(DebugVertex {
                position: [0f32, 0f32, 0f32, 0f32],
                color: [0f32, 0f32, 1f32, 1f32],
            });
        }
        gpu.upload_slice_to(
            "terrain-geo-debug-vertices-upload-buffer",
            &dbg_verts,
            self.dbg_vertex_buffer.clone(),
            wgpu::BufferUsage::all(),
            tracker,
        );
        gpu.upload_slice_to(
            "terrain-geo-debug-indices-upload-buffer",
            &dbg_indices,
            self.dbg_index_buffer.clone(),
            wgpu::BufferUsage::all(),
            tracker,
        );

        //println!("dt: {:?}", Instant::now() - loop_start);
        Ok(())
    }

    pub fn precompute<'a>(
        &'a self,
        mut cpass: wgpu::ComputePass<'a>,
    ) -> Fallible<wgpu::ComputePass<'a>> {
        cpass.set_pipeline(&self.subdivide_prepare_pipeline);
        cpass.set_bind_group(0, &self.subdivide_prepare_bind_group, &[]);
        cpass.dispatch(3 * self.desired_patch_count as u32, 1, 1);
        Ok(cpass)
    }

    fn get_index_buffer() -> Vec<u32> {
        // This needs to line up with our index dependence lut. There's not really any trivial
        // way
        vec![]
    }

    fn get_index_dependency_lut(subdivisions: usize) -> &'static [u32] {
        match subdivisions {
            0 => &INDEX_DEPENDENCY_LUT0,
            1 => &INDEX_DEPENDENCY_LUT1,
            2 => &INDEX_DEPENDENCY_LUT2,
            3 => &INDEX_DEPENDENCY_LUT3,
            4 => &INDEX_DEPENDENCY_LUT4,
            5 => &INDEX_DEPENDENCY_LUT5,
            6 => &INDEX_DEPENDENCY_LUT6,
            7 => &INDEX_DEPENDENCY_LUT7,
            8 => &INDEX_DEPENDENCY_LUT8,
            _ => panic!("subdivisions only supported up to 9"),
        }
    }

    /*
    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }
    pub fn block_bind_group(&self) -> &wgpu::BindGroup {
        &self.block_bind_group
    }
    */

    pub fn num_patches(&self) -> i32 {
        self.desired_patch_count as i32
    }

    pub fn vertex_buffer(&self) -> &wgpu::Buffer {
        &self.target_vertex_buffer
    }

    pub fn patch_upload_buffer(&self) -> &wgpu::Buffer {
        &self.patch_upload_buffer
    }

    pub fn patch_debug_index_buffer(&self) -> &wgpu::Buffer {
        &self.patch_debug_index_buffer
    }

    pub fn patch_offset(&self, patch_number: i32) -> i32 {
        assert!(patch_number >= 0);
        (patch_number as u32 * self.subdivide_context.target_stride) as i32
    }

    pub fn patch_index_range(&self) -> Range<u32> {
        0..6
    }

    pub fn debug_index_buffer(&self) -> &wgpu::Buffer {
        &self.dbg_index_buffer
    }

    pub fn debug_vertex_buffer(&self) -> &wgpu::Buffer {
        &self.dbg_vertex_buffer
    }

    pub fn debug_index_range(&self) -> Range<u32> {
        0..self.dbg_vertex_count
        //0..(DBG_VERT_COUNT as u32 * 2u32)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_subdivision_vertex_counts() {
        let expect = vec![3, 6, 15, 45, 153, 561, 2145, 8385];
        for (i, &value) in expect.iter().enumerate() {
            assert_eq!(value, GpuDetailLevel::vertices_per_subdivision(i));
        }
    }

    #[test]
    fn test_built_index_lut() {
        // let lut = TerrainGeoBuffer::build_index_dependence_lut();
        // for (i, (j, k)) in lut.iter().skip(3).enumerate() {
        //     println!("at offset: {}: {}, {}", i + 3, j, k);
        //     assert!((i as u32) + 3 > *j);
        //     assert!((i as u32) + 3 > *k);
        // }
        // assert_eq!(lut[0], (0, 0));
        // assert_eq!(lut[1], (0, 0));
        // assert_eq!(lut[2], (0, 0));
        // assert_eq!(lut[3], (0, 1));
        // assert_eq!(lut[4], (1, 2));
        // assert_eq!(lut[5], (2, 0));
        for i in 0..300 {
            let patch_id = i / 3;
            let offset = i % 3;
            assert_eq!(i, patch_id * 3 + offset);
        }
    }
}
