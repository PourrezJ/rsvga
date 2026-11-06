#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    missing_docs
)]

use std::cell::Cell;

use svga_core::command::{Command, GuestPtr, PageSource, Point, Rect, SignedRect};
use svga_core::{Decode, DecodeError, Limits, WordSource, decode};
use svga_proto::{cmd, remap_gmr2};

/// Words that are readable now, inside a larger ring.
struct Partial<'a> {
    words: &'a [u32],
    capacity: usize,
}

impl WordSource for Partial<'_> {
    fn available(&self) -> usize {
        self.words.len()
    }
    fn capacity(&self) -> usize {
        self.capacity
    }
    fn peek(&self, offset: usize) -> Option<u32> {
        self.words.get(offset).copied()
    }
}

fn ready(words: &[u32]) -> (Command, usize) {
    match decode(words, &Limits::default()) {
        Decode::Ready { command, words } => (command, words),
        other => panic!("expected a command, got {other:?}"),
    }
}

fn invalid(words: &[u32]) -> DecodeError {
    match decode(words, &Limits::default()) {
        Decode::Invalid(error) => error,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn update() {
    let (command, words) = ready(&[cmd::UPDATE, 1, 2, 3, 4, 0xdead]);
    assert_eq!(words, 5);
    assert_eq!(
        command,
        Command::Update(Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4
        })
    );
}

#[test]
fn fixed_size_commands_have_the_reference_lengths() {
    let cases = [
        (cmd::UPDATE, 5),
        (cmd::RECT_COPY, 7),
        (cmd::RECT_ROP_COPY, 8),
        (cmd::UPDATE_VERBOSE, 6),
        (cmd::FRONT_ROP_FILL, 7),
        (cmd::FENCE, 2),
        (cmd::DESTROY_SCREEN, 2),
        (cmd::DEFINE_GMRFB, 5),
        (cmd::BLIT_GMRFB_TO_SCREEN, 8),
        (cmd::BLIT_SCREEN_TO_GMRFB, 8),
        (cmd::ANNOTATION_FILL, 2),
        (cmd::ANNOTATION_COPY, 4),
        (cmd::DEFINE_GMR2, 3),
        (cmd::NOP, 1),
        (cmd::NOP_ERROR, 1),
    ];
    for (id, expected) in cases {
        let mut words = vec![0u32; 16];
        words[0] = id;
        assert_eq!(ready(&words).1, expected, "command {id}");
    }
}

#[test]
fn signed_fields_and_blits() {
    let (command, _) = ready(&[
        cmd::BLIT_GMRFB_TO_SCREEN,
        (-5i32) as u32,
        7,
        0,
        0,
        640,
        480,
        2,
    ]);
    assert_eq!(
        command,
        Command::BlitGmrFbToScreen {
            src_origin: Point { x: -5, y: 7 },
            dst_rect: SignedRect {
                left: 0,
                top: 0,
                right: 640,
                bottom: 480
            },
            dst_screen: 2,
        }
    );
}

#[test]
fn gmrfb_format_is_split_into_bytes() {
    let (command, _) = ready(&[cmd::DEFINE_GMRFB, 7, 0x100, 2560, 0x0000_1820]);
    assert_eq!(
        command,
        Command::DefineGmrFb {
            ptr: GuestPtr {
                gmr_id: 7,
                offset: 0x100
            },
            bytes_per_line: 2560,
            bits_per_pixel: 32,
            color_depth: 24,
            format_reserved: 0,
        }
    );
}

#[test]
fn define_cursor_masks_are_sized_per_scanline() {
    // 2x2 cursor: AND mask at 1 bpp -> 1 word per line; XOR at 32 bpp -> 2.
    let mut words = vec![cmd::DEFINE_CURSOR, 0, 1, 1, 2, 2, 1, 32];
    words.extend([0xa, 0xb]); // AND mask
    words.extend([1, 2, 3, 4]); // XOR mask
    let (command, len) = ready(&words);
    assert_eq!(len, 14);
    let Command::DefineCursor(cursor) = command else {
        panic!("not a cursor")
    };
    assert_eq!(cursor.and_mask, [0xa, 0xb]);
    assert_eq!(cursor.xor_mask, [1, 2, 3, 4]);
}

#[test]
fn define_cursor_rejects_bad_sizes_and_depths() {
    assert_eq!(
        invalid(&[cmd::DEFINE_CURSOR, 0, 0, 0, 257, 1, 1, 1]),
        DecodeError::CursorTooLarge {
            width: 257,
            height: 1
        }
    );
    assert_eq!(
        invalid(&[cmd::DEFINE_CURSOR, 0, 0, 0, 1, u32::MAX, 1, 1]),
        DecodeError::CursorTooLarge {
            width: 1,
            height: u32::MAX
        }
    );
    assert_eq!(
        invalid(&[cmd::DEFINE_CURSOR, 0, 0, 0, 1, 1, 0, 1]),
        DecodeError::InvalidCursorDepth(0)
    );
    assert_eq!(
        invalid(&[cmd::DEFINE_CURSOR, 0, 0, 0, 1, 1, 1, 33]),
        DecodeError::InvalidCursorDepth(33)
    );
}

#[test]
fn alpha_cursor() {
    let mut words = vec![cmd::DEFINE_ALPHA_CURSOR, 0, 1, 2, 3, 2];
    words.extend(1..=6);
    let (command, len) = ready(&words);
    assert_eq!(len, 12);
    assert_eq!(
        command,
        Command::DefineAlphaCursor {
            hotspot_x: 1,
            hotspot_y: 2,
            width: 3,
            height: 2,
            pixels: (1..=6).collect()
        }
    );
}

#[test]
fn escape_keeps_exactly_size_bytes() {
    let (command, len) = ready(&[cmd::ESCAPE, 0x8000, 5, 0x0403_0201, 0xffff_ff05]);
    assert_eq!(len, 5);
    assert_eq!(
        command,
        Command::Escape {
            nsid: 0x8000,
            data: vec![1, 2, 3, 4, 5]
        }
    );
    assert_eq!(
        invalid(&[cmd::ESCAPE, 0, cmd::MAX_DATASIZE + 1]),
        DecodeError::EscapeTooLarge(cmd::MAX_DATASIZE + 1)
    );
}

#[test]
fn define_screen_versions() {
    let v1 = [cmd::DEFINE_SCREEN, 28, 0, 3, 1024, 768, 0, 0];
    let Command::DefineScreen(screen) = ready(&v1).0 else {
        panic!()
    };
    assert_eq!((screen.id, screen.width, screen.height), (0, 1024, 768));
    assert_eq!((screen.backing_store, screen.clone_count), (None, None));

    let v3 = [
        cmd::DEFINE_SCREEN,
        44,
        1,
        3,
        800,
        600,
        -10i32 as u32,
        20,
        5,
        64,
        3200,
        2,
    ];
    let (command, len) = ready(&v3);
    assert_eq!(len, 12);
    let Command::DefineScreen(screen) = command else {
        panic!()
    };
    assert_eq!(screen.root, Point { x: -10, y: 20 });
    assert_eq!(
        screen
            .backing_store
            .map(|b| (b.ptr.gmr_id, b.ptr.offset, b.pitch)),
        Some((5, 64, 3200))
    );
    assert_eq!(screen.clone_count, Some(2));

    for bad in [24, 30, 260] {
        assert_eq!(
            invalid(&[cmd::DEFINE_SCREEN, bad]),
            DecodeError::InvalidScreenObjectSize(bad)
        );
    }
}

#[test]
fn remap_gmr2_page_sources() {
    let via = [cmd::REMAP_GMR2, 1, remap_gmr2::VIA_GMR, 0, 1000, 9, 16];
    let (command, len) = ready(&via);
    assert_eq!(len, 7);
    assert!(matches!(
        command,
        Command::RemapGmr2 {
            pages: PageSource::ViaGmr(GuestPtr {
                gmr_id: 9,
                offset: 16
            }),
            ..
        }
    ));

    let ppn32 = [cmd::REMAP_GMR2, 1, remap_gmr2::PPN32, 0, 3, 10, 11, 12];
    let Command::RemapGmr2 { pages, .. } = ready(&ppn32).0 else {
        panic!()
    };
    assert_eq!(pages, PageSource::Inline(vec![10, 11, 12]));

    let ppn64 = [
        cmd::REMAP_GMR2,
        1,
        remap_gmr2::PPN64,
        0,
        2,
        1,
        0x10,
        2,
        0x20,
    ];
    let Command::RemapGmr2 { pages, .. } = ready(&ppn64).0 else {
        panic!()
    };
    assert_eq!(
        pages,
        PageSource::Inline(vec![0x10_0000_0001, 0x20_0000_0002])
    );

    let single = [
        cmd::REMAP_GMR2,
        1,
        remap_gmr2::PPN64 | remap_gmr2::SINGLE_PPN,
        0,
        500,
        7,
        0,
    ];
    let (command, len) = ready(&single);
    assert_eq!(len, 7);
    assert!(matches!(command, Command::RemapGmr2 { num_pages: 500, .. }));
}

#[test]
fn remap_gmr2_larger_than_the_ring_is_rejected() {
    let words = [cmd::REMAP_GMR2, 1, remap_gmr2::PPN64, 0, u32::MAX];
    let source = Partial {
        words: &words,
        capacity: 1024,
    };
    assert!(matches!(
        decode(&source, &Limits::default()),
        Decode::Invalid(DecodeError::CommandTooLarge {
            id: cmd::REMAP_GMR2,
            ..
        })
    ));
}

#[test]
fn command_3d_framing_and_padding() {
    let (command, len) = ready(&[1040, 6, 0x4433_2211, 0xffff_6655, 99]);
    assert_eq!(len, 4);
    assert_eq!(
        command,
        Command::Command3d {
            id: 1040,
            size: 6,
            payload: vec![0x4433_2211, 0x6655]
        }
    );
    assert_eq!(
        invalid(&[1100, cmd::MAX_DATASIZE + 4]),
        DecodeError::Payload3dTooLarge {
            id: 1100,
            size: cmd::MAX_DATASIZE + 4
        }
    );
}

#[test]
fn unknown_and_obsolete_ids_are_rejected() {
    assert_eq!(invalid(&[0]), DecodeError::UnknownCommand(0));
    assert_eq!(invalid(&[2, 0, 0]), DecodeError::UnknownCommand(2));
    assert_eq!(invalid(&[3000, 0]), DecodeError::UnknownCommand(3000));
    assert_eq!(
        invalid(&[cmd::DEAD]),
        DecodeError::ObsoleteCommand(cmd::DEAD)
    );
}

#[test]
fn partial_commands_are_incomplete_not_errors() {
    let limits = Limits::default();
    for available in 0..5 {
        let words = [cmd::UPDATE, 1, 2, 3, 4];
        let source = Partial {
            words: &words[..available],
            capacity: 100,
        };
        assert_eq!(
            decode(&source, &limits),
            Decode::Incomplete,
            "{available} words"
        );
    }
    // The header needed for the length is missing too.
    let source = Partial {
        words: &[cmd::ESCAPE, 0],
        capacity: 100,
    };
    assert_eq!(decode(&source, &limits), Decode::Incomplete);
}

#[test]
fn a_command_that_can_never_fit_is_an_error() {
    let words = [cmd::UPDATE, 1, 2];
    let source = Partial {
        words: &words,
        capacity: 3,
    };
    assert!(matches!(
        decode(&source, &Limits::default()),
        Decode::Invalid(DecodeError::CommandTooLarge {
            id: cmd::UPDATE,
            words: 5
        })
    ));
}

/// Returns a different cursor width on every read of word 4, like a guest
/// rewriting the command while the device copies it.
struct Racy {
    words: Vec<u32>,
    reads: Cell<u32>,
}

impl WordSource for Racy {
    fn available(&self) -> usize {
        self.words.len()
    }
    fn capacity(&self) -> usize {
        self.words.len()
    }
    fn peek(&self, offset: usize) -> Option<u32> {
        if offset == 4 {
            self.reads.set(self.reads.get() + 1);
            return Some(self.reads.get());
        }
        self.words.get(offset).copied()
    }
}

#[test]
fn a_command_changed_while_being_read_is_rejected() {
    let mut words = vec![cmd::DEFINE_ALPHA_CURSOR, 0, 0, 0, 0, 1];
    words.extend([0; 8]);
    let source = Racy {
        words,
        reads: Cell::new(0),
    };
    assert_eq!(
        decode(&source, &Limits::default()),
        Decode::Invalid(DecodeError::ChangedWhileReading {
            id: cmd::DEFINE_ALPHA_CURSOR
        })
    );
}
