//! Text JeTTY did not write — a program's OSC 0/2 title, the name of the
//! process in the foreground, a directory name, a command's last output line,
//! a link's target — made safe to show in the chrome: the tab bar, the OS
//! window title, the palette, dialogs, notifications and the link pill.
//!
//! The chrome lays text out with the Unicode bidi algorithm (cosmic-text), so
//! besides the control characters (a newline started a second line drawn over
//! the grid) the invisible FORMAT characters matter: an override or isolate
//! (RLO, LRI …) reorders what follows it — a title or a link's host then reads
//! as something else — and a zero-width space or BOM hides a difference. One
//! rule ([`hides_text`]) decides for every such string.

/// Most chars a title (an OSC 0/2 title, a smart tab title) keeps; the cap
/// also bounds the per-tab title hashing in the app's tab-bar cache.
pub const TITLE_MAX_CHARS: usize = 256;

/// Most chars a link-target preview ([`link_preview`]) is long — what the
/// pill shows whole in a window of ordinary width.
pub const LINK_PREVIEW_MAX_CHARS: usize = 96;

/// Whether `c` must not reach the chrome as it is: a control character (C0,
/// DEL, C1), or an invisible format character that reorders or hides text —
/// the bidi controls (ALM, LRM, RLM, the embeddings and overrides LRE … RLO,
/// the isolates LRI … PDI), the zero-width space, word joiner and invisible
/// operators, the soft hyphen, the BOM, the line and paragraph separators,
/// the deprecated format controls, interlinear annotations, musical
/// formatting and the tag characters. ZWJ and ZWNJ stay (emoji sequences,
/// Persian and Indic text need them), and so do the variation selectors
/// (VS16 makes an emoji colorful).
pub fn hides_text(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// `s` as one line of chrome text: every [`hides_text`] char dropped, at most
/// `max_chars` chars kept, whitespace trimmed. `None` when nothing is left
/// (an OSC title of only such chars resets the tab's title).
pub fn display(s: &str, max_chars: usize) -> Option<String> {
    let kept: String = s.chars().filter(|&c| !hides_text(c)).take(max_chars).collect();
    let t = kept.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A link's target as the Ctrl+hover pill shows it — where a click really
/// goes, in at most [`LINK_PREVIEW_MAX_CHARS`] chars. The pill keeps a long
/// text's head, so the HOST comes first: `https://good.example:443-login-…
/// @evil.example/` read as good.example; a user part (`user@` before the
/// host, which browsers drop without a word) is shown as `…@`, a host longer
/// than half the preview keeps its end (where the registered domain is), and
/// a long path is shortened in the middle. Every [`hides_text`] char is
/// percent-escaped rather than dropped, so a reordering RLO is visible as
/// `%E2%80%AE`.
pub fn link_preview(uri: &str) -> String {
    let mut esc = String::with_capacity(uri.len().min(1024));
    for c in uri.chars() {
        if hides_text(c) {
            for b in c.encode_utf8(&mut [0; 4]).bytes() {
                esc.push_str(&format!("%{b:02X}"));
            }
        } else {
            esc.push(c);
        }
    }
    let scheme_like = |s: &str| s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    let Some((scheme, rest)) = esc.split_once("://").filter(|(s, _)| scheme_like(s)) else {
        return ellipsize_middle(&esc, LINK_PREVIEW_MAX_CHARS);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    // The LAST `@` ends the user part (as browsers parse it).
    let (user, host) = match authority.rsplit_once('@') {
        Some((_, host)) => ("…@", host),
        None => ("", authority),
    };
    let host_max = LINK_PREVIEW_MAX_CHARS / 2;
    let n = host.chars().count();
    let host = if n > host_max {
        format!("…{}", host.chars().skip(n - (host_max - 1)).collect::<String>())
    } else {
        host.to_string()
    };
    let head = format!("{scheme}://{user}{host}");
    let room = LINK_PREVIEW_MAX_CHARS.saturating_sub(head.chars().count());
    format!("{head}{}", ellipsize_middle(tail, room))
}

/// `s` in at most `max` chars: its head and tail around an ellipsis.
fn ellipsize_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep - keep / 2;
    let tail: Vec<char> = s.chars().skip(n - keep / 2).collect();
    s.chars().take(head).chain(std::iter::once('…')).chain(tail).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_hides_text_and_what_stays() {
        let hidden = [
            '\t', '\n', '\r', '\x1b', '\x07', '\x7f', '\u{85}', '\u{9b}', // C0, DEL, C1
            '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', // LRE RLE PDF LRO RLO
            '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', // LRI RLI FSI PDI
            '\u{200E}', '\u{200F}', '\u{061C}', // LRM RLM ALM
            '\u{200B}', '\u{2060}', '\u{2063}', '\u{FEFF}', '\u{00AD}', '\u{180E}', // invisibles
            '\u{2028}', '\u{2029}', '\u{206A}', '\u{FFF9}', '\u{1D173}', '\u{E0001}', '\u{E0041}',
        ];
        for c in hidden {
            assert!(hides_text(c), "U+{:04X} must not reach the chrome", c as u32);
        }
        let shown = [
            'a', ' ', 'é', '中', 'ß', '😀', '\u{200C}', '\u{200D}', '\u{FE0F}', '\u{1F3FD}', '…', '\u{FFFD}',
        ];
        for c in shown {
            assert!(!hides_text(c), "U+{:04X} is ordinary text", c as u32);
        }
    }

    #[test]
    fn display_drops_what_hides_text_caps_and_trims() {
        let cases: [(&str, Option<&str>); 9] = [
            ("vim", Some("vim")),
            ("a\x01b\x08c\x7fd", Some("abcd")),
            ("dir\nname", Some("dirname")),
            ("  \u{202E}gnp.exe  ", Some("gnp.exe")),
            ("\u{2066}admin\u{2069} \u{200B}", Some("admin")),
            ("👨\u{200D}👩\u{200D}👧 ❤\u{FE0F}", Some("👨\u{200D}👩\u{200D}👧 ❤\u{FE0F}")),
            ("", None),
            ("\u{202E}\u{200F}\t", None),
            ("   ", None),
        ];
        for (s, want) in cases {
            assert_eq!(display(s, TITLE_MAX_CHARS).as_deref(), want, "{s:?}");
        }
        let long = "x".repeat(1000);
        assert_eq!(display(&long, TITLE_MAX_CHARS).unwrap().chars().count(), TITLE_MAX_CHARS);
        assert_eq!(display("\u{202E}abc", 2).as_deref(), Some("ab"), "the cap counts kept chars");
    }

    #[test]
    fn a_link_preview_leads_with_the_host() {
        let cases = [
            ("https://example.com/a/b?q=1#f", "https://example.com/a/b?q=1#f"),
            ("file:///home/u/notes.txt", "file:///home/u/notes.txt"),
            ("mailto:me@example.com", "mailto:me@example.com"),
            // A user part pushed the real host past the pill's edge.
            (
                &format!("https://good.example:443-login-{}@evil.example/x", "a".repeat(300)),
                "https://…@evil.example/x",
            ),
            ("https://user:pw@host.example:8443/", "https://…@host.example:8443/"),
            // A reordering override shows as what it is.
            ("https://evil.example/\u{202E}moc.elgoog", "https://evil.example/%E2%80%AEmoc.elgoog"),
            ("https://ex\u{200B}ample.com/", "https://ex%E2%80%8Bample.com/"),
        ];
        for (uri, want) in cases {
            assert_eq!(link_preview(uri), want, "{uri:?}");
        }
    }

    #[test]
    fn a_long_link_preview_keeps_the_host_and_both_ends_of_the_path() {
        let path = format!("/start/{}/end.html", "p".repeat(500));
        let p = link_preview(&format!("https://docs.example.org{path}?v=2"));
        assert!(p.chars().count() <= LINK_PREVIEW_MAX_CHARS, "{p}");
        assert!(p.starts_with("https://docs.example.org/start/"), "{p}");
        assert!(p.ends_with("/end.html?v=2") && p.contains('…'), "{p}");
        // A host too long to show whole keeps its end: the registered domain.
        let host = format!("accounts.google.com.{}.evil.example", "x".repeat(200));
        let p = link_preview(&format!("https://{host}/login"));
        assert!(p.chars().count() <= LINK_PREVIEW_MAX_CHARS, "{p}");
        assert!(p.starts_with("https://…") && p.ends_with(".evil.example/login"), "{p}");
        // No scheme: shortened in the middle like a path.
        let p = link_preview(&"z".repeat(500));
        assert_eq!(p.chars().count(), LINK_PREVIEW_MAX_CHARS);
    }
}
