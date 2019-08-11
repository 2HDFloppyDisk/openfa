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
pub mod buffer;
mod draw_state;
mod texture_atlas;

use crate::{
    buffer::{DrawSelection, ShapeBufferRef, ShapesBuffer, Vertex},
    draw_state::DrawState,
};
use camera::CameraAbstract;
use failure::Fallible;
use lib::Library;
use log::trace;
use nalgebra::{Matrix4, Vector3};
use pal::Palette;
use sh::RawShape;
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Instant};
use vulkano::{
    buffer::{BufferUsage, CpuBufferPool, DeviceLocalBuffer},
    command_buffer::{AutoCommandBufferBuilder, DynamicState},
    framebuffer::Subpass,
    pipeline::{
        depth_stencil::{Compare, DepthBounds, DepthStencil},
        GraphicsPipeline, GraphicsPipelineAbstract,
    },
};
use window::GraphicsWindow;

// NOTE: if updating MAX_UNIFORMS, the same constant must be fixed up in the vertex shader.
const MAX_XFORMS: usize = 14;
const UNIFORM_POOL_SIZE: usize = MAX_XFORMS * 6;

mod vs {
    use vulkano_shaders::shader;

    shader! {
    ty: "vertex",
    include: ["./libs/render"],
    src: "
        #version 450

        layout(location = 0) in vec3 position;
        layout(location = 1) in vec4 color;
        layout(location = 2) in vec2 tex_coord;
        layout(location = 3) in uint flags0;
        layout(location = 4) in uint flags1;
        layout(location = 5) in uint xform_id;

        layout(binding = 1) buffer MatrixArray {
            float xform_data[];
        } ma;


        layout(push_constant) uniform PushConstantData {
          mat4 view;
          mat4 projection;
          uint flag_mask0;
          uint flag_mask1;
        } pc;

        #include <common/include/include_global.glsl>
        #include <draw/shape/src/include_shape.glsl>

        layout(location = 0) smooth out vec4 v_color;
        layout(location = 1) smooth out vec2 v_tex_coord;
        layout(location = 2) flat out uint f_flags0;
        layout(location = 3) flat out uint f_flags1;

        void main() {
            gl_Position = pc.projection * pc.view * matrix_for_xform(xform_id) * vec4(position, 1.0);
            v_color = color;
            v_tex_coord = tex_coord;
            f_flags0 = flags0 & pc.flag_mask0;
            f_flags1 = flags1 & pc.flag_mask1;
        }"
    }
}

mod fs {
    use vulkano_shaders::shader;

    shader! {
    ty: "fragment",
    src: "
        #version 450

        layout(location = 0) smooth in vec4 v_color;
        layout(location = 1) smooth in vec2 v_tex_coord;
        layout(location = 2) flat in uint f_flags0;
        layout(location = 3) flat in uint f_flags1;

        layout(location = 0) out vec4 f_color;

        layout(set = 0, binding = 0) uniform sampler2D tex;

        void main() {
            if ((f_flags0 & 0xFFFFFFFE) == 0 && f_flags1 == 0) {
                discard;
            } else if (v_tex_coord.x == 0.0) {
                f_color = v_color;
            } else {
                vec4 tex_color = texture(tex, v_tex_coord);

                if ((f_flags0 & 1) == 1) {
                    f_color = vec4((1.0 - tex_color[3]) * v_color.xyz + tex_color[3] * tex_color.xyz, 1.0);
                } else {
                    if (tex_color.a < 0.5)
                        discard;
                    else
                        f_color = tex_color;
                }
            }
        }
        "
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
            flag_mask0: 0xFFFF_FFFF,
            flag_mask1: 0xFFFF_FFFF,
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

    pub fn set_mask(&mut self, mask: u64) {
        self.flag_mask0 = (mask & 0xFFFF_FFFF) as u32;
        self.flag_mask1 = (mask >> 32) as u32;
    }
}

pub struct ShapeInstance {
    buffer: ShapeBufferRef,

    transform_states: HashMap<u32, [f32; 6]>,

    draw_state: Rc<RefCell<DrawState>>,
}

impl ShapeInstance {
    fn new(buffer: ShapeBufferRef) -> Fallible<Self> {
        Ok(Self {
            buffer,
            transform_states: HashMap::new(),
            draw_state: Default::default(),
        })
    }

    fn animate(&mut self, start: &Instant, now: &Instant) -> Fallible<()> {
        self.transform_states =
            self.buffer
                .apply_animation(&self.draw_state.borrow(), start, now)?;
        Ok(())
    }

