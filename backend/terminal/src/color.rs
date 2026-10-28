//! Colours on the terminal: 24-bit, or the nearest of the 256-colour
//! palette (the 6 × 6 × 6 cube and the grey ramp, chosen as tmux chooses),
//! or of the 16 basic colours (the Linux console), and the SGR parameters
//! that set them.

/// A cell colour: the terminal's own, an RGB colour, or an index of the
/// 256-colour palette (an RGB colour already reduced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Rgb(u8, u8, u8),
    Index(u8),
}

/// The colours a terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Ansi256,
    Ansi16,
    /// No colour or attribute at all (characters only).
    Mono,
}

impl Depth {
    pub fn from_name(s: &str) -> Option<Depth> {
        match s {
            "truecolor" | "24bit" => Some(Depth::TrueColor),
            "256" => Some(Depth::Ansi256),
            "16" => Some(Depth::Ansi16),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Depth::TrueColor => "truecolor",
            Depth::Ansi256 => "256",
            Depth::Ansi16 => "16",
            Depth::Mono => "none",
        }
    }

    /// `c` as this depth shows it (RGB kept in 24-bit, an index otherwise).
    pub fn reduce(self, c: [u8; 3]) -> Color {
        match self {
            Depth::TrueColor => Color::Rgb(c[0], c[1], c[2]),
            Depth::Ansi256 => Color::Index(ansi256(c)),
            Depth::Ansi16 => Color::Index(ansi16(c)),
            Depth::Mono => Color::Default,
        }
    }
}

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    a.iter().zip(b).map(|(&x, y)| (x as i32 - y as i32).pow(2) as u32).sum()
}

fn cube_level(v: u8) -> usize {
    match v {
        0..=47 => 0,
        48..=114 => 1,
        _ => (v as usize - 35) / 40,
    }
}

/// The RGB value of a palette index (16 basic colours as xterm sets them).
pub fn palette_rgb(i: u8) -> [u8; 3] {
    match i {
        0..=15 => BASIC[i as usize],
        16..=231 => {
            let n = i as usize - 16;
            [CUBE[n / 36], CUBE[n / 6 % 6], CUBE[n % 6]]
        }
        _ => {
            let g = 8 + 10 * (i - 232);
            [g, g, g]
        }
    }
}

/// The nearest colour of the 256-colour palette among the cube and the
/// grey ramp (indices 16 to 255; the first 16 vary between terminals).
pub fn ansi256(c: [u8; 3]) -> u8 {
    let q = [cube_level(c[0]), cube_level(c[1]), cube_level(c[2])];
    let cube = [CUBE[q[0]], CUBE[q[1]], CUBE[q[2]]];
    let cube_index = 16 + 36 * q[0] + 6 * q[1] + q[2];
    let average = (c[0] as u32 + c[1] as u32 + c[2] as u32) / 3;
    let grey_index = if average > 238 { 23 } else { (average.saturating_sub(3) / 10) as u8 };
    let g = 8 + 10 * grey_index;
    if distance(c, [g, g, g]) < distance(c, cube) {
        232 + grey_index
    } else {
        cube_index as u8
    }
}

/// xterm's 16 basic colours.
const BASIC: [[u8; 3]; 16] = [
    [0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0], [0, 0, 238], [205, 0, 205], [0, 205, 205], [229, 229, 229],
    [127, 127, 127], [255, 0, 0], [0, 255, 0], [255, 255, 0], [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
];

/// The nearest of the 16 basic colours.
pub fn ansi16(c: [u8; 3]) -> u8 {
    (0..16).min_by_key(|&i| distance(c, BASIC[i as usize])).unwrap_or(0)
}

/// Append the SGR parameter(s) setting `c` as foreground or background
/// (without the CSI and the final `m`).
pub fn sgr(c: Color, foreground: bool, depth: Depth, out: &mut Vec<u8>) {
    use std::io::Write;
    let base = if foreground { 30 } else { 40 };
    let _ = match (c, depth) {
        (_, Depth::Mono) => return,
        (Color::Default, _) => write!(out, "{}", base + 9),
        (Color::Rgb(r, g, b), Depth::TrueColor) => write!(out, "{};2;{r};{g};{b}", base + 8),
        (Color::Rgb(r, g, b), d) => return sgr(d.reduce([r, g, b]), foreground, d, out),
        (Color::Index(i), Depth::Ansi16) if i >= 16 => return sgr(Color::Index(ansi16(palette_rgb(i))), foreground, Depth::Ansi16, out),
        (Color::Index(i), Depth::Ansi16) if i >= 8 => write!(out, "{}", base + 60 + (i - 8) as i32),
        (Color::Index(i), Depth::Ansi16) => write!(out, "{}", base + i as i32),
        (Color::Index(i), _) => write!(out, "{};5;{i}", base + 8),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_256() {
        // Greys land on the ramp, pure colours on the cube.
        assert_eq!(ansi256([0, 0, 0]), 16);
        assert_eq!(ansi256([255, 255, 255]), 231);
        assert_eq!(ansi256([128, 128, 128]), 244);
        assert_eq!(palette_rgb(244), [128, 128, 128]);
        assert_eq!(ansi256([255, 0, 0]), 196);
        assert_eq!(ansi256([220, 216, 20]), 184);
        // Every index of the cube and of the ramp maps to itself.
        for i in 16..=255u8 {
            let c = palette_rgb(i);
            assert_eq!(palette_rgb(ansi256(c)), c, "index {i}");
        }
        // Every grey level is within 13 of the colour it is shown with.
        for v in 0..=255u8 {
            let s = palette_rgb(ansi256([v, v, v]));
            assert!(s.iter().all(|&x| (x as i32 - v as i32).abs() <= 13), "{v} -> {s:?}");
        }
    }

    #[test]
    fn sixteen_and_sgr() {
        assert_eq!(ansi16([250, 250, 250]), 15);
        assert_eq!(ansi16([10, 10, 10]), 0);
        let s = |c, fg, d| {
            let mut v = Vec::new();
            sgr(c, fg, d, &mut v);
            String::from_utf8(v).unwrap()
        };
        assert_eq!(s(Color::Rgb(1, 2, 3), true, Depth::TrueColor), "38;2;1;2;3");
        assert_eq!(s(Color::Rgb(255, 0, 0), false, Depth::Ansi256), "48;5;196");
        assert_eq!(s(Color::Index(9), true, Depth::Ansi16), "91");
        assert_eq!(s(Color::Index(2), false, Depth::Ansi16), "42");
        assert_eq!(s(Color::Index(196), true, Depth::Ansi16), "91");
        assert_eq!(s(Color::Default, false, Depth::TrueColor), "49");
    }
}
