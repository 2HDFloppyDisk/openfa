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

// Accumulate all depthless raymarching passes into one draw operation.

use atmosphere::AtmosphereBuffer;
use failure::Fallible;
use fullscreen::{FullscreenBuffer, FullscreenVertex};
use global_data::GlobalParametersBuffer;
use gpu::GPU;
use log::trace;
use stars::StarsBuffer;
use wgpu;

pub struct SkyboxRenderPass {
    pipeline: wgpu::RenderPipeline,
}

impl SkyboxRenderPass {
    pub fn new(
        gpu: &mut GPU,
        globals_buffer: &GlobalParametersBuffer,
        _fullscreen_buffer: &FullscreenBuffer,
        stars_buffer: &StarsBuffer,
        atmosphere_buffer: &AtmosphereBuffer,
    ) -> Fallible<Self> {
        trace!("SkyboxRenderPass::new");

        let vert_shader =
            gpu.create_shader_module(include_bytes!("../target/skybox.vert.spirv"))?;
        let frag_shader =
            gpu.create_shader_module(include_bytes!("../target/skybox.frag.spirv"))?;

        let pipeline_layout =
            gpu.device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    bind_group_layouts: &[
                        globals_buffer.bind_group_layout(),
                        atmosphere_buffer.bind_group_layout(),
                        stars_buffer.bind_group_layout(),
                    ],
                });

        let pipeline = gpu
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                layout: &pipeline_layout,
                vertex_stage: wgpu::ProgrammableStageDescriptor {
                    module: &vert_shader,
                    entry_point: "main",
                },
                fragment_stage: Some(wgpu::ProgrammableStageDescriptor {
                    module: &frag_shader,
                    entry_point: "main",
                }),
                rasterization_state: Some(wgpu::RasterizationStateDescriptor {
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: wgpu::CullMode::Back,
                    depth_bias: 0,
                    depth_bias_slope_scale: 0.0,
                    depth_bias_clamp: 0.0,
                }),
                primitive_topology: wgpu::PrimitiveTopology::TriangleStrip,
                color_states: &[wgpu::ColorStateDescriptor {
                    format: GPU::texture_format(),
                    color_blend: wgpu::BlendDescriptor::REPLACE,
                    alpha_blend: wgpu::BlendDescriptor::REPLACE,
                    write_mask: wgpu::ColorWrite::ALL,
                }],
                depth_stencil_state: Some(wgpu::DepthStencilStateDescriptor {
                    format: GPU::DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil_front: wgpu::StencilStateFaceDescriptor::IGNORE,
                    stencil_back: wgpu::StencilStateFaceDescriptor::IGNORE,
                    stencil_read_mask: 0,
                    stencil_write_mask: 0,
                }),
                index_format: wgpu::IndexFormat::Uint16,
                vertex_buffers: &[FullscreenVertex::descriptor()],
                sample_count: 1,
                sample_mask: !0,
                alpha_to_coverage_enabled: false,
            });

        Ok(Self { pipeline })
    }

    pub fn draw(
        &self,
        rpass: &mut wgpu::RenderPass,
        globals_buffer: &GlobalParametersBuffer,
        fullscreen_buffer: &FullscreenBuffer,
        stars_buffer: &StarsBuffer,
        atmosphere_buffer: &AtmosphereBuffer,
    ) {
        rpass.set_pipeline(&self.pipeline);
        rpass.set_bind_group(0, &globals_buffer.bind_group(), &[]);
        rpass.set_bind_group(1, &atmosphere_buffer.bind_group(), &[]);
        rpass.set_bind_group(2, &stars_buffer.bind_group(), &[]);
        rpass.set_vertex_buffers(0, &[(fullscreen_buffer.vertex_buffer(), 0)]);
        rpass.draw(0..4, 0..1);
    }
}