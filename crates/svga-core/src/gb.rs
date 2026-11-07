//! Guest-backed object commands: memory objects (MOBs), object tables,
//! surfaces and screen targets.
//!
//! They arrive as 3D-framed commands ([`crate::Command::Command3d`]): an ID
//! and a payload of words. [`parse`] turns the payload into a
//! [`GbCommand`]; the device model executes it.

use svga_proto::cmd3d;

use crate::command::Rect;
use crate::layout::{Box3, Size3};

/// One image of a surface: layer (`face`) and mip level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageId {
    /// Surface ID.
    pub sid: u32,
    /// Layer: array slice or cube face.
    pub face: u32,
    /// Mip level.
    pub mipmap: u32,
}

/// A box of a copy (`SVGA3dCopyBox`): destination box and source origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyBox {
    /// Destination box.
    pub dest: Box3,
    /// Source corner matching `dest`'s corner.
    pub src: (u32, u32, u32),
}

/// A surface definition, whatever the command version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceDef {
    /// Surface ID.
    pub sid: u32,
    /// `SVGA3D_SURFACE_*` flags.
    pub flags: u64,
    /// `SVGA3dSurfaceFormat`.
    pub format: u32,
    /// Number of mip levels.
    pub mip_levels: u32,
    /// Multisample count (0 or 1: none).
    pub samples: u32,
    /// Base level size.
    pub size: Size3,
    /// Array size (0: not an array).
    pub array_size: u32,
}

/// A decoded guest-backed object command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GbCommand {
    /// Where the guest put an object table (`SET_OTABLE_BASE[64]`), or its
    /// bigger replacement (`GROW_OTABLE`).
    SetOTable {
        /// `SVGAOTableType`.
        table: u32,
        /// Root page number.
        base: u64,
        /// Size in bytes.
        size: u32,
        /// Bytes holding valid entries.
        valid_size: u32,
        /// `SVGAMobFormat`.
        pt_depth: u32,
    },
    /// The device must write its object state back (`READBACK_OTABLE`).
    ReadbackOTable {
        /// `SVGAOTableType`.
        table: u32,
    },
    /// Defines (or redefines) a MOB.
    DefineMob {
        /// MOB ID.
        id: u32,
        /// `SVGAMobFormat`.
        pt_depth: u32,
        /// Root page number.
        base: u64,
        /// Size in bytes.
        size: u32,
    },
    /// Destroys a MOB.
    DestroyMob {
        /// MOB ID.
        id: u32,
    },
    /// Defines a surface.
    DefineSurface(SurfaceDef),
    /// Destroys a surface.
    DestroySurface {
        /// Surface ID.
        sid: u32,
    },
    /// Binds a surface to a MOB (or unbinds it, with `INVALID_ID`).
    BindSurface {
        /// Surface ID.
        sid: u32,
        /// MOB ID.
        mob: u32,
        /// Row pitch of the base level in the MOB; 0: tightly packed
        /// (`BIND_GB_SURFACE_WITH_PITCH` gives one).
        pitch: u32,
    },
    /// Rebinds a surface if it is bound to `test_mob`.
    CondBindSurface {
        /// Surface ID.
        sid: u32,
        /// Expected current MOB.
        test_mob: u32,
        /// New MOB.
        mob: u32,
        /// `cond_bind` flags.
        flags: u32,
    },
    /// Loads a box of an image from its MOB.
    UpdateImage {
        /// The image.
        image: ImageId,
        /// The box.
        area: Box3,
    },
    /// Loads a whole surface from its MOB.
    UpdateSurface {
        /// Surface ID.
        sid: u32,
    },
    /// Writes an image back to its MOB.
    ReadbackImage {
        /// The image.
        image: ImageId,
    },
    /// Writes a box of an image back to its MOB (`invert`: everything but
    /// the box).
    ReadbackImagePartial {
        /// The image.
        image: ImageId,
        /// The box.
        area: Box3,
        /// Whether the box is what must *not* be read back.
        invert: bool,
    },
    /// Writes a whole surface back to its MOB.
    ReadbackSurface {
        /// Surface ID.
        sid: u32,
    },
    /// The device copy of an image may be discarded.
    InvalidateImage {
        /// The image.
        image: ImageId,
    },
    /// The device copy of a surface may be discarded.
    InvalidateSurface {
        /// Surface ID.
        sid: u32,
    },
    /// Fills a surface with zeroes.
    UpdateZeroSurface {
        /// Surface ID.
        sid: u32,
    },
    /// Copies boxes between two images.
    SurfaceCopy {
        /// Source image.
        src: ImageId,
        /// Destination image.
        dest: ImageId,
        /// The boxes.
        boxes: Vec<CopyBox>,
    },
    /// Uses a MOB as the GART (`ENABLE_GART`).
    EnableGart {
        /// MOB ID.
        mob: u32,
    },
    /// Stops using the GART.
    DisableGart,
    /// Maps a MOB into the GART.
    MapMobIntoGart {
        /// MOB ID.
        mob: u32,
        /// Page offset in the GART.
        offset: u32,
    },
    /// Unmaps GART pages.
    UnmapGartRange {
        /// Page offset in the GART.
        offset: u32,
        /// Number of pages.
        pages: u32,
    },
    /// Defines a screen target.
    DefineScreenTarget {
        /// Screen target ID.
        stid: u32,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Position on the virtual desktop.
        root: (i32, i32),
        /// `stflag` bits.
        flags: u32,
        /// Dots per inch (0: unknown).
        dpi: u32,
    },
    /// Destroys a screen target.
    DestroyScreenTarget {
        /// Screen target ID.
        stid: u32,
    },
    /// Shows an image on a screen target (unbinds with `sid = INVALID_ID`).
    BindScreenTarget {
        /// Screen target ID.
        stid: u32,
        /// The image.
        image: ImageId,
    },
    /// Presents an area of a screen target's image.
    UpdateScreenTarget {
        /// Screen target ID.
        stid: u32,
        /// The area, in screen target pixels.
        rect: Rect,
    },
    /// Writes `value` into a MOB once the previous commands are done.
    MobFence {
        /// The value.
        value: u32,
        /// MOB ID.
        mob: u32,
        /// Byte offset in the MOB.
        offset: u32,
    },
    /// Does nothing.
    Nop,
}

