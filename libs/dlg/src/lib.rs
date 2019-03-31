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
use ansi::ansi;
use failure::{bail, ensure, err_msg, Fallible};
use peff::PE;
use reverse::bs2s;
use std::collections::{HashMap, HashSet};
use std::mem;
use std::path::Path;
use packed_struct::packed_struct;

packed_struct!(PreloadHeader {
    _0 => function: u32,
    _1 => unk0: u16,
    _2 => unk1: u16,
    _3 => unk2: u16,
    _4 => unk3: u16,
    _5 => unk4: u16,
    _6 => flag_or_ascii: u8
});

packed_struct!(PreloadFooter {
    _1 => unk0: u16,
    _2 => unk1: u16,
    _3 => unk2: u16
});

#[derive(Debug, Eq, PartialEq)]
pub enum PreloadKind {
    ChoosePreload,
    GrafPrefPreload,
    Info2640Preload,
    Info640Preload,
    MultiPreload,
    SndPrefPreload,
    TestDiagPreload,
    TopCenterDialog,
}

impl PreloadKind {
    fn from_name(name: &str) -> Fallible<Self> {
        Ok(match name {
            "_ChoosePreload" => PreloadKind::ChoosePreload,
            "_GrafPrefPreload" => PreloadKind::GrafPrefPreload,
            "_Info2640Preload" => PreloadKind::Info2640Preload,
            "_Info640Preload" => PreloadKind::Info640Preload,
            "_MultiPreload" => PreloadKind::MultiPreload,
            "_SndPrefPreload" => PreloadKind::SndPrefPreload,
            "_TestDiagPreload" => PreloadKind::TestDiagPreload,
            "_TopCenterDialog" => PreloadKind::TopCenterDialog,
            _ => bail!("unknown preload kind: {}", name)
        })
    }
}

pub struct Preload {
    kind: Option<PreloadKind>,
    unk_header_0: u16,
    unk_header_1: u16,
    unk_header_2: u16,
    unk_header_3: u16,
    unk_header_4: u16, // usually 0, but 0x100 in one case
    name: Option<String>,
    unk_footer_0: u16,
    unk_footer_1: u16,
    unk_footer_2: u16,
}

impl Preload {
    fn from_bytes(bytes: &[u8], offset: &mut usize, pe: &PE, trampolines: &HashMap<u32, String>) -> Fallible<Preload> {
        let header_ptr: *const PreloadHeader = bytes.as_ptr() as *const _;
        let header: &PreloadHeader = unsafe { &*header_ptr };
        ensure!(header.unk4() == 0 || header.unk4() == 0x0100, "expected 0 or 100 word in header");

        let trampoline_target = header.function().saturating_sub(pe.code_vaddr);
        let kind = if trampolines.contains_key(&trampoline_target) {
            Some(PreloadKind::from_name(&trampolines[&trampoline_target])?)
        } else {
            None
        };

        let header_end_offset = *offset + mem::size_of::<PreloadHeader>();
        let (name, footer_offset) = match header.flag_or_ascii() {
            0 => (None, header_end_offset),
            0xFF => (None, header_end_offset + 1),
            _ => {
                let mut off = header_end_offset;
                let mut name = String::new();
                while pe.code[off] != 0 {
                    name.push(pe.code[off] as char);
                    off += 1;
                }
                (Some(name), off + 1)
            }
        };
        let footer_ptr: *const PreloadFooter = bytes.as_ptr() as *const _;
        let footer: &PreloadFooter = unsafe { &*footer_ptr };

        let end_offset = footer_offset + mem::size_of::<PreloadFooter>();
        *offset += end_offset - *offset;
        Ok(Self {
            kind,
            unk_header_0: header.unk0(),
            unk_header_1: header.unk1(),
            unk_header_2: header.unk2(),
            unk_header_3: header.unk3(),
            unk_header_4: header.unk4(),
            name,
            unk_footer_0: footer.unk0(),
            unk_footer_1: footer.unk1(),
            unk_footer_2: footer.unk2(),
        })
    }
}

pub enum Widget {

}

pub struct Dialog {
}

impl Dialog {
    pub fn from_bytes(bytes: &[u8]) -> Fallible<Self> {
        let pe = PE::from_bytes(bytes)?;
        if pe.code.len() == 0 {
            return Ok(Self {});
        }

        let mut offset = 0;
        let trampolines = Self::find_trampolines(&pe)?;
        let widget = Preload::from_bytes(&pe.code, &mut offset, &pe, &trampolines)?;
        loop {
            let code = &pe.code[offset..];
            let dwords: &[u32] = unsafe { mem::transmute(code) };
            if dwords[0] == 0x02030201 {
                break;
            }
            let ptr = dwords[0].saturating_sub(pe.code_vaddr).saturating_sub(pe.image_base);
            ensure!(trampolines.contains_key(&ptr), "expected a pointer in first dword");
            println!("at: {}", trampolines[&ptr]);

            break;
        }
        Ok(Self {})
    }

