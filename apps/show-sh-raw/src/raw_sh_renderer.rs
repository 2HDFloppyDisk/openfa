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
use crate::texture_atlas::TextureAtlas;
use crate::window::GraphicsWindow;
use camera::CameraAbstract;
use failure::{bail, ensure, Fallible};
use geometry::Arrow;
use i386::ExitInfo;
use image::{ImageBuffer, Rgba};
use lib::Library;
use log::trace;
use nalgebra::{Matrix4, Point3, Vector3, Vector4};
use pal::Palette;
use pic::Pic;
use sh::{FacetFlags, Instr, RawShape};
use std::{
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use vulkano::{
    buffer::{BufferUsage, CpuAccessibleBuffer},
    command_buffer::{AutoCommandBufferBuilder, DynamicState},
    descriptor::descriptor_set::{DescriptorSet, PersistentDescriptorSet},
    device::Device,
    format::Format,
    framebuffer::Subpass,
    image::{Dimensions, ImmutableImage},
    impl_vertex,
    pipeline::{
        depth_stencil::{Compare, DepthBounds, DepthStencil},
        GraphicsPipeline, GraphicsPipelineAbstract,
    },
    sampler::{Filter, MipmapMode, Sampler, SamplerAddressMode},
    sync::GpuFuture,
};

#[derive(Copy, Clone, Default)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
    tex_coord: [f32; 2],
    flags: u32,
}
impl_vertex!(Vertex, position, color, tex_coord, flags);

mod vs {
    use vulkano_shaders::shader;