    fn before_render(&self, uniforms: &mut [f32; UNIFORM_POOL_SIZE], uniforms_offset: &mut usize) {
        assert_eq!(*uniforms_offset, 0);
        const COUNT: usize = 6;
        for (xform_id, xform) in self.transform_states.iter() {
            let xform_id = *xform_id as usize;
            for i in 0..COUNT {
                uniforms[*uniforms_offset + (COUNT * xform_id) + i] = xform[i];
            }
        }
        *uniforms_offset += UNIFORM_POOL_SIZE;
    }

    fn render(
        &self,
        pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
        camera: &CameraAbstract,
        cb: AutoCommandBufferBuilder,
        dynamic_state: &DynamicState,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let transform = Matrix4::new_translation(&Vector3::new(6_378_000.0 + 1000.0, 0.0, 0.0));
        let mut push_consts = vs::ty::PushConstantData::new();
        push_consts.set_projection(&camera.projection_matrix());
        push_consts.set_view(&(camera.view_matrix() * transform));
        push_consts.set_mask(
            self.draw_state
                .borrow()
                .build_mask(self.draw_state.borrow().time_origin(), self.buffer.errata())?,
        );

        Ok(cb.draw_indexed(
            pipeline,
            dynamic_state,
            vec![self.buffer.vertex_buffer_ref()],
            self.buffer.index_buffer_ref(),
            self.buffer.descriptor_set_ref(),
            push_consts,
        )?)
    }
}

#[derive(Clone)]
pub struct ShapeInstanceRef {
    value: Arc<RefCell<ShapeInstance>>,
}

impl ShapeInstanceRef {
    pub fn new(instance: ShapeInstance) -> Self {
        ShapeInstanceRef {
            value: Arc::new(RefCell::new(instance)),
        }
    }

    pub fn draw_state(&self) -> Rc<RefCell<DrawState>> {
        self.value.borrow().draw_state.clone()
    }

    pub fn animate(&mut self, start: &Instant, now: &Instant) -> Fallible<()> {
        {
            self.value
                .borrow_mut()
                .draw_state
                .borrow_mut()
                .animate(start, now);
        }
        self.value.borrow_mut().animate(start, now)?;
        Ok(())
    }

    fn before_render(&self, uniforms: &mut [f32; UNIFORM_POOL_SIZE], uniforms_offset: &mut usize) {
        self.value.borrow().before_render(uniforms, uniforms_offset)
    }

    fn render(
        &self,
        pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
        camera: &CameraAbstract,
        cb: AutoCommandBufferBuilder,
        dynamic_state: &DynamicState,
    ) -> Fallible<AutoCommandBufferBuilder> {
        self.value
            .borrow()
            .render(pipeline, camera, cb, dynamic_state)
    }
}

pub struct ShapeRenderer {
    start: Instant,
    pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,

    uniform_upload_pool: Arc<CpuBufferPool<[f32; UNIFORM_POOL_SIZE]>>,
    device_uniform_buffer: Arc<DeviceLocalBuffer<[f32; UNIFORM_POOL_SIZE]>>,
    buffers: ShapesBuffer,

    last_instance_handle: usize,
    instances: HashMap<usize, ShapeInstanceRef>,
}

impl ShapeRenderer {
    pub fn new(window: &GraphicsWindow) -> Fallible<Self> {
        trace!("ShapeRenderer::new");

        let vs = vs::Shader::load(window.device())?;
        let fs = fs::Shader::load(window.device())?;

        let pipeline = Arc::new(
            GraphicsPipeline::start()
                .vertex_input_single_buffer::<Vertex>()
                .vertex_shader(vs.main_entry_point(), ())
                .triangle_list()
                .cull_mode_back()
                .front_face_clockwise()
                .viewports_dynamic_scissors_irrelevant(1)
                .fragment_shader(fs.main_entry_point(), ())
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
        );

        let device_uniform_buffer = DeviceLocalBuffer::new(
            window.device(),
            BufferUsage::all(),
            window.device().active_queue_families(),
        )?;

        let uniform_upload_pool = Arc::new(CpuBufferPool::upload(window.device()));

        let buffers = ShapesBuffer::new(pipeline.clone())?;

        Ok(Self {
            start: Instant::now(),
            pipeline,
            device_uniform_buffer,
            uniform_upload_pool,
            buffers,
            last_instance_handle: 0,
            instances: HashMap::new(),
        })
    }

