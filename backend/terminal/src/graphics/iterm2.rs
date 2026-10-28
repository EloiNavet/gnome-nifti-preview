//! iTerm2's inline images (also WezTerm's): a PNG in an OSC 1337 sequence,
//! scaled by the terminal to `width × height` cells.

use super::base64;

pub fn inline(png: &[u8], cols: usize, rows: usize) -> Vec<u8> {
    let mut s = format!("\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=0:", png.len()).into_bytes();
    s.extend_from_slice(&base64::encode(png));
    s.push(0x07);
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn osc_1337() {
        assert_eq!(super::inline(b"PNG", 4, 2), b"\x1b]1337;File=inline=1;size=3;width=4;height=2;preserveAspectRatio=0:UE5H\x07");
    }
}
