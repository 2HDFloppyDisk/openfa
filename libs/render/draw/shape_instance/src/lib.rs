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
use camera::CameraAbstract;
use failure::{bail, ensure, Fallible};
use global_layout::GlobalSets;
use nalgebra::Matrix4;
use nalgebra::Point3;
use omnilib::OmniLib;
use shape_chunk::{
    Chunk, ChunkIndex, ChunkPart, ClosedChunk, DrawSelection, DrawState, OpenChunk,
    ShapeChunkManager, ShapeId, Vertex,
};
use specs::{
    world::Index as EntityId, DispatcherBuilder, Entities, Join, ReadStorage, System, VecStorage,
};
use std::{
    collections::HashMap,
    mem,
    sync::{Arc, RwLock},
    time::Instant,
};
use vulkano::buffer::cpu_pool::CpuBufferPoolSubbuffer;
use vulkano::buffer::CpuAccessibleBuffer;
use vulkano::command_buffer::AutoCommandBufferBuilder;
use vulkano::{
    buffer::{BufferAccess, BufferSlice, BufferUsage, CpuBufferPool, DeviceLocalBuffer},
    command_buffer::DrawIndirectCommand,
    descriptor::descriptor_set::{DescriptorSet, PersistentDescriptorSet},
    device::Device,
    framebuffer::Subpass,
    instance::QueueFamily,
    pipeline::{
        depth_stencil::{Compare, DepthBounds, DepthStencil},
        GraphicsPipeline, GraphicsPipelineAbstract,
    },
    sync::GpuFuture,
};
use window::GraphicsWindow;
use world::{
    component::{ShapeMesh, Transform},
    Entity, World,
};

mod vs {
    use vulkano_shaders::shader;

    shader! {
    ty: "vertex",
    include: ["./libs/render"],
    src: "
        #version 450
        #include <common/include/include_global.glsl>
        #include <buffer/shape_chunk/src/include_shape.glsl>

        // Scene info
        layout(push_constant) uniform PushConstantData {
            mat4 view;
            mat4 projection;
        } pc;

        // Per shape input
        const uint MAX_XFORM_ID = 32;
        layout(set = 3, binding = 0) buffer ChunkBaseTransforms {
            float data[];
        } shape_transforms;
        layout(set = 3, binding = 1) buffer ChunkFlags {
            uint data[];
        } shape_flags;
        layout(set = 3, binding = 2) buffer ChunkXforms {
            float data[];
        } shape_xforms;
        layout(set = 3, binding = 3) buffer ChunkXformOffsets {
            uint data[];
        } shape_xform_offsets;

        // Per Vertex input
        layout(location = 0) in vec3 position;
        layout(location = 1) in vec4 color;
        layout(location = 2) in vec2 tex_coord;
        layout(location = 3) in uint flags0;
        layout(location = 4) in uint flags1;
        layout(location = 5) in uint xform_id;

        layout(location = 0) smooth out vec4 v_color;
        layout(location = 1) smooth out vec2 v_tex_coord;
        layout(location = 2) flat out uint f_flags0;
        layout(location = 3) flat out uint f_flags1;

        void main() {
            uint base_transform = gl_InstanceIndex * 6;
            uint base_flag = gl_InstanceIndex * 2;
            uint base_xform = shape_xform_offsets.data[gl_InstanceIndex];

            float transform[6] = {
                shape_transforms.data[base_transform + 0],
                shape_transforms.data[base_transform + 1],
                shape_transforms.data[base_transform + 2],
                shape_transforms.data[base_transform + 3],
                shape_transforms.data[base_transform + 4],
                shape_transforms.data[base_transform + 5]
            };
            float xform[6] = {0, 0, 0, 0, 0, 0};
            if (xform_id < MAX_XFORM_ID) {
                xform[0] = shape_xforms.data[base_xform + 6 * xform_id + 0];
                xform[1] = shape_xforms.data[base_xform + 6 * xform_id + 1];
                xform[2] = shape_xforms.data[base_xform + 6 * xform_id + 2];
                xform[3] = shape_xforms.data[base_xform + 6 * xform_id + 3];
                xform[4] = shape_xforms.data[base_xform + 6 * xform_id + 4];
                xform[5] = shape_xforms.data[base_xform + 6 * xform_id + 5];
            }

            gl_Position = pc.projection * pc.view * matrix_for_xform(transform) * matrix_for_xform(xform) * vec4(position, 1.0);
            v_color = color;
            v_tex_coord = tex_coord;

            f_flags0 = flags0 & shape_flags.data[base_flag + 0];
            f_flags1 = flags1 & shape_flags.data[base_flag + 1];
        }"
    }
}

