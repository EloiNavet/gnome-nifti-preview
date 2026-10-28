//! Pictures drawn by the terminal itself with an image protocol: Kitty's
//! graphics protocol, Sixel, or iTerm2's inline images. Each picture is
//! drawn at the cursor over the cells it covers; the cursor is saved and
//! restored around it, since the protocols leave it in different places.
//! In tmux, Kitty and iTerm2 sequences are wrapped for tmux to pass them on
//! (`allow-passthrough on`); Sixel is drawn by tmux itself (3.4 and later).

pub mod base64;
pub mod iterm2;
pub mod kitty;
pub mod sixel;

use crate::caps::{Caps, Graphics};
use crate::picture::Picture;
use crate::screen::Rect;

/// zlib level of the PNGs sent: the fastest that still compresses (a
/// picture is sent once, while the user waits).
const PNG_LEVEL: u32 = 1;

/// Draw `pic` over the cells `at` (its size in cells), at the cursor. `id`
/// names a Kitty image so that it can be replaced (0: none).
pub fn draw(pic: &Picture, at: Rect, caps: &Caps, id: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&encode(pic, at, caps, id));
}

/// The bytes `draw` writes (kept by the viewer to draw the same picture
/// again without encoding it again).
pub fn encode(pic: &Picture, at: Rect, caps: &Caps, id: u32) -> Vec<u8> {
    let png = || gnome_nifti_preview::png::encode_rgb_with(pic.w, pic.h, &pic.rgb, PNG_LEVEL);
    let sequences = match caps.graphics {
        Graphics::Kitty => kitty::transmit(&png(), id, at.w, at.h),
        Graphics::Iterm2 => vec![iterm2::inline(&png(), at.w, at.h)],
        Graphics::Sixel => vec![sixel::encode(pic, caps.registers)],
        Graphics::Blocks | Graphics::Ascii => return Vec::new(),
    };
    let mut out = b"\x1b7".to_vec();
    for s in &sequences {
        put(s, caps, &mut out);
    }
    out.extend_from_slice(b"\x1b8");
    out
}

/// Write one sequence, through tmux when it would not pass it on itself.
pub fn put(seq: &[u8], caps: &Caps, out: &mut Vec<u8>) {
    if caps.tmux && caps.graphics != Graphics::Sixel {
        tmux_passthrough(seq, out);
    } else {
        out.extend_from_slice(seq);
    }
}

/// tmux's passthrough: `DCS tmux; …ST` with every ESC of the sequence doubled.
pub fn tmux_passthrough(seq: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_doubles_escapes() {
        let mut out = Vec::new();
        tmux_passthrough(b"\x1b_Ga=T;AAAA\x1b\\", &mut out);
        assert_eq!(out, b"\x1bPtmux;\x1b\x1b_Ga=T;AAAA\x1b\x1b\\\x1b\\");
    }
}