/// A payload too short for its command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortPayload {
    /// Command ID.
    pub id: u32,
    /// Words received.
    pub words: usize,
    /// Words needed.
    pub needed: usize,
}

impl core::fmt::Display for ShortPayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "3D command {} has {} payload words, {} needed",
            self.id, self.words, self.needed
        )
    }
}

fn wide(lo: u32, hi: u32) -> u64 {
    u64::from(lo) | u64::from(hi) << 32
}

fn image(w: &[u32]) -> ImageId {
    ImageId {
        sid: w[0],
        face: w[1],
        mipmap: w[2],
    }
}

fn area(w: &[u32]) -> Box3 {
    Box3 {
        x: w[0],
        y: w[1],
        z: w[2],
        w: w[3],
        h: w[4],
        d: w[5],
    }
}

/// Words of the fixed part of each command this module decodes.
fn fixed_words(id: u32) -> Option<usize> {
    Some(match id {
        cmd3d::NOP | cmd3d::DISABLE_GART => 0,
        cmd3d::READBACK_OTABLE
        | cmd3d::DESTROY_GB_MOB
        | cmd3d::DESTROY_GB_SURFACE
        | cmd3d::UPDATE_GB_SURFACE
        | cmd3d::READBACK_GB_SURFACE
        | cmd3d::INVALIDATE_GB_SURFACE
        | cmd3d::UPDATE_ZERO_SURFACE
        | cmd3d::DESTROY_GB_SCREENTARGET => 1,
        cmd3d::BIND_GB_SURFACE | cmd3d::MAP_MOB_INTO_GART | cmd3d::UNMAP_GART_RANGE => 2,
        cmd3d::READBACK_GB_IMAGE | cmd3d::INVALIDATE_GB_IMAGE | cmd3d::GB_MOB_FENCE => 3,
        cmd3d::BIND_GB_SURFACE_WITH_PITCH => 3,
        cmd3d::ENABLE_GART => 3,
        cmd3d::DEFINE_GB_MOB | cmd3d::COND_BIND_GB_SURFACE | cmd3d::BIND_GB_SCREENTARGET => 4,
        cmd3d::SET_OTABLE_BASE | cmd3d::DEFINE_GB_MOB64 | cmd3d::REDEFINE_GB_MOB64 => 5,
        cmd3d::UPDATE_GB_SCREENTARGET => 5,
        cmd3d::SET_OTABLE_BASE64 | cmd3d::GROW_OTABLE | cmd3d::SURFACE_COPY => 6,
        cmd3d::DEFINE_GB_SCREENTARGET => 7,
        cmd3d::DEFINE_GB_SURFACE | cmd3d::UPDATE_GB_IMAGE => 9,
        cmd3d::READBACK_GB_IMAGE_PARTIAL | cmd3d::INVALIDATE_GB_IMAGE_PARTIAL => 10,
        cmd3d::DEFINE_GB_SURFACE_V2 => 11,
        cmd3d::DEFINE_GB_SURFACE_V3 => 13,
        cmd3d::DEFINE_GB_SURFACE_V4 => 14,
        _ => return None,
    })
}

