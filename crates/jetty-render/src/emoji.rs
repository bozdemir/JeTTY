//! Which grid text renders as a COLOR emoji.
//!
//! Only what Unicode says defaults to emoji presentation (the
//! `Emoji_Presentation` property — 😀, ✅, ⚡ …) or explicitly asks for it (a
//! VARIATION SELECTOR-16 after the base, `❤️`), plus ZWJ sequences built on an
//! emoji. Text-default symbols a terminal UI uses as plain glyphs — ✔ ❤ ☐ ⏺ —
//! stay on the text font unless followed by VS16.

/// `Emoji_Presentation=Yes` ranges (Unicode 16.0 emoji-data.txt), sorted.
const EMOJI_PRESENTATION: &[(u32, u32)] = &[
    (0x231A, 0x231B),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F1E6, 0x1F1FF),
    (0x1F201, 0x1F201),
    (0x1F21A, 0x1F21A),
    (0x1F22F, 0x1F22F),
    (0x1F232, 0x1F236),
    (0x1F238, 0x1F23A),
    (0x1F250, 0x1F251),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA89),
    (0x1FA8F, 0x1FAC6),
    (0x1FACE, 0x1FADC),
    (0x1FADF, 0x1FAE9),
    (0x1FAF0, 0x1FAF8),
];

/// VARIATION SELECTOR-16: "show the preceding char as an emoji".
pub const VS16: char = '\u{FE0F}';
/// VARIATION SELECTOR-15: "show the preceding char as text".
pub const VS15: char = '\u{FE0E}';

/// Whether `c` defaults to emoji presentation (`Emoji_Presentation=Yes`).
pub fn is_emoji_presentation(c: char) -> bool {
    let cp = c as u32;
    if cp < 0x231A {
        return false;
    }
    EMOJI_PRESENTATION
        .binary_search_by(|&(a, b)| {
            if b < cp {
                std::cmp::Ordering::Less
            } else if a > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Whether a grid cell's grapheme cluster (base char + its zero-width chars)
/// renders as a color emoji: an explicit VS16 (`❤️`, `#️⃣`), or an
/// emoji-presentation base (a ZWJ sequence's first part, `👨‍`) — never with an
/// explicit VS15 (text presentation).
pub fn is_emoji_cluster(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(base) = chars.next() else { return false };
    let mut vs16 = false;
    for c in chars {
        if c == VS15 {
            return false;
        }
        vs16 |= c == VS16;
    }
    vs16 || is_emoji_presentation(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_disjoint() {
        for w in EMOJI_PRESENTATION.windows(2) {
            assert!(w[0].0 <= w[0].1 && w[0].1 < w[1].0, "{:X?}", w);
        }
    }

    #[test]
    fn emoji_presentation_classifies_common_chars() {
        for c in ['😀', '✅', '⚡', '🚀', '🦀', '👍', '🔥', '⌛', '⭐', '🟢', '🫠', '🪿', '🇹'] {
            assert!(is_emoji_presentation(c), "{c} defaults to emoji");
        }
        // Text-default symbols a TUI uses as plain glyphs stay text.
        for c in ['✔', '❤', '☐', '⏺', '✻', '⎿', '★', '©', '™', '↑', '•', 'a', '─', '\u{E0B0}', '⚠'] {
            assert!(!is_emoji_presentation(c), "{c} defaults to text");
        }
    }

    #[test]
    fn clusters_follow_the_variation_selectors() {
        assert!(is_emoji_cluster("❤\u{FE0F}"), "VS16 asks for emoji");
        assert!(is_emoji_cluster("#\u{FE0F}\u{20E3}"), "keycap");
        assert!(is_emoji_cluster("👨\u{200D}"), "ZWJ part on an emoji base");
        assert!(!is_emoji_cluster("❤"), "text-default without VS16");
        assert!(!is_emoji_cluster("⌚\u{FE0E}"), "VS15 asks for text");
        assert!(!is_emoji_cluster("e\u{301}"), "combining accent");
        assert!(!is_emoji_cluster(""));
    }

    /// Range edges (the table was cross-checked against Unicode's emoji-test.txt:
    /// every single-code-point fully-qualified emoji is in it, every one that
    /// needs a VS16 to be fully-qualified is not).
    #[test]
    fn range_edges() {
        for (cp, yes) in [
            (0x2319, false),
            (0x231A, true),
            (0x231B, true),
            (0x231C, false),
            (0x2764, false), // ❤ needs VS16
            (0x1F336, false), // 🌶 needs VS16
            (0x1F337, true),
            (0x1F3F3, false), // 🏳 needs VS16
            (0x1F3F4, true),
            (0x1F5A4, true),
            (0x1F5A5, false), // 🖥 needs VS16
            (0x1FAF8, true),
            (0x1FAF9, false),
        ] {
            assert_eq!(is_emoji_presentation(char::from_u32(cp).unwrap()), yes, "U+{cp:04X}");
        }
    }
}
