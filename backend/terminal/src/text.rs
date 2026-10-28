//! Text on the terminal: which characters may be written (no control
//! character, so that a file name or a header field cannot send an escape
//! sequence), how many cells a character takes, and cutting to a width.

/// Whether `c` may be written to the terminal: not a C0 or C1 control, DEL,
/// or a bidirectional override (the characters `header::clean_text` of the
/// library removes from header text).
pub fn printable(c: char) -> bool {
    !matches!(c,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `s` without the characters that may not be written (tabs and line
/// breaks become spaces).
pub fn clean(s: &str) -> String {
    s.chars().filter_map(|c| match c {
        '\t' | '\n' | '\r' => Some(' '),
        c if printable(c) => Some(c),
        _ => None,
    }).collect()
}

/// Cells taken by `c` in a terminal: 0 for combining marks and other
/// zero-width characters, 2 for East Asian wide and fullwidth characters
/// and emoji, 1 otherwise (the ranges terminals agree on).
pub fn width(c: char) -> usize {
    let u = c as u32;
    let zero = matches!(u,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064
        | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0xFEFF);
    if zero {
        return 0;
    }
    // East Asian Wide and Fullwidth (Unicode 15): scripts, then the emoji
    // shown wide by default.
    let wide = matches!(u,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE10..=0xFE19
        | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x16FE0..=0x18CFF | 0x1B000..=0x1B2FF
        | 0x231A..=0x231B | 0x2329..=0x232A | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD..=0x25FE
        | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 | 0x26AA..=0x26AB
        | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2..=0x26F3 | 0x26F5
        | 0x26FA | 0x26FD | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755
        | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
        | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A | 0x1F200..=0x1F2FF | 0x1F300..=0x1F64F
        | 0x1F680..=0x1F6FF | 0x1F7E0..=0x1F7EB | 0x1F90C..=0x1F9FF | 0x1FA70..=0x1FAFF
        | 0x20000..=0x3FFFD);
    if wide { 2 } else { 1 }
}

/// Cells taken by `s` (printable characters only).
pub fn str_width(s: &str) -> usize {
    s.chars().filter(|&c| printable(c)).map(width).sum()
}

/// `s` cut to at most `w` cells, ending with "…" when cut.
pub fn fit(s: &str, w: usize) -> String {
    if str_width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars().filter(|&c| printable(c)) {
        let cw = width(c);
        if used + cw > w - 1 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_never_reach_the_terminal() {
        assert_eq!(clean("a\x1b]52;c;aGk=\x07b\u{9b}c\u{202e}d\te"), "a]52;c;aGk=bcd e");
        assert!(!printable('\x1b') && !printable('\u{85}') && printable('é') && printable('▄'));
    }

    #[test]
    fn widths_and_cuts() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("山田太郎"), 8);
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(str_width("🚀🧠⌚"), 6, "emoji shown wide");
        assert_eq!(str_width("▄■✓"), 3, "block and geometric characters stay narrow");
        assert_eq!(fit("Left-Hippocampus", 8), "Left-Hi…");
        assert_eq!(fit("山田太郎", 5), "山田…");
        assert_eq!(fit("abc", 3), "abc");
        assert_eq!(fit("abcd", 0), "");
    }
}