/// Decodes a guest-backed object command. `None`: not one of them.
pub fn parse(id: u32, payload: &[u32]) -> Option<Result<GbCommand, ShortPayload>> {
    let needed = fixed_words(id)?;
    if payload.len() < needed {
        return Some(Err(ShortPayload {
            id,
            words: payload.len(),
            needed,
        }));
    }
    let w = payload;
    let size = |s: &[u32]| Size3 {
        width: s[0],
        height: s[1],
        depth: s[2],
    };
    Some(Ok(match id {
        cmd3d::NOP => GbCommand::Nop,
        cmd3d::SET_OTABLE_BASE => GbCommand::SetOTable {
            table: w[0],
            base: u64::from(w[1]),
            size: w[2],
            valid_size: w[3],
            pt_depth: w[4],
        },
        cmd3d::SET_OTABLE_BASE64 | cmd3d::GROW_OTABLE => GbCommand::SetOTable {
            table: w[0],
            base: wide(w[1], w[2]),
            size: w[3],
            valid_size: w[4],
            pt_depth: w[5],
        },
        cmd3d::READBACK_OTABLE => GbCommand::ReadbackOTable { table: w[0] },
        cmd3d::DEFINE_GB_MOB => GbCommand::DefineMob {
            id: w[0],
            pt_depth: w[1],
            base: u64::from(w[2]),
            size: w[3],
        },
        cmd3d::DEFINE_GB_MOB64 | cmd3d::REDEFINE_GB_MOB64 => GbCommand::DefineMob {
            id: w[0],
            pt_depth: w[1],
            base: wide(w[2], w[3]),
            size: w[4],
        },
        cmd3d::DESTROY_GB_MOB => GbCommand::DestroyMob { id: w[0] },
        cmd3d::DEFINE_GB_SURFACE | cmd3d::DEFINE_GB_SURFACE_V2 => {
            GbCommand::DefineSurface(SurfaceDef {
                sid: w[0],
                flags: u64::from(w[1]),
                format: w[2],
                mip_levels: w[3],
                samples: w[4],
                size: size(&w[6..9]),
                array_size: if id == cmd3d::DEFINE_GB_SURFACE {
                    0
                } else {
                    w[9]
                },
            })
        }
        cmd3d::DEFINE_GB_SURFACE_V3 | cmd3d::DEFINE_GB_SURFACE_V4 => {
            GbCommand::DefineSurface(SurfaceDef {
                sid: w[0],
                flags: wide(w[1], w[2]),
                format: w[3],
                mip_levels: w[4],
                samples: w[5],
                size: size(&w[9..12]),
                array_size: w[12],
            })
        }
        cmd3d::DESTROY_GB_SURFACE => GbCommand::DestroySurface { sid: w[0] },
        cmd3d::BIND_GB_SURFACE => GbCommand::BindSurface {
            sid: w[0],
            mob: w[1],
            pitch: 0,
        },
        cmd3d::BIND_GB_SURFACE_WITH_PITCH => GbCommand::BindSurface {
            sid: w[0],
            mob: w[1],
            pitch: w[2],
        },
        cmd3d::READBACK_GB_IMAGE_PARTIAL => GbCommand::ReadbackImagePartial {
            image: image(&w[0..3]),
            area: area(&w[3..9]),
            invert: w[9] != 0,
        },
        cmd3d::INVALIDATE_GB_IMAGE_PARTIAL => GbCommand::InvalidateImage {
            image: image(&w[0..3]),
        },
        cmd3d::COND_BIND_GB_SURFACE => GbCommand::CondBindSurface {
            sid: w[0],
            test_mob: w[1],
            mob: w[2],
            flags: w[3],
        },
        cmd3d::UPDATE_GB_IMAGE => GbCommand::UpdateImage {
            image: image(&w[0..3]),
            area: area(&w[3..9]),
        },
        cmd3d::UPDATE_GB_SURFACE => GbCommand::UpdateSurface { sid: w[0] },
        cmd3d::READBACK_GB_IMAGE => GbCommand::ReadbackImage {
            image: image(&w[0..3]),
        },
        cmd3d::READBACK_GB_SURFACE => GbCommand::ReadbackSurface { sid: w[0] },
        cmd3d::INVALIDATE_GB_IMAGE => GbCommand::InvalidateImage {
            image: image(&w[0..3]),
        },
        cmd3d::INVALIDATE_GB_SURFACE => GbCommand::InvalidateSurface { sid: w[0] },
        cmd3d::UPDATE_ZERO_SURFACE => GbCommand::UpdateZeroSurface { sid: w[0] },
        cmd3d::SURFACE_COPY => GbCommand::SurfaceCopy {
            src: image(&w[0..3]),
            dest: image(&w[3..6]),
            boxes: w[6..]
                .chunks_exact(9)
                .map(|b| CopyBox {
                    dest: area(&b[0..6]),
                    src: (b[6], b[7], b[8]),
                })
                .collect(),
        },
        cmd3d::ENABLE_GART => GbCommand::EnableGart { mob: w[0] },
        cmd3d::DISABLE_GART => GbCommand::DisableGart,
        cmd3d::MAP_MOB_INTO_GART => GbCommand::MapMobIntoGart {
            mob: w[0],
            offset: w[1],
        },
        cmd3d::UNMAP_GART_RANGE => GbCommand::UnmapGartRange {
            offset: w[0],
            pages: w[1],
        },
        cmd3d::DEFINE_GB_SCREENTARGET => GbCommand::DefineScreenTarget {
            stid: w[0],
            width: w[1],
            height: w[2],
            root: (w[3].cast_signed(), w[4].cast_signed()),
            flags: w[5],
            dpi: w[6],
        },
        cmd3d::DESTROY_GB_SCREENTARGET => GbCommand::DestroyScreenTarget { stid: w[0] },
        cmd3d::BIND_GB_SCREENTARGET => GbCommand::BindScreenTarget {
            stid: w[0],
            image: image(&w[1..4]),
        },
        cmd3d::UPDATE_GB_SCREENTARGET => GbCommand::UpdateScreenTarget {
            stid: w[0],
            rect: Rect {
                x: w[1],
                y: w[2],
                width: w[3],
                height: w[4],
            },
        },
        cmd3d::GB_MOB_FENCE => GbCommand::MobFence {
            value: w[0],
            mob: w[1],
            offset: w[2],
        },
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobs_tables_and_surfaces() {
        assert_eq!(
            parse(cmd3d::DEFINE_GB_MOB64, &[3, 5, 0x1234, 0x1, 8192]),
            Some(Ok(GbCommand::DefineMob {
                id: 3,
                pt_depth: 5,
                base: 0x1_0000_1234,
                size: 8192
            }))
        );
        assert_eq!(
            parse(cmd3d::SET_OTABLE_BASE64, &[1, 0x99, 0, 4096, 4096, 4]),
            Some(Ok(GbCommand::SetOTable {
                table: 1,
                base: 0x99,
                size: 4096,
                valid_size: 4096,
                pt_depth: 4
            }))
        );
        // V4: sid, flags (2 words), format, mips, samples, pattern, quality,
        // filter, size (3), arraySize, bufferByteStride.
        let v4 = [7, 0x200, 1, 142, 1, 0, 0, 0, 0, 1920, 1080, 1, 0, 0];
        assert_eq!(
            parse(cmd3d::DEFINE_GB_SURFACE_V4, &v4),
            Some(Ok(GbCommand::DefineSurface(SurfaceDef {
                sid: 7,
                flags: 0x1_0000_0200,
                format: 142,
                mip_levels: 1,
                samples: 0,
                size: Size3 {
                    width: 1920,
                    height: 1080,
                    depth: 1
                },
                array_size: 0
            })))
        );
    }

    #[test]
    fn copies_and_screen_targets() {
        let copy = [1, 0, 0, 2, 0, 0, 10, 20, 0, 4, 4, 1, 1, 2, 0];
        assert_eq!(
            parse(cmd3d::SURFACE_COPY, &copy),
            Some(Ok(GbCommand::SurfaceCopy {
                src: ImageId {
                    sid: 1,
                    face: 0,
                    mipmap: 0
                },
                dest: ImageId {
                    sid: 2,
                    face: 0,
                    mipmap: 0
                },
                boxes: vec![CopyBox {
                    dest: Box3 {
                        x: 10,
                        y: 20,
                        z: 0,
                        w: 4,
                        h: 4,
                        d: 1
                    },
                    src: (1, 2, 0)
                }]
            }))
        );
        assert_eq!(
            parse(
                cmd3d::DEFINE_GB_SCREENTARGET,
                &[0, 800, 600, u32::MAX, 0, 1, 96]
            ),
            Some(Ok(GbCommand::DefineScreenTarget {
                stid: 0,
                width: 800,
                height: 600,
                root: (-1, 0),
                flags: 1,
                dpi: 96
            }))
        );
        assert_eq!(
            parse(cmd3d::UPDATE_GB_SCREENTARGET, &[0, 1, 2, 3, 4]),
            Some(Ok(GbCommand::UpdateScreenTarget {
                stid: 0,
                rect: Rect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4
                }
            }))
        );
    }

    #[test]
    fn short_and_foreign_commands() {
        assert_eq!(
            parse(cmd3d::UPDATE_GB_IMAGE, &[1, 2]),
            Some(Err(ShortPayload {
                id: cmd3d::UPDATE_GB_IMAGE,
                words: 2,
                needed: 9
            }))
        );
        assert_eq!(parse(cmd3d::DX_DEFINE_CONTEXT, &[1]), None);
        assert_eq!(parse(cmd3d::NOP, &[]), Some(Ok(GbCommand::Nop)));
        assert_eq!(
            parse(cmd3d::BIND_GB_SURFACE_WITH_PITCH, &[1, 4, 7680]),
            Some(Ok(GbCommand::BindSurface {
                sid: 1,
                mob: 4,
                pitch: 7680
            }))
        );
    }
}