    fn find_trampolines(pe: &PE) -> Fallible<HashMap<u32, String>> {
        ensure!(pe.code.len() >= 6, "PE too short for trampolines");
        let mut tramps = HashMap::new();
        let mut tramp_offset = pe.code.len() - 6;
        loop {
            if pe.code[tramp_offset] == 0xFF && pe.code[tramp_offset + 1] == 0x25 {
                let dwords: *const u32 =
                    unsafe { mem::transmute(pe.code[tramp_offset + 2..].as_ptr() as *const u8) };
                let tgt = unsafe { *dwords };
                let mut found = false;
                for thunk in &pe.thunks {
                    if thunk.vaddr == tgt.saturating_sub(pe.image_base) {
                        found = true;
                        tramps.insert(tramp_offset as u32, thunk.name.clone());
                        break;
                    }
                }
                assert!(found, "no matching thunk");
                tramp_offset -= 6;
            } else {
                break;
            }
        }
        return Ok(tramps);
    }

    pub fn explore(name: &str, bytes: &[u8]) -> Fallible<()> {
        let pe = PE::from_bytes(bytes)?;
        if pe.code.len() == 0 {
            return Ok(());
        }

        let vaddr = pe.code_vaddr;

        //println!("=== {} ======", name);

        let mut all_thunk_descrs = Vec::new();
        for thunk in &pe.thunks {
            all_thunk_descrs.push(format!("{}:{:04X}", thunk.name, thunk.vaddr));
        }

        let tramps = Self::find_trampolines(&pe)?;

        let mut relocs = HashSet::new();
        let mut targets = HashSet::new();
        let mut target_names = HashMap::new();
        for reloc in &pe.relocs {
            let r = *reloc as usize;
            relocs.insert((r, 0));
            relocs.insert((r + 1, 1));
            relocs.insert((r + 2, 2));
            relocs.insert((r + 3, 3));
            let a = pe.code[r] as u32;
            let b = pe.code[r + 1] as u32;
            let c = pe.code[r + 2] as u32;
            let d = pe.code[r + 3] as u32;
            //println!("a: {:02X} {:02X} {:02X} {:02X}", d, c, b, a);
            let vtgt = (d << 24) + (c << 16) + (b << 8) + a;
            let tgt = vtgt - vaddr;
            // println!(
            //     "tgt:{:04X} => {:04X} <> {}",
            //     tgt,
            //     vtgt,
            //     all_thunk_descrs.join(", ")
            // );
            for thunk in &pe.thunks {
                if vtgt == thunk.vaddr {
                    target_names.insert(r + 3, thunk.name.to_owned());
                    break;
                }
            }
            for (tramp_off, thunk_name) in &tramps {
                //println!("AT:{:04X} ?= {:04X}", *tramp_off, tgt);
                if tgt == *tramp_off {
                    target_names.insert(r + 3, thunk_name.to_owned());
                    break;
                }
            }
            //assert!(tgt <= pe.code.len());
            targets.insert(tgt);
            targets.insert(tgt + 1);
            targets.insert(tgt + 2);
            targets.insert(tgt + 3);
        }

        let mut instr_offset = 0;
        let mut out = String::new();
        let mut offset = 0;
        while offset < pe.code.len() {
            let b = bs2s(&pe.code[offset..offset + 1]);
            if relocs.contains(&(offset, 0)) {
                // println!("{:04} - {}", out.len(), out);
                // return Ok(Dialog {});

                // if instr_offset == 1 {
                //     println!("{:04} - {}", out.len(), out);
                //     return Ok(Dialog {});
                // }

                instr_offset += 1;
                //out = String::new();
                out += &format!("\n {}{}{}", ansi().green(), &b, ansi());
            } else if relocs.contains(&(offset, 1)) {
                out += &format!("{}{}{}", ansi().green(), &b, ansi());
            } else if relocs.contains(&(offset, 2)) {
                out += &format!("{}{}{}", ansi().green(), &b, ansi());
            } else if relocs.contains(&(offset, 3)) {
                if target_names.contains_key(&offset) {
                    out += &format!(
                        "{}{}{}[{}] ",
                        ansi().green(),
                        &b,
                        ansi(),
                        target_names[&offset]
                    );
                } else {
                    out += &format!("{}{}{} ", ansi().green(), &b, ansi());
                }
            } else if targets.contains(&(offset as u32)) {
                out += &format!("{}{}{}", ansi().red(), &b, ansi());
            //} else if offset == 0 {
            //    out += &format!("0000: ");
            } else {
                out += &b;
            }
            offset += 1;
        }

        println!("{} - {}", out, name);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnilib::OmniLib;

    #[test]
    fn it_can_load_all_dialogs() -> Fallible<()> {
        //let omni = OmniLib::new_for_test_in_games(&["ATF"])?;
        let omni = OmniLib::new_for_test()?;
        for (game, name) in omni.find_matching("*.DLG")? {
            println!("AT: {}:{}", game, name);

            //let palette = Palette::from_bytes(&omni.library(&game).load("PALETTE.PAL")?)?;
            //let img = decode_pic(&palette, &omni.library(&game).load(&name)?)?;

            let _dlg = Dialog::from_bytes(&omni.library(&game).load(&name)?)?;
            //Dialog::explore(&name, &omni.library(&game).load(&name)?)?;
        }

        Ok(())
    }
}