mod fs {
    use vulkano_shaders::shader;

    shader! {
    ty: "fragment",
    include: ["./libs/render"],
    src: "
        #version 450

        layout(location = 0) smooth in vec4 v_color;
        layout(location = 1) smooth in vec2 v_tex_coord;
        layout(location = 2) flat in uint f_flags0;
        layout(location = 3) flat in uint f_flags1;

        layout(location = 0) out vec4 f_color;

        layout(set = 4, binding = 0) uniform sampler2DArray mega_atlas;
        //layout(set = 5, binding = 1) uniform sampler2DArray nose_art; NOSE\\d\\d.PIC
        //layout(set = 5, binding = 2) uniform sampler2DArray left_tail_art; LEFT\\d\\d.PIC
        //layout(set = 5, binding = 3) uniform sampler2DArray right_tail_art; RIGHT\\d\\d.PIC
        //layout(set = 5, binding = 4) uniform sampler2DArray round_art; ROUND\\d\\d.PIC

        void main() {
            if ((f_flags0 & 0xFFFFFFFE) == 0 && f_flags1 == 0) {
                discard;
            } else if (v_tex_coord.x == 0.0) {
                f_color = v_color;
            } else {
                vec4 tex_color = texture(mega_atlas, vec3(v_tex_coord, 0));

                if ((f_flags0 & 1) == 1) {
                    f_color = vec4((1.0 - tex_color[3]) * v_color.xyz + tex_color[3] * tex_color.xyz, 1.0);
                } else {
                    if (tex_color.a < 0.5)
                        discard;
                    else
                        f_color = tex_color;
                }
            }
        }"
    }
}

impl vs::ty::PushConstantData {
    fn new() -> Self {
        Self {
            view: [
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
            ],
            projection: [
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
            ],
        }
    }

    fn set_view(&mut self, mat: &Matrix4<f32>) {
        self.view[0][0] = mat[0];
        self.view[0][1] = mat[1];
        self.view[0][2] = mat[2];
        self.view[0][3] = mat[3];
        self.view[1][0] = mat[4];
        self.view[1][1] = mat[5];
        self.view[1][2] = mat[6];
        self.view[1][3] = mat[7];
        self.view[2][0] = mat[8];
        self.view[2][1] = mat[9];
        self.view[2][2] = mat[10];
        self.view[2][3] = mat[11];
        self.view[3][0] = mat[12];
        self.view[3][1] = mat[13];
        self.view[3][2] = mat[14];
        self.view[3][3] = mat[15];
    }

    fn set_projection(&mut self, mat: &Matrix4<f32>) {
        self.projection[0][0] = mat[0];
        self.projection[0][1] = mat[1];
        self.projection[0][2] = mat[2];
        self.projection[0][3] = mat[3];
        self.projection[1][0] = mat[4];
        self.projection[1][1] = mat[5];
        self.projection[1][2] = mat[6];
        self.projection[1][3] = mat[7];
        self.projection[2][0] = mat[8];
        self.projection[2][1] = mat[9];
        self.projection[2][2] = mat[10];
        self.projection[2][3] = mat[11];
        self.projection[3][0] = mat[12];
        self.projection[3][1] = mat[13];
        self.projection[3][2] = mat[14];
        self.projection[3][3] = mat[15];
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct BlockIndex(usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SlotIndex(usize);

const BLOCK_SIZE: usize = 128;

// Fixed reservation blocks for upload of a number of entities. Unfortunately, because of
// xforms, we don't know exactly how many instances will fit in any given block.
pub struct DynamicInstanceBlock {
    // Weak reference to the associated chunk in the Manager.
    chunk_index: ChunkIndex,
    //chunk_type: ChunkType,

    // Map from the entity to the stored offset and from the offset to the entity.
    slot_reservations: [Option<EntityId>; BLOCK_SIZE],
    entity_to_slot_map: HashMap<EntityId, SlotIndex>,
    mark_buffer: [bool; BLOCK_SIZE], // GC marked set

    descriptor_set: Arc<dyn DescriptorSet + Send + Sync>,

    // FIXME / BUG: most of these will be passed in; it crashes vulkano if we create empty sets
    // FIXME / BUG: before our real sets, however, so we have to push these down for now.
    pds0: Arc<dyn DescriptorSet + Send + Sync>,
    pds1: Arc<dyn DescriptorSet + Send + Sync>,
    pds2: Arc<dyn DescriptorSet + Send + Sync>,

    // Buffers for all instances stored in this instance set. One command per unique entity.
    // 16 bytes per entity; index unnecessary for draw
    command_buffer_scratch: [DrawIndirectCommand; BLOCK_SIZE],
    command_buffer_pool: CpuBufferPool<DrawIndirectCommand>,
    command_buffer: Arc<DeviceLocalBuffer<[DrawIndirectCommand]>>,

    // Base position and orientation in xyz+euler angles stored as 6 adjacent floats.
    // 24 bytes per entity; buffer index inferable from drawing index
    transform_buffer_scratch: [[f32; 6]; BLOCK_SIZE],
    transform_buffer_pool: CpuBufferPool<[f32; 6]>,
    transform_buffer: Arc<DeviceLocalBuffer<[[f32; 6]]>>,

    // 2 32bit flags words for each entity.
    // 8 bytes per entity; buffer index inferable from drawing index
    flag_buffer_scratch: [[u32; 2]; BLOCK_SIZE],
    flag_buffer_pool: CpuBufferPool<[u32; 2]>,
    flag_buffer: Arc<DeviceLocalBuffer<[[u32; 2]]>>,

    // 4 bytes per entity; can infer position from index
    xform_index_buffer: Arc<DeviceLocalBuffer<[i32; BLOCK_SIZE]>>,
    xform_index_buffer_pool: CpuBufferPool<[i32; BLOCK_SIZE]>,

    // 0 to 14 position/orientation [f32; 6], depending on the shape.
    // assume 96 bytes per entity if we're talking about planes
    // cannot infer position, so needs an index buffer
    xform_buffer: Arc<DeviceLocalBuffer<[[f32; 6]; 14 * BLOCK_SIZE]>>,
    xform_buffer_pool: CpuBufferPool<[[f32; 6]; 14 * BLOCK_SIZE]>,
}

impl DynamicInstanceBlock {
    fn new(
        chunk_index: ChunkIndex,
        pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
        command_buffer_pool: CpuBufferPool<DrawIndirectCommand>,
        transform_buffer_pool: CpuBufferPool<[f32; 6]>,
        flag_buffer_pool: CpuBufferPool<[u32; 2]>,
        xform_index_buffer_pool: CpuBufferPool<[i32; BLOCK_SIZE]>,
        xform_buffer_pool: CpuBufferPool<[[f32; 6]; 14 * BLOCK_SIZE]>,
        device: Arc<Device>,
    ) -> Fallible<Self> {
        let command_buffer = DeviceLocalBuffer::array(
            device.clone(),
            BLOCK_SIZE,
            BufferUsage::all(),
            device.active_queue_families(),
        )?;
        let transform_buffer = DeviceLocalBuffer::array(
            device.clone(),
            BLOCK_SIZE,
            BufferUsage::all(),
            device.active_queue_families(),
        )?;
        let flag_buffer = DeviceLocalBuffer::array(
            device.clone(),
            BLOCK_SIZE,
            BufferUsage::all(),
            device.active_queue_families(),
        )?;
        let xform_index_buffer = DeviceLocalBuffer::new(
            device.clone(),
            BufferUsage::all(),
            device.active_queue_families(),
        )?;
        let xform_buffer = DeviceLocalBuffer::new(
            device.clone(),
            BufferUsage::all(),
            device.active_queue_families(),
        )?;
        let descriptor_set = Arc::new(
            PersistentDescriptorSet::start(pipeline.clone(), GlobalSets::ShapeBuffers.into())
                .add_buffer(transform_buffer.clone())?
                .add_buffer(flag_buffer.clone())?
                .add_buffer(xform_buffer.clone())?
                .add_buffer(xform_index_buffer.clone())?
                .build()?,
        );
        Ok(Self {
            chunk_index,
            slot_reservations: [None; BLOCK_SIZE],
            entity_to_slot_map: HashMap::new(),
            mark_buffer: [false; BLOCK_SIZE],
            descriptor_set,
            pds0: GraphicsWindow::empty_descriptor_set(pipeline.clone(), 0)?,
            pds1: GraphicsWindow::empty_descriptor_set(pipeline.clone(), 1)?,
            pds2: GraphicsWindow::empty_descriptor_set(pipeline.clone(), 2)?,
            command_buffer_scratch: [DrawIndirectCommand {
                vertex_count: 0u32,
                instance_count: 0u32,
                first_vertex: 0u32,
                first_instance: 0u32,
            }; BLOCK_SIZE],
            command_buffer_pool,
            command_buffer,
            transform_buffer_scratch: [[0f32; 6]; BLOCK_SIZE],
            transform_buffer_pool,
            transform_buffer,
            flag_buffer_scratch: [[0u32; 2]; BLOCK_SIZE],
            flag_buffer_pool,
            flag_buffer,
            xform_index_buffer,
            xform_index_buffer_pool,
            xform_buffer,
            xform_buffer_pool,
        })
    }

    fn reserve_slot_for(&mut self, slot: SlotIndex, id: EntityId) {
        self.slot_reservations[slot.0] = Some(id);
        self.entity_to_slot_map.insert(id, slot);
        /*
        let foo = &mut self.command_buffer_scratch[slot.0];
        foo.vertex_count = 10;
        foo.instance_count = 1;
        */
    }

    fn find_free_slot(&self) -> Option<SlotIndex> {
        for (slot_offset, entity_id) in self.slot_reservations.iter().enumerate() {
            if entity_id.is_none() {
                return Some(SlotIndex(slot_offset));
            }
        }
        None
    }

    fn reserve_free_slot(&mut self, id: EntityId, chunk_index: ChunkIndex) -> Option<SlotIndex> {
        if chunk_index != self.chunk_index {
            return None;
        }
        let maybe_slot_index = self.find_free_slot();
        if let Some(slot_index) = maybe_slot_index {
            self.reserve_slot_for(slot_index, id);
        }
        maybe_slot_index
    }

    fn get_existing_slot(&mut self, id: EntityId) -> SlotIndex {
        *self.entity_to_slot_map.get(&id).unwrap()
    }

    fn get_command_buffer_slot(&mut self, slot_index: SlotIndex) -> &mut DrawIndirectCommand {
        &mut self.command_buffer_scratch[slot_index.0]
    }

    fn get_transform_buffer_slot(&mut self, slot_index: SlotIndex) -> &mut [f32; 6] {
        &mut self.transform_buffer_scratch[slot_index.0]
    }

    fn get_flag_buffer_slot(&mut self, slot_index: SlotIndex) -> &mut [u32; 2] {
        &mut self.flag_buffer_scratch[slot_index.0]
    }

    /*
    fn get_upload_buffer(&mut self, slot_index: SlotIndex) -> Fallible<()> {
        self.mark_buffer[slot_index.0] = true;

        Ok(())
    }
    */

    fn update_buffers(
        &self,
        mut cbb: AutoCommandBufferBuilder,
        pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
        chunk: &ClosedChunk,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let dic = self.command_buffer_scratch.to_vec();
        let command_buffer_upload = self.command_buffer_pool.chunk(dic)?;
        cbb = cbb.copy_buffer(command_buffer_upload, self.command_buffer.clone())?;

        let tr = self.transform_buffer_scratch.to_vec();
        let transform_buffer_upload = self.transform_buffer_pool.chunk(tr)?;
        cbb = cbb.copy_buffer(transform_buffer_upload, self.transform_buffer.clone())?;

        let fl = self.flag_buffer_scratch.to_vec();
        let flag_buffer_upload = self.flag_buffer_pool.chunk(fl)?;
        cbb = cbb.copy_buffer(flag_buffer_upload, self.flag_buffer.clone())?;

        Ok(cbb)
    }

    pub fn render(
        &self,
        mut cbb: AutoCommandBufferBuilder,
        pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
        chunk: &ClosedChunk,
        push_constants: &vs::ty::PushConstantData,
        camera: &dyn CameraAbstract,
        window: &GraphicsWindow,
        f18_part: &ChunkPart,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let mut local_push_constants = vs::ty::PushConstantData::new();
        local_push_constants.set_projection(&camera.projection_matrix());
        local_push_constants.set_view(&camera.view_matrix());

        let ib = self.command_buffer.clone();
        Ok(cbb.draw_indirect(
            pipeline.clone(),
            &window.dynamic_state,
            vec![chunk.vertex_buffer()],
            ib.into_buffer_slice().slice(0..1).unwrap(),
            (
                self.pds0.clone(),
                self.pds1.clone(),
                self.pds2.clone(),
                self.descriptor_set.clone(),
                chunk.atlas_descriptor_set_ref(),
            ),
            push_constants,
        )?)
    }
}

pub struct ShapeRenderer {
    device: Arc<Device>,
    world: Arc<World>,
    pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,

    // TODO: push mutability down further -- we'd like to parallelize upload, but in practice we
    // TODO: can currently push all shapes into chunks in under a second, so it may not matter.
    chunks: ShapeChunkManager,

    // All upload blocks. We will do one draw call per instance block each frame.
    blocks: Vec<DynamicInstanceBlock>,

    // Map from the index to the block that it has a reserved upload slot in.
    upload_block_map: HashMap<EntityId, BlockIndex>,

    // FIXME: We need to move our empty and atmosphere descriptor sets here, but vulkano is bugged
    // FIXME: and won't let us create empty sets before our filled sets, so we've pushed these down.
    //    pds0: Arc<dyn DescriptorSet + Send + Sync>,
    //    pds1: Arc<dyn DescriptorSet + Send + Sync>,
    //    pds2: Arc<dyn DescriptorSet + Send + Sync>,

    // Buffer pools are shared by all blocks for maximum re-use.
    command_buffer_pool: CpuBufferPool<DrawIndirectCommand>,
    transform_buffer_pool: CpuBufferPool<[f32; 6]>,
    flag_buffer_pool: CpuBufferPool<[u32; 2]>,
    xform_index_buffer_pool: CpuBufferPool<[i32; BLOCK_SIZE]>,
    xform_buffer_pool: CpuBufferPool<[[f32; 6]; 14 * BLOCK_SIZE]>, // FIXME: hunt down this max somewhere
}

impl ShapeRenderer {
    pub fn new(world: Arc<World>, window: &GraphicsWindow) -> Fallible<Self> {
        let pipeline = Self::build_pipeline(&window)?;
        let chunks = ShapeChunkManager::new(pipeline.clone(), &window)?;
        Ok(Self {
            device: window.device(),
            world,
            pipeline,
            chunks,
            blocks: Vec::new(),
            upload_block_map: HashMap::new(),
            command_buffer_pool: CpuBufferPool::new(window.device(), BufferUsage::all()),
            transform_buffer_pool: CpuBufferPool::new(window.device(), BufferUsage::all()),
            flag_buffer_pool: CpuBufferPool::new(window.device(), BufferUsage::all()),
            xform_index_buffer_pool: CpuBufferPool::new(window.device(), BufferUsage::all()),
            xform_buffer_pool: CpuBufferPool::new(window.device(), BufferUsage::all()),
        })
    }

    fn build_pipeline(
        window: &GraphicsWindow,
    ) -> Fallible<Arc<dyn GraphicsPipelineAbstract + Send + Sync>> {
        let vert_shader = vs::Shader::load(window.device())?;
        let frag_shader = fs::Shader::load(window.device())?;
        Ok(Arc::new(
            GraphicsPipeline::start()
                .vertex_input_single_buffer::<Vertex>()
                .vertex_shader(vert_shader.main_entry_point(), ())
                .triangle_list()
                .cull_mode_back()
                .front_face_clockwise()
                .viewports_dynamic_scissors_irrelevant(1)
                .fragment_shader(frag_shader.main_entry_point(), ())
                .depth_stencil(DepthStencil {
                    depth_write: true,
                    depth_compare: Compare::GreaterOrEqual,
                    depth_bounds_test: DepthBounds::Disabled,
                    stencil_front: Default::default(),
                    stencil_back: Default::default(),
                })
                .blend_alpha_blending()
                .render_pass(
                    Subpass::from(window.render_pass(), 0)
                        .expect("gfx: did not find a render pass"),
                )
                .build(window.device())?,
        ) as Arc<dyn GraphicsPipelineAbstract + Send + Sync>)
    }

    pub fn pipeline(&self) -> Arc<dyn GraphicsPipelineAbstract + Send + Sync> {
        self.pipeline.clone()
    }

    pub fn upload_shape(
        &mut self,
        name: &str,
        selection: DrawSelection,
        window: &GraphicsWindow,
    ) -> Fallible<(ShapeId, Option<Box<dyn GpuFuture>>)> {
        self.chunks.upload_shape(
            name,
            selection,
            self.world.system_palette(),
            self.world.library(),
            window,
        )
    }

    // Close any outstanding chunks and prepare to render.
    pub fn ensure_uploaded(&mut self, window: &GraphicsWindow) -> Fallible<Box<dyn GpuFuture>> {
        self.chunks.finish(window)
    }

    // First fit: find the first block with a free upload slot.
    fn reserve_free_slot(
        &mut self,
        id: EntityId,
        shape_id: ShapeId,
    ) -> Fallible<(BlockIndex, SlotIndex)> {
        let chunk_index = self.chunks.find_chunk_for_shape(shape_id)?;

        // Note that we do not bother sorting blocks by chunk because we only have to care about
        // that mapping when adding new entries. We do a simple chunk_id check to filter out
        // non-matching blocks. The assumption is that we will have few enough chunks that a large
        // fraction of blocks will be relevant, usually.
        for (block_index, block) in self.blocks.iter_mut().enumerate() {
            if let Some(slot_index) = block.reserve_free_slot(id, chunk_index) {
                return Ok((BlockIndex(block_index), slot_index));
            }
        }

        // No free slots in any blocks. Build a new one.
        let next_block_index = BlockIndex(self.blocks.len());
        let mut block = DynamicInstanceBlock::new(
            chunk_index,
            self.pipeline.clone(),
            self.command_buffer_pool.clone(),
            self.transform_buffer_pool.clone(),
            self.flag_buffer_pool.clone(),
            self.xform_index_buffer_pool.clone(),
            self.xform_buffer_pool.clone(),
            self.device.clone(),
        )?;
        let slot_index = block.reserve_free_slot(id, chunk_index).unwrap();
        self.blocks.push(block);
        self.upload_block_map.insert(id, next_block_index);
        Ok((next_block_index, slot_index))
    }

    pub fn ensure_entity_slot(
        &mut self,
        id: EntityId,
        shape_id: ShapeId,
    ) -> Fallible<(BlockIndex, SlotIndex)> {
        // Fast path: in most cases we'll already have a block.
        if let Some(&block_index) = self.upload_block_map.get(&id) {
            let slot_index = self.blocks[block_index.0].get_existing_slot(id);
            return Ok((block_index, slot_index));
        }

        self.reserve_free_slot(id, shape_id)
    }

    pub fn chunks(&self) -> &ShapeChunkManager {
        &self.chunks
    }

    pub fn blocks(&self) -> &Vec<DynamicInstanceBlock> {
        &self.blocks
    }

    fn get_chunk_for_slot(&self, index: (BlockIndex, SlotIndex)) -> &ClosedChunk {
        let (block_index, _) = index;
        let block = &self.blocks[block_index.0];
        self.chunks.at(block.chunk_index)
    }

    fn get_command_buffer_slot(
        &mut self,
        index: (BlockIndex, SlotIndex),
    ) -> &mut DrawIndirectCommand {
        let (block_index, slot_index) = index;
        self.blocks[block_index.0].get_command_buffer_slot(slot_index)
    }

    fn get_transform_buffer_slot(&mut self, index: (BlockIndex, SlotIndex)) -> &mut [f32; 6] {
        let (block_index, slot_index) = index;
        self.blocks[block_index.0].get_transform_buffer_slot(slot_index)
    }

    fn get_flag_buffer_slot(&mut self, index: (BlockIndex, SlotIndex)) -> &mut [u32; 2] {
        let (block_index, slot_index) = index;
        self.blocks[block_index.0].get_flag_buffer_slot(slot_index)
    }

    pub fn update_buffers(
        &self,
        mut cbb: AutoCommandBufferBuilder,
    ) -> Fallible<AutoCommandBufferBuilder> {
        for block in self.blocks.iter() {
            let chunk = self.chunks.get_chunk(block.chunk_index);
            cbb = block.update_buffers(cbb, self.pipeline(), &chunk)?;
        }
        Ok(cbb)
    }

    pub fn render(
        &self,
        mut cbb: AutoCommandBufferBuilder,
        camera: &dyn CameraAbstract,
        window: &GraphicsWindow,
        f18_part: &ChunkPart,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let mut push_constants = vs::ty::PushConstantData::new();
        push_constants.set_projection(&camera.projection_matrix());
        push_constants.set_view(&camera.view_matrix());

        let chunk_man = &self.chunks;
        for block in self.blocks.iter() {
            let chunk = chunk_man.get_chunk(block.chunk_index);
            println!("at chunk: {:?}", block.chunk_index);
            cbb = block.render(
                cbb,
                self.pipeline(),
                &chunk,
                &push_constants,
                camera,
                window,
                f18_part,
            )?;
        }
        Ok(cbb)
    }
}

pub struct ShapeRenderSystem<'b> {
    renderer: &'b mut ShapeRenderer,
}

impl<'b> ShapeRenderSystem<'b> {
    pub fn new(renderer: &'b mut ShapeRenderer) -> Self {
        Self { renderer }
    }
}

impl<'a, 'b> System<'a> for ShapeRenderSystem<'b> {
    // These are the resources required for execution.
    // You can also define a struct and `#[derive(SystemData)]`,
    // see the `full` example.
    type SystemData = (
        Entities<'a>,
        ReadStorage<'a, Transform>,
        ReadStorage<'a, ShapeMesh>,
    );

    fn run(&mut self, (entities, transform, shape_mesh): Self::SystemData) {
        for (entity, transform, shape_mesh) in (&entities, &transform, &shape_mesh).join() {
            let index = self
                .renderer
                .ensure_entity_slot(entity.id(), shape_mesh.shape_id())
                .expect("unable to reserve instance slot");

            // Push all.
            let chunk = self.renderer.get_chunk_for_slot(index);
            let chunk_part = chunk.part(shape_mesh.shape_id()).unwrap();
            let errata = chunk_part.widgets().errata();
            *self.renderer.get_command_buffer_slot(index) = chunk_part.draw_command(0, 1);
            *self.renderer.get_transform_buffer_slot(index) = [0f32; 6];
            let flag_slot = self.renderer.get_flag_buffer_slot(index);

            // FIXME: get time start somehow
            shape_mesh
                .draw_state()
                .build_mask_into(&Instant::now(), errata, flag_slot);

            //let foo = self.acquire_upload_buffers(block_index, slot_index);
            //self.blocks[block_index];
            //println!("{:?} => block_index: {:?}", entity.id(), block_index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulkano::pipeline::GraphicsPipeline;
    use window::GraphicsConfigBuilder;
    use world::World;

    #[test]
    fn it_works() -> Fallible<()> {
        let omni = OmniLib::new_for_test_in_games(&["FA"])?;

        let window = GraphicsWindow::new(&GraphicsConfigBuilder::new().build())?;
        let lib = omni.library("FA");

        let world = Arc::new(World::new(lib)?);

        let mut shape_renderer = ShapeRenderer::new(world.clone(), &window)?;
        let (t80_id, fut1) =
            shape_renderer.upload_shape("T80.SH", DrawSelection::NormalModel, &window)?;
        let future = shape_renderer.ensure_uploaded(&window)?;
        future.then_signal_fence_and_flush()?.wait(None)?;

        let t80_ent1 = world.create_ground_mover(t80_id, Point3::new(0f64, 0f64, 0f64))?;

        let shape_render_system = ShapeRenderSystem::new(&mut shape_renderer);
        let mut dispatcher = DispatcherBuilder::new()
            .with(shape_render_system, "", &[])
            .build();

        world.run(&mut dispatcher);
        let t80_ent2 = world.create_ground_mover(t80_id, Point3::new(0f64, 0f64, 0f64))?;
        world.run(&mut dispatcher);
        let t80_ent3 = world.create_ground_mover(t80_id, Point3::new(0f64, 0f64, 0f64))?;
        world.run(&mut dispatcher);

        world.destroy_entity(t80_ent2)?;
        world.run(&mut dispatcher);
        world.destroy_entity(t80_ent1)?;
        world.run(&mut dispatcher);

        Ok(())
    }
}

/*
// Types of data we want to be able to deal with.
//
// Static Immortal:
//   CommandBuf: [ Name1(0...N), Name2(0...M), ...]
//   BaseBuffer: [A, A, A, ... A{N}, B, B, B, ... B{M}]; A/B: [f32; 6]
//   FlagsBuffer: []
//   XFormBuffer: []
//
// We need to accumulate before uploading the command buffer, which means we need to be
// careful with the order in BaseBuffer. Assert that there are no xforms or flags on any of these.
// How much can we simplify the renderer if we know there are no xforms?
//
// Xforms vs no xforms -- most shapes have no xforms, even if they can be destroyed, or
// move around and be destroyed. How much can we simplify the renderer if we don't have
// xforms? Probably quite a bit. Is it worth having two pipelines? Benchmark to figure out
// how many fully dynamic shapes we can have.
//
// Fully dynamic:
//   CommandBuf: [ E0, E1, E2, E3, ... EN ]  <- updated on add/remove entity (as are all)
//   BaseBuffer: [ B0, B1, B2, B3, ... BN ]  <- updated every frame for movers, never for static
//   FlagsBuffer: [ F0, F1, F2, F3, ... FN ] <- updated occasionally
//   XformBuffer: [ X0..M, X0...L, X0...H ... X0...I ] <- updated every frame for some things
//
// Implement fullest feature set first. If we can render a million SOLDIER.SH, we can easily
// render a million TREE.SH.

pub struct OpenChunkInstance {
    open_chunk: OpenChunk,
    command_buf: Vec<Entity>,
    base_buffer: Vec<Matrix4<f32>>,
    flags_buffer: Vec<[u32; 2]>,
}

pub struct InstanceSet {
    // Offset of the chunk these instances draw from.
    chunk_reference: usize,

    // Buffers for all instances stored in this instance set. One command per unique entity.
    // 16 bytes per entity; index unnecessary for draw
    command_buf: CpuAccessibleBuffer<[DrawIndirectCommand]>,

    // Base position and orientation in xyz+euler angles stored as 6 adjacent floats.
    // 24 bytes per entity; buffer index inferable from drawing index
    base_buffer: CpuAccessibleBuffer<[f32]>, // Flags buffers

    // 2 32bit flags words for each entity.
    // 8 bytes per entity; buffer index inferable from drawing index
    flags_buffer: CpuAccessibleBuffer<[u32]>,

    // 0 to 14 position/orientation [f32; 6], depending on the shape.
    // assume 240 bytes per entity if we're talking about planes
    // cannot infer position, so needs an index buffer
    xform_buffer: CpuAccessibleBuffer<[f32]>,

    // 4 bytes per entity; can infer position from index
    xform_index_buffer: CpuAccessibleBuffer<[i32]>,
    //
    // Total cost per entity is: 16 + 24 + 8 + 240 + 4 ~ 300 bytes per entity
    // We cannot really upload more than 1MiB per frame, so... ~3000 planes
}
*/
