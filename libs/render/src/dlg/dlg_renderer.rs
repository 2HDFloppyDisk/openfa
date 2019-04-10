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
use dlg::{Dialog, DrawAction, Widget};
use failure::Fallible;
use image::{ImageBuffer, Rgba};
use lib::Library;
use log::trace;
use nalgebra::{Matrix4, Vector3};
use pal::Palette;
use pic::Pic;
use std::sync::Arc;
use vulkano::{
    buffer::{BufferUsage, CpuAccessibleBuffer},
    command_buffer::{AutoCommandBufferBuilder, DynamicState},
    descriptor::descriptor_set::{DescriptorSet, PersistentDescriptorSet},
    device::Device,
    format::Format,
    framebuffer::Subpass,
    image::{Dimensions, ImmutableImage},
    impl_vertex,
    pipeline::{GraphicsPipeline, GraphicsPipelineAbstract},
    sampler::{Filter, MipmapMode, Sampler, SamplerAddressMode},
    sync::GpuFuture,
};
use window::GraphicsWindow;

#[derive(Copy, Clone)]
struct Vertex {
    position: [f32; 2],
    tex_coord: [f32; 2],
}

impl_vertex!(Vertex, position, tex_coord);

mod vs {
    use vulkano_shaders::shader;

    shader! {
    ty: "vertex",
        src: "
            #version 450

            layout(location = 0) in vec2 position;
            layout(location = 1) in vec2 tex_coord;

            layout(push_constant) uniform PushConstantData {
              mat4 projection;
            } pc;

            layout(location = 0) out vec2 v_tex_coord;

            void main() {
                gl_Position = pc.projection * vec4(position, 0.0, 1.0);
                v_tex_coord = tex_coord;
            }"
    }
}

mod fs {
    use vulkano_shaders::shader;

    shader! {
    ty: "fragment",
        src: "
            #version 450

            layout(location = 0) in vec2 v_tex_coord;

            layout(location = 0) out vec4 f_color;

            layout(set = 0, binding = 0) uniform sampler2D tex;

            void main() {
                f_color = texture(tex, v_tex_coord);
            }
            "
    }
}

impl vs::ty::PushConstantData {
    fn new() -> Self {
        Self {
            projection: [
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
                [0.0f32, 0.0f32, 0.0f32, 0.0f32],
            ],
        }
    }

    fn set_projection(&mut self, mat: Matrix4<f32>) {
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

pub struct QuadRenderer {
    pds: Arc<dyn DescriptorSet + Send + Sync>,
    vertex_buffer: Arc<CpuAccessibleBuffer<[Vertex]>>,
    index_buffer: Arc<CpuAccessibleBuffer<[u32]>>,
}

impl QuadRenderer {
    pub fn new(
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        img: &image::DynamicImage,
        pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
        window: &GraphicsWindow,
    ) -> Fallible<QuadRenderer> {
        // Compute vertices such that we can handle any aspect ratio, or set up the camera to handle this?
        let verts = vec![
            Vertex {
                position: [x0, y0],
                tex_coord: [0f32, 0f32],
            },
            Vertex {
                position: [x0, y1],
                tex_coord: [0f32, 1f32],
            },
            Vertex {
                position: [x1, y0],
                tex_coord: [1f32, 0f32],
            },
            Vertex {
                position: [x1, y1],
                tex_coord: [1f32, 1f32],
            },
        ];
        let indices = vec![0u32, 1u32, 2u32, 3u32];

        trace!(
            "uploading vertex buffer with {} bytes",
            std::mem::size_of::<Vertex>() * verts.len()
        );
        let vertex_buffer =
            CpuAccessibleBuffer::from_iter(window.device(), BufferUsage::all(), verts.into_iter())?;

        trace!(
            "uploading index buffer with {} bytes",
            std::mem::size_of::<u32>() * indices.len()
        );
        let index_buffer = CpuAccessibleBuffer::from_iter(
            window.device(),
            BufferUsage::all(),
            indices.into_iter(),
        )?;

        let (texture, tex_future) = Self::upload_texture_rgba(window, img.to_rgba())?;
        tex_future.then_signal_fence_and_flush()?.cleanup_finished();
        let sampler = Self::make_sampler(window.device())?;

        let pds = Arc::new(
            PersistentDescriptorSet::start(pipeline.clone(), 0)
                .add_sampled_image(texture.clone(), sampler.clone())?
                .build()?,
        );

        Ok(QuadRenderer {
            pds,
            vertex_buffer,
            index_buffer,
        })
    }

    fn upload_texture_rgba(
        window: &GraphicsWindow,
        image_buf: ImageBuffer<Rgba<u8>, Vec<u8>>,
    ) -> Fallible<(Arc<ImmutableImage<Format>>, Box<GpuFuture>)> {
        let image_dim = image_buf.dimensions();
        let image_data = image_buf.into_raw().clone();

        let dimensions = Dimensions::Dim2d {
            width: image_dim.0,
            height: image_dim.1,
        };
        let (texture, tex_future) = ImmutableImage::from_iter(
            image_data.iter().cloned(),
            dimensions,
            Format::R8G8B8A8Unorm,
            window.queue(),
        )?;
        trace!(
            "uploading texture with {} bytes",
            image_dim.0 * image_dim.1 * 4
        );
        Ok((texture, Box::new(tex_future) as Box<GpuFuture>))
    }

    fn make_sampler(device: Arc<Device>) -> Fallible<Arc<Sampler>> {
        let sampler = Sampler::new(
            device.clone(),
            Filter::Linear,
            Filter::Linear,
            MipmapMode::Nearest,
            SamplerAddressMode::ClampToEdge,
            SamplerAddressMode::ClampToEdge,
            SamplerAddressMode::ClampToEdge,
            0.0,
            1.0,
            0.0,
            0.0,
        )?;

        Ok(sampler)
    }
}

pub struct DialogRenderer {
    _dlg: Arc<Box<Dialog>>,
    //palette: Palette,
    pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
    push_constants: vs::ty::PushConstantData,
    quads: Vec<QuadRenderer>,
}

impl DialogRenderer {
    pub fn new(
        dlg: Arc<Box<Dialog>>,
        screen_picture: &str,
        lib: &Arc<Box<Library>>,
        window: &GraphicsWindow,
    ) -> Fallible<Self> {
        trace!("DialogRenderer::new");
        let system_palette = Palette::from_bytes(&lib.load("PALETTE.PAL")?)?;
        let screen_data = lib.load(screen_picture)?;
        let screen_meta = Pic::from_bytes(&screen_data)?;
        let screen_img = Pic::decode(&system_palette, &screen_data)?;

        let btn_img = Pic::decode(&screen_meta.palette.unwrap(), &lib.load("ACTIOD0M.PIC")?)?;

        let vs = vs::Shader::load(window.device())?;
        let fs = fs::Shader::load(window.device())?;

        let pipeline = Arc::new(
            GraphicsPipeline::start()
                .vertex_input_single_buffer::<Vertex>()
                .vertex_shader(vs.main_entry_point(), ())
                .triangle_strip()
                .cull_mode_back()
                .front_face_counter_clockwise()
                .viewports_dynamic_scissors_irrelevant(1)
                .fragment_shader(fs.main_entry_point(), ())
                /*
                .depth_stencil(DepthStencil {
                    depth_write: false,
                    depth_compare: Compare::GreaterOrEqual,
                    depth_bounds_test: DepthBounds::Disabled,
                    stencil_front: Default::default(),
                    stencil_back: Default::default(),
                })
                */
                //.blend_alpha_blending()
                .render_pass(
                    Subpass::from(window.render_pass(), 0)
                        .expect("gfx: did not find a render pass"),
                )
                .build(window.device())?,
        );

        let mut quads = Vec::new();
        quads.push(QuadRenderer::new(
            -1f32,
            -1f32,
            1f32,
            1f32,
            &screen_img,
            pipeline.clone(),
            window,
        )?);

        fn rescale_pos(x: u16, y: u16) -> (f32, f32) {
            (
                2f32 * ((x as f32) / 250f32) - 1f32,
                2f32 * ((y as f32) / 350f32) - 1f32,
            )
        }
        fn rescale_offset(w: u16, h: u16) -> (f32, f32) {
            (((w as f32) / 256f32), ((h as f32) / 350f32))
        }

        for widget in &dlg.widgets {
            println!("DRAWING: {:?}", widget);
            match widget {
                Widget::Action(DrawAction {
                    unk0, unk1, unk2, ..
                }) => {
                    let (x, y) = rescale_pos(*unk0, *unk1);
                    let (w, h) = rescale_offset(*unk2, 35);
                    quads.push(QuadRenderer::new(
                        x,
                        y,
                        x + w,
                        y + h,
                        &btn_img,
                        pipeline.clone(),
                        window,
                    )?);
                }
                _ => println!("skipping: {:?}", widget),
            }
        }

        Ok(Self {
            _dlg: dlg,
            //palette: screen_meta.palette.unwrap(),
            pipeline,
            push_constants: vs::ty::PushConstantData::new(),
            quads,
        })
    }

    pub fn set_projection(&mut self, window: &GraphicsWindow) -> Fallible<()> {
        let dim = window.dimensions()?;
        let aspect = window.aspect_ratio()? * 4f32 / 3f32;
        if dim[0] > dim[1] {
            self.push_constants
                .set_projection(Matrix4::new_nonuniform_scaling(&Vector3::new(
                    aspect, 1f32, 1f32,
                )));
        } else {
            self.push_constants
                .set_projection(Matrix4::new_nonuniform_scaling(&Vector3::new(
                    1f32,
                    1f32 / aspect,
                    1f32,
                )));
        }
        Ok(())
    }

    pub fn render(
        &self,
        cb: AutoCommandBufferBuilder,
        dynamic_state: &DynamicState,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let mut cb = cb;
        for quad in &self.quads {
            cb = cb.draw_indexed(
                self.pipeline.clone(),
                dynamic_state,
                vec![quad.vertex_buffer.clone()],
                quad.index_buffer.clone(),
                quad.pds.clone(),
                self.push_constants,
            )?;
        }

        Ok(cb)
    }
}