    shader! {
    ty: "vertex",
        src: "
            #version 450

            layout(location = 0) in vec3 position;
            layout(location = 1) in vec4 color;
            layout(location = 2) in vec2 tex_coord;
            layout(location = 3) in uint flags;

            layout(push_constant) uniform PushConstantData {
              mat4 view;
              mat4 projection;
            } pc;

            layout(location = 0) smooth out vec4 v_color;
            layout(location = 1) smooth out vec2 v_tex_coord;
            layout(location = 2) flat out uint v_flags;

            void main() {
                gl_Position = pc.projection * pc.view * vec4(position, 1.0);
                v_color = color;
                v_tex_coord = tex_coord;
                v_flags = flags;
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
            layout(location = 2) flat in uint v_flags;

            layout(location = 0) out vec4 f_color;

            layout(set = 0, binding = 0) uniform sampler2D tex;

            void main() {
                if (v_tex_coord.x == 0.0) {
                    f_color = v_color;
                } else {
                    vec4 tex_color = texture(tex, v_tex_coord);

                    if ((v_flags & 1) == 1) {
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
        }
    }

    fn set_view(&mut self, mat: Matrix4<f32>) {
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

#[derive(Clone)]
pub struct ShInstance {
    push_constants: vs::ty::PushConstantData,
    pds: Arc<dyn DescriptorSet + Send + Sync>,
    vertex_buffer: Arc<CpuAccessibleBuffer<[Vertex]>>,
    index_buffer: Arc<CpuAccessibleBuffer<[u32]>>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct DrawMode {
    pub range: Option<[usize; 2]>,
    pub damaged: bool,
    pub closeness: usize,
    pub frame_number: usize,
    pub detail: u16,

    pub gear_position: Option<u32>,
    pub bay_position: Option<u32>,
    pub flaps_down: bool,
    pub slats_down: bool,
    pub airbrake_extended: bool,
    pub hook_extended: bool,
    pub afterburner_enabled: bool,
    pub rudder_position: i32,
    pub left_aileron_position: i32,
    pub right_aileron_position: i32,
    pub sam_count: u32,
}

pub struct RawShRenderer {
    system_palette: Rc<Box<Palette>>,
    pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
    instance: Option<ShInstance>,
}

const INST_BASE: u32 = 0x0000_4000;

impl RawShRenderer {
    pub fn new(system_palette: Rc<Box<Palette>>, window: &GraphicsWindow) -> Fallible<Self> {
        trace!("RawShRenderer::new");

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
        Ok(RawShRenderer {
            system_palette,
            pipeline,
            instance: None,
        })
    }

    pub fn set_projection(&mut self, projection: &Matrix4<f32>) {
        self.instance
            .as_mut()
            .unwrap()
            .push_constants
            .set_projection(projection);
    }

    pub fn set_view(&mut self, view: Matrix4<f32>) {
        self.instance
            .as_mut()
            .unwrap()
            .push_constants
            .set_view(view);
    }

    #[allow(clippy::cognitive_complexity)] // Don't know where the end is, so can't organize better.
    pub fn add_shape_to_render(
        &mut self,
        _name: &str,
        sh: &RawShape,
        stop_at_offset: usize,
        draw_mode: &DrawMode,
        lib: &Library,
        window: &GraphicsWindow,
    ) -> Fallible<()> {
        let mut _xform = [0f32, 0f32, 0f32, 0f32, 0f32, 0f32];

        let texture_filenames = sh.all_textures();
        let mut texture_headers = Vec::new();
        for filename in texture_filenames {
            let data = lib.load(&filename.to_uppercase())?;
            texture_headers.push((filename.to_owned(), Pic::from_bytes(&data)?, data));
        }
        let atlas = TextureAtlas::from_raw_data(&self.system_palette, texture_headers)?;
        let mut active_frame = None;

        let flaps_down = draw_mode.flaps_down;
        let slats_down = draw_mode.slats_down;
        let gear_position = draw_mode.gear_position;
        let bay_position = draw_mode.bay_position;
        let airbrake_extended = draw_mode.airbrake_extended;
        let hook_extended = draw_mode.hook_extended;
        let afterburner_enabled = draw_mode.afterburner_enabled;
        let rudder_position = draw_mode.rudder_position;
        let left_aileron_position = draw_mode.left_aileron_position;
        let right_aileron_position = draw_mode.right_aileron_position;
        let current_ticks = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let sam_count = draw_mode.sam_count;

        let call_names = vec![
            "do_start_interp",
            "_CATGUYDraw@4",
            "@HARDNumLoaded@8",
            "@HardpointAngle@4",
            "_InsectWingAngle@0",
        ];
        let mut interp = i386::Interpreter::new();
        let mut _v = [0u8; 0x100];
        _v[0x8E + 1] = 0x1;
        /*
        let mut inst = Vec::new();
        for i in 0..0x100 {
            inst.push(0u8);
        }
        inst[0x40] = 0xFF;
        interp
            .map_writable(INST_BASE, inst)
            .unwrap();
        */
        for tramp in sh.trampolines.iter() {
            if call_names.contains(&tramp.name.as_ref()) {
                interp.add_trampoline(tramp.mem_location, &tramp.name, 1);
                continue;
            }
            println!(
                "Adding port for {} at {:08X}",
                tramp.name, tramp.mem_location
            );
            match tramp.name.as_ref() {
                "_currentTicks" => interp.map_value(tramp.mem_location, current_ticks as u32),
                "_lowMemory" => interp.map_value(tramp.mem_location, 0),
                "_nightHazing" => interp.map_value(tramp.mem_location, 1),
                "_PLafterBurner" => {
                    interp.map_value(tramp.mem_location, afterburner_enabled as u32)
                }
                "_PLbayOpen" => interp.map_value(tramp.mem_location, bay_position.is_some() as u32),
                "_PLbayDoorPos" => interp.map_value(tramp.mem_location, bay_position.unwrap_or(0)),
                "_PLbrake" => interp.map_value(tramp.mem_location, airbrake_extended as u32),
                "_PLcanardPos" => interp.map_value(tramp.mem_location, 0),
                "_PLdead" => interp.map_value(tramp.mem_location, 0),
                "_PLgearDown" => {
                    interp.map_value(tramp.mem_location, gear_position.is_some() as u32)
                }
                "_PLgearPos" => interp.map_value(tramp.mem_location, gear_position.unwrap_or(0)),
                "_PLhook" => interp.map_value(tramp.mem_location, hook_extended as u32),
                "_PLrightFlap" => {
                    interp.map_value(tramp.mem_location, if flaps_down { 0xFFFF_FFFF } else { 0 })
                }
                "_PLleftFlap" => {
                    interp.map_value(tramp.mem_location, if flaps_down { 0xFFFF_FFFF } else { 0 })
                }
                "_PLrightAln" => {
                    interp.map_value(tramp.mem_location, right_aileron_position as u32)
                }
                "_PLleftAln" => interp.map_value(tramp.mem_location, left_aileron_position as u32),
                "_PLrudder" => interp.map_value(tramp.mem_location, rudder_position as u32),
                "_PLslats" => interp.map_value(tramp.mem_location, slats_down as u32),
                "_PLstate" => interp.map_value(tramp.mem_location, 0),
                "_PLswingWing" => interp.map_value(tramp.mem_location, 0),
                "_PLvtAngle" => interp.map_value(tramp.mem_location, 0),
                "_PLvtOn" => interp.map_value(tramp.mem_location, 0),

                "_SAMcount" => interp.map_value(tramp.mem_location, sam_count),

                "brentObjId" => interp.map_value(tramp.mem_location, INST_BASE),

                "_effectsAllowed" => {
                    interp.map_writable(tramp.mem_location, vec![2, 0, 0, 0])?;
                }
                "_effects" => {
                    interp.map_writable(tramp.mem_location, vec![2, 0, 0, 0])?;
                }
                "lighteningAllowed" => {
                    interp.map_writable(tramp.mem_location, vec![0, 0, 0, 0])?;
                }
                "mapAdj" => {
                    interp.map_writable(tramp.mem_location, vec![0, 0, 0, 0])?;
                }

                "_v" => {
                    interp
                        .map_writable(tramp.mem_location, _v.to_vec())
                        .unwrap();
                }
                _ => {}
            }
        }
        for instr in &sh.instrs {
            match instr {
                // Written into by windmill with (_currentTicks & 0xFF) << 2.
                // The frame of animation to show, maybe?
                Instr::XformUnmask(ref c4) => {
                    interp
                        .map_writable(0xAA00_0000 + c4.offset as u32 + 2, c4.xform_base.to_vec())?;
                }
                Instr::XformUnmask4(ref c6) => {
                    interp
                        .map_writable(0xAA00_0000 + c6.offset as u32 + 2, c6.xform_base.to_vec())?;
                }
                Instr::UnkE4(ref e4) => {
                    let mut v = Vec::new();
                    for i in 0..sh::UnkE4::SIZE {
                        v.push(unsafe { *e4.data.add(i) });
                    }
                    interp
                        .map_writable((0xAA00_0000 + e4.offset) as u32, v)
                        .unwrap();
                }
                Instr::UnkEA(ref _ea) => {
                    // interp.add_write_port(0xAA00_0000 + ea.offset as u32 + 2, move |value| {
                    //     println!("WOULD UPDATE EA.0 <- {:04X}", value);
                    // });
                    // interp.add_write_port(0xAA00_0000 + ea.offset as u32 + 2 + 2, move |value| {
                    //     println!("WOULD UPDATE EA.2 <- {:04X}", value);
                    // });
                }
                Instr::UnknownData(ref unk) => {
                    interp
                        .map_writable((0xAA00_0000 + unk.offset) as u32, unk.data.clone())
                        .unwrap();
                }
                Instr::X86Code(ref code) => {
                    interp.add_code(code.bytecode.clone());
                }
                _ => {}
            }
        }

        // The current pool of vertices.
        let mut vert_pool = Vec::new();

        // We pull from the vert buffer as needed to build faces, because the color and
        // texture information is specified per face.
        let mut indices = Vec::new();
        let mut verts = Vec::new();

        let mut _end_target = None;
        let mut damage_target = None;
        let mut section_close = None;

        let mut unmasked_faces = HashMap::new();
        let mut masking_faces = false;

        let mut byte_offset = 0;
        let mut offset = 0;
        while offset < sh.instrs.len() {
            let instr = &sh.instrs[offset];

            // Handle ranged mode before all others. No guarantee we won't be sidetracked;
            // we may need to split this into a different runloop.
            if let Some([start, end]) = draw_mode.range {
                if byte_offset < start {
                    byte_offset += instr.size();
                    offset += 1;
                    continue;
                }
                if byte_offset >= end {
                    byte_offset += instr.size();
                    offset += 1;
                    continue;
                }
            }

            if offset > stop_at_offset {
                trace!("reached configured stopping point");
                break;
            }

            if let Some(close_offset) = section_close {
                if close_offset == byte_offset {
                    trace!("reached section close; stopping");
                    // FIXME: jump to end_offset
                    break;
                }
            }
            if let Some(damage_offset) = damage_target {
                if damage_offset == byte_offset && !draw_mode.damaged {
                    trace!("reached damage section in non-damage draw mode; stopping");
                    // FIXME: jump to end_offset
                    break;
                }
            }

            println!("At: {:3} => {}", offset, instr.show());
            match instr {
                Instr::X86Code(code) => {
                    let rv = interp.interpret(code.code_offset(0xAA00_0000u32)).unwrap();
                    match rv {
                        ExitInfo::OutOfInstructions => break,
                        ExitInfo::Trampoline(ref name, ref args) => {
                            println!("Got trampoline return to {} with args {:?}", name, args);
                            // FIXME: handle call and set up return if !do_start_interp
                            match name.as_str() {
                                "do_start_interp" => {
                                    byte_offset = (args[0] - 0xAA00_0000u32) as usize;
                                    offset =
                                        sh.map_interpreter_offset_to_instr_offset(args[0]).unwrap();
                                    println!("Resuming at instruction {}", offset);
                                    continue;
                                }
                                "@HARDNumLoaded@8" => {
                                    interp.set_register_value(i386::Reg::EAX, 1);
                                    let exit_info = interp.interpret(interp.eip())?;
                                    let (name, args) = exit_info.ok_trampoline()?;
                                    ensure!(
                                        name == "do_start_interp",
                                        "unexpected trampoline return"
                                    );
                                    ensure!(args.len() == 1, "unexpected arg count");
                                    byte_offset = (args[0] - 0xAA00_0000u32) as usize;
                                    offset =
                                        sh.map_interpreter_offset_to_instr_offset(args[0]).unwrap();
                                    println!("Resuming at instruction {}", offset);
                                    continue;
                                }
                                "@HardpointAngle@4" => {
                                    interp.set_register_value(i386::Reg::EAX, 256);
                                    let exit_info = interp.interpret(interp.eip())?;
                                    let (name, args) = exit_info.ok_trampoline()?;
                                    ensure!(
                                        name == "do_start_interp",
                                        "unexpected trampoline return"
                                    );
                                    ensure!(args.len() == 1, "unexpected arg count");
                                    byte_offset = (args[0] - 0xAA00_0000u32) as usize;
                                    offset =
                                        sh.map_interpreter_offset_to_instr_offset(args[0]).unwrap();
                                    println!("Resuming at instruction {}", offset);
                                    continue;
                                }
                                "_InsectWingAngle@0" => {
                                    interp.set_register_value(i386::Reg::EAX, 256);
                                    let exit_info = interp.interpret(interp.eip())?;
                                    let (name, args) = exit_info.ok_trampoline()?;
                                    ensure!(
                                        name == "do_start_interp",
                                        "unexpected trampoline return"
                                    );
                                    ensure!(args.len() == 1, "unexpected arg count");
                                    byte_offset = (args[0] - 0xAA00_0000u32) as usize;
                                    offset =
                                        sh.map_interpreter_offset_to_instr_offset(args[0]).unwrap();
                                    println!("Resuming at instruction {}", offset);
                                    continue;
                                }
                                _ => bail!("don't know how to handle {}", name),
                            }
                        }
                    }
                }
                Instr::Unmask(unk) => {
                    unmasked_faces.insert(unk.target_byte_offset(), [0f32; 6]);
                }
                Instr::Unmask4(unk) => {
                    unmasked_faces.insert(unk.target_byte_offset(), [0f32; 6]);
                }
                Instr::XformUnmask(c4) => {
                    let xform = [
                        f32::from(c4.t0),
                        f32::from(c4.t1),
                        f32::from(c4.t2),
                        f32::from(c4.a0),
                        f32::from(c4.a1),
                        f32::from(c4.a2),
                    ];
                    unmasked_faces.insert(c4.target_byte_offset(), xform);
                }
                Instr::XformUnmask4(c6) => {
                    let xform = [
                        f32::from(c6.t0),
                        f32::from(c6.t1),
                        f32::from(c6.t2),
                        f32::from(c6.a0),
                        f32::from(c6.a1),
                        f32::from(c6.a2),
                    ];
                    unmasked_faces.insert(c6.target_byte_offset(), xform);
                }
                Instr::Header(_hdr) => {
                    //_xform = [0f32, 0f32, 0f32, 0f32, 0f32, 0f32];
                }
                Instr::TextureRef(texture) => {
                    active_frame = Some(&atlas.frames[&texture.filename]);
                }
                Instr::PtrToObjEnd(end) => {
                    // We do not ever not draw from range; maybe there is some other use of
                    // this target offset that we just don't know yet?
                    _end_target = Some(end.end_byte_offset())
                }
                Instr::JumpToDamage(dam) => {
                    damage_target = Some(dam.damage_byte_offset());
                    if draw_mode.damaged {
                        trace!(
                            "jumping to damaged model at {:04X}",
                            dam.damage_byte_offset()
                        );
                        byte_offset = dam.damage_byte_offset();
                        offset = sh.bytes_to_index(byte_offset)?;
                        continue;
                    }
                }
                Instr::JumpToLOD(lod) => {
                    if draw_mode.closeness > lod.unk1 as usize {
                        // For high detail, the bytes after the c8 up to the indicated end contain
                        // the high detail model.
                        trace!("setting section close to {}", lod.target_byte_offset());
                        section_close = Some(lod.target_byte_offset());
                    } else {
                        // For low detail, the bytes after the c8 end marker contain the low detail
                        // model. We have no way to know how where the close is, so we have to
                        // monitor and abort to end if we hit the damage section?
                        trace!(
                            "jumping to low detail model at {:04X}",
                            lod.target_byte_offset()
                        );
                        byte_offset = lod.target_byte_offset();
                        offset = sh.bytes_to_index(byte_offset)?;
                        continue;
                    }
                }
                Instr::JumpToDetail(detail) => {
                    if draw_mode.detail == detail.level {
                        // If we are drawing in a low detail, jump to the relevant model.
                        trace!(
                            "jumping to low detail model at {:04X}",
                            detail.target_byte_offset()
                        );
                        byte_offset = detail.target_byte_offset();
                        offset = sh.bytes_to_index(byte_offset)?;
                        continue;
                    } else {
                        // If in higher detail we want to not draw this section.
                        trace!("setting section close to {}", detail.target_byte_offset());
                        section_close = Some(detail.target_byte_offset());
                    }
                }
                Instr::EndOfObject(_end) => {
                    break;
                }
                Instr::JumpToFrame(animation) => {
                    byte_offset = animation.target_for_frame(draw_mode.frame_number);
                    offset = sh.bytes_to_index(byte_offset)?;
                    continue;
                }
                Instr::Jump(jump) => {
                    byte_offset = jump.target_byte_offset();
                    offset = sh.bytes_to_index(byte_offset)?;
                    continue;
                }
                Instr::VertexBuf(buf) => {
                    let xform = if vert_pool.is_empty() {
                        masking_faces = false;
                        [0f32; 6]
                    } else if unmasked_faces.contains_key(&instr.at_offset()) {
                        masking_faces = false;
                        unmasked_faces[&instr.at_offset()]
                    } else {
                        masking_faces = true;
                        [0f32; 6]
                    };
                    let r2 = xform[5] / 256f32;
                    let m = Matrix4::new(
                        r2.cos(),
                        -r2.sin(),
                        0f32,
                        xform[0],
                        r2.sin(),
                        r2.cos(),
                        0f32,
                        -xform[1],
                        0f32,
                        0f32,
                        1f32,
                        xform[2],
                        0f32,
                        0f32,
                        0f32,
                        1f32,
                    );
                    if buf.buffer_target_offset() < vert_pool.len() {
                        vert_pool.truncate(buf.buffer_target_offset());
                    } else {
                        let pad_count = buf.buffer_target_offset() - vert_pool.len();
                        for _ in 0..pad_count {
                            vert_pool.push(Default::default());
                        }
                    }
                    for v in &buf.verts {
                        let v0 =
                            Vector4::new(f32::from(v[0]), f32::from(-v[2]), f32::from(v[1]), 1f32);
                        let v1 = m * v0;
                        vert_pool.push(Vertex {
                            position: [v1[0], v1[1], -v1[2]],
                            color: [0.75f32, 0.5f32, 0f32, 1f32],
                            tex_coord: [0f32, 0f32],
                            flags: 0,
                        });
                    }
                }
                Instr::Facet(facet) => {
                    if !masking_faces {
                        // Load all vertices in this facet into the vertex upload buffer, copying
                        // in the color and texture coords for each face. Note that the layout is
                        // for triangle fans.
                        let mut v_base = verts.len() as u32;
                        for i in 2..facet.indices.len() {
                            // Given that most facets are very short strips, and we need to copy the
                            // vertices anyway, it is probably more space efficient to just upload triangle
                            // lists instead of trying to span safely between adjacent strips.
                            let o = [0, i - 1, i];
                            let inds = [
                                facet.indices[o[0]],
                                facet.indices[o[1]],
                                facet.indices[o[2]],
                            ];
                            let tcs = if facet.flags.contains(FacetFlags::HAVE_TEXCOORDS) {
                                [
                                    facet.tex_coords[o[0]],
                                    facet.tex_coords[o[1]],
                                    facet.tex_coords[o[2]],
                                ]
                            } else {
                                [[0, 0], [0, 0], [0, 0]]
                            };

                            for (index, tex_coord) in inds.iter().zip(&tcs) {
                                if (*index as usize) >= vert_pool.len() {
                                    println!(
                                        "skipping face with index at {} of {}",
                                        *index,
                                        vert_pool.len()
                                    );
                                    continue;
                                }
                                ensure!(
                                    (*index as usize) < vert_pool.len(),
                                    "out-of-bounds vertex reference in facet {:?}, current pool size: {}",
                                    facet,
                                    vert_pool.len()
                                );
                                let mut v = vert_pool[*index as usize];
                                v.color = self.system_palette.rgba_f32(facet.color as usize)?;
                                if facet.flags.contains(FacetFlags::FILL_BACKGROUND)
                                    || facet.flags.contains(FacetFlags::UNK1)
                                    || facet.flags.contains(FacetFlags::UNK5)
                                {
                                    v.flags = 1;
                                }
                                if facet.flags.contains(FacetFlags::HAVE_TEXCOORDS) {
                                    assert!(active_frame.is_some());
                                    let frame = active_frame.unwrap();
                                    v.tex_coord = frame.tex_coord_at(*tex_coord);
                                }
                                //println!("v: {:?}", v.position);
                                verts.push(v);
                                indices.push(v_base);
                                v_base += 1;
                            }
                        }
                    }
                }
                Instr::VertexNormal(dot) => {
                    let pt = vert_pool[dot.index];
                    let v0 = Point3::new(pt.position[0], pt.position[1], pt.position[2]);
                    // right: 100f32, 0f32, 0f32
                    // down:  0f32, 100f32, 0f32
                    // back:  0f32, 0f32, 100f32
                    let n = Vector3::new(
                        f32::from(dot.norm[0]),
                        f32::from(-dot.norm[1]),
                        f32::from(dot.norm[2]),
                    );
                    let base = verts.len() as u32;
                    let arrow = Arrow::new(v0, n / 12f32);
                    for pos in &arrow.verts {
                        let v = Vertex {
                            flags: 0,
                            position: [pos.x, pos.y, pos.z],
                            tex_coord: [0f32, 0f32],
                            color: [1f32, 1f32, 1f32, 1f32],
                            // color: self.system_palette.rgba_f32(dot.color as usize)?,
                        };
                        verts.push(v);
                    }
                    for face in &arrow.faces {
                        indices.push(base + face.index0);
                        indices.push(base + face.index1);
                        indices.push(base + face.index2);
                    }
                }
                _ => {}
            }

            offset += 1;
            byte_offset += instr.size();
        }

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

        let (texture, tex_future) = Self::upload_texture_rgba(window, atlas.img.to_rgba())?;
        tex_future.then_signal_fence_and_flush()?.cleanup_finished();
        let sampler = Self::make_sampler(window.device())?;

        let pds = Arc::new(
            PersistentDescriptorSet::start(self.pipeline.clone(), 0)
                .add_sampled_image(texture, sampler)?
                .build()?,
        );

        let inst = ShInstance {
            push_constants: vs::ty::PushConstantData::new(),
            pds,
            vertex_buffer,
            index_buffer,
        };

        self.instance = Some(inst);

        Ok(())
    }

    fn upload_texture_rgba(
        window: &GraphicsWindow,
        image_buf: ImageBuffer<Rgba<u8>, Vec<u8>>,
    ) -> Fallible<(Arc<ImmutableImage<Format>>, Box<dyn GpuFuture>)> {
        let image_dim = image_buf.dimensions();
        let image_data = image_buf.into_raw();

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
        Ok((texture, Box::new(tex_future) as Box<dyn GpuFuture>))
    }

    fn make_sampler(device: Arc<Device>) -> Fallible<Arc<Sampler>> {
        let sampler = Sampler::new(
            device,
            Filter::Nearest,
            Filter::Nearest,
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

    pub fn before_frame(&mut self, camera: &dyn CameraAbstract) -> Fallible<()> {
        self.set_view(camera.view_matrix());
        self.set_projection(&camera.projection_matrix());
        Ok(())
    }

    pub fn render(
        &self,
        command_buffer: AutoCommandBufferBuilder,
        dynamic_state: &DynamicState,
    ) -> Fallible<AutoCommandBufferBuilder> {
        let inst = self.instance.clone().unwrap();
        Ok(command_buffer.draw_indexed(
            self.pipeline.clone(),
            dynamic_state,
            vec![inst.vertex_buffer.clone()],
            inst.index_buffer.clone(),
            inst.pds.clone(),
            inst.push_constants,
        )?)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use camera::ArcBallCamera;
    use failure::Error;
    use omnilib::OmniLib;
    use sh::RawShape;
    use std::f64::consts::PI;
    use window::GraphicsConfigBuilder;

    #[test]
    fn it_can_render_raw_shapes() -> Fallible<()> {
        let mut window = GraphicsWindow::new(&GraphicsConfigBuilder::new().build())?;
        let mut camera = ArcBallCamera::new(window.aspect_ratio_f64()?, 0.1, 3.4e+38);
        camera.set_distance(100.);
        camera.set_angle(115. * PI / 180., -135. * PI / 180.);

        let omni = OmniLib::new_for_test_in_games(&[
            "USNF", "MF", "ATF", "ATFNATO", "ATFGOLD", "USNF97", "FA",
        ])?;
        let skipped = vec![
            "CATGUY.SH",
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

            for name in lib.find_matching("*.SH")?.iter() {
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

                let data = lib.load(name)?;
                let sh = RawShape::from_bytes(&data)?;
                let mut sh_renderer = RawShRenderer::new(system_palette.clone(), &window)?;

                let draw_mode = DrawMode {
                    range: None,
                    damaged: false,
                    closeness: 0x200,
                    frame_number: 0,
                    detail: 4,
                    gear_position: Some(18),
                    bay_position: Some(18),
                    flaps_down: false,
                    slats_down: false,
                    airbrake_extended: true,
                    hook_extended: true,
                    afterburner_enabled: true,
                    rudder_position: 0,
                    left_aileron_position: 0,
                    right_aileron_position: 0,
                    sam_count: 4,
                };
                sh_renderer.add_shape_to_render(
                    &name,
                    &sh,
                    usize::max_value(),
                    &draw_mode,
                    &lib,
                    &window,
                )?;

                {
                    let frame = window.begin_frame()?;
                    if !frame.is_valid() {
                        continue;
                    }

                    sh_renderer.before_frame(&camera)?;

                    let mut cbb = AutoCommandBufferBuilder::primary_one_time_submit(
                        window.device(),
                        window.queue().family(),
                    )?;

                    cbb = cbb.begin_render_pass(
                        frame.framebuffer(&window),
                        false,
                        vec![[0f32, 0f32, 1f32, 1f32].into(), 0f32.into()],
                    )?;

                    cbb = sh_renderer.render(cbb, &window.dynamic_state)?;

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