    pub fn add_shape_to_render(
        &mut self,
        name: &str,
        sh: &RawShape,
        selection: DrawSelection,
        palette: &Palette,
        lib: &Library,
        window: &GraphicsWindow,
    ) -> Fallible<ShapeInstanceRef> {
        let buffer = self.buffers.upload_shape(
            name,
            sh,
            selection,
            self.device_uniform_buffer.clone(),
            palette,
            lib,
            window,
        )?;
        let instance = ShapeInstance::new(buffer)?;
        let instance_ref = ShapeInstanceRef::new(instance);
        let instance_handle = self.last_instance_handle + 1;
        self.last_instance_handle += 1;
        self.instances.insert(instance_handle, instance_ref.clone());
        Ok(instance_ref)
    }

    pub fn animate(&mut self, now: &Instant) -> Fallible<()> {
        for instance in self.instances.values_mut() {
            instance.animate(&self.start, now)?;
        }
        Ok(())
    }

    pub fn before_render(
        &self,
        command_buffer: AutoCommandBufferBuilder,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let mut uniforms = [0f32; UNIFORM_POOL_SIZE];
        let mut uniforms_offset = 0;
        for instance in self.instances.values() {
            instance.before_render(&mut uniforms, &mut uniforms_offset);
        }
        let new_uniforms_buffer = self.uniform_upload_pool.next(uniforms)?;
        Ok(command_buffer.copy_buffer(new_uniforms_buffer, self.device_uniform_buffer.clone())?)
    }

    pub fn render(
        &self,
        camera: &CameraAbstract,
        command_buffer: AutoCommandBufferBuilder,
        dynamic_state: &DynamicState,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let mut cb = command_buffer;
        for instance in self.instances.values() {
            cb = instance.render(self.pipeline.clone(), camera, cb, dynamic_state)?;
        }
        Ok(cb)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use camera::ArcBallCamera;
    use failure::Error;
    use omnilib::OmniLib;
    use sh::RawShape;
    use std::{f64::consts::PI, rc::Rc};
    use window::GraphicsConfigBuilder;

    #[test]
    fn it_can_render_shapes() -> Fallible<()> {
        let mut window = GraphicsWindow::new(&GraphicsConfigBuilder::new().build())?;
        let mut camera = ArcBallCamera::new(window.aspect_ratio_f64()?, 0.1, 3.4e+38);
        camera.set_distance(100.);
        camera.set_angle(115. * PI / 180., -135. * PI / 180.);

        let omni = OmniLib::new_for_test_in_games(&[
            "USNF", "MF", "ATF", "ATFNATO", "ATFGOLD", "USNF97", "FA",
        ])?;
        let skipped = vec![
            "CHAFF.SH",
            "CRATER.SH",
            "DEBRIS.SH",
            "EXP.SH",
            "FIRE.SH",
            "FLARE.SH",
            "MOTHB.SH",
            "SMOKE.SH",
            "WAVE1.SH",
            "WAVE2.SH",
        ];
        for (game, lib) in omni.libraries() {
            let system_palette = Rc::new(Box::new(Palette::from_bytes(&lib.load("PALETTE.PAL")?)?));

            for name in &lib.find_matching("*.SH")? {
                if skipped.contains(&name.as_ref()) {
                    continue;
                }

                println!(
                    "At: {}:{:13} @ {}",
                    game,
                    name,
                    omni.path(&game, name)
                        .or_else::<Error, _>(|_| Ok("<none>".to_string()))?
                );

                let sh = RawShape::from_bytes(&lib.load(name)?)?;
                let mut sh_renderer = ShapeRenderer::new(&window)?;

                let sh_instance = sh_renderer.add_shape_to_render(
                    &(game.to_owned() + ":" + name),
                    &sh,
                    DrawSelection::NormalModel,
                    &system_palette,
                    &lib,
                    &window,
                )?;
                sh_instance
                    .draw_state()
                    .borrow_mut()
                    .toggle_gear(&Instant::now());
                sh_renderer.animate(&Instant::now())?;

                {
                    let frame = window.begin_frame()?;
                    if !frame.is_valid() {
                        continue;
                    }

                    let mut cbb = AutoCommandBufferBuilder::primary_one_time_submit(
                        window.device(),
                        window.queue().family(),
                    )?;

                    cbb = sh_renderer.before_render(cbb)?;

                    cbb = cbb.begin_render_pass(
                        frame.framebuffer(&window),
                        false,
                        vec![[0f32, 0f32, 1f32, 1f32].into(), 0f32.into()],
                    )?;

                    cbb = sh_renderer.render(&camera, cbb, &window.dynamic_state)?;

                    cbb = cbb.end_render_pass()?;

                    let cb = cbb.build()?;

                    frame.submit(cb, &mut window)?;
                }
            }
        }
        std::mem::drop(window);
        Ok(())
    }
}