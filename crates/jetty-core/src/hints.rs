//! Pure, terminal-free token scanner for HINT MODE (Ctrl+Shift+H).
//!
//! [`scan_line`] operates on a `&[char]` slice — one char per grid cell, wide-char
//! spacers already blanked to `' '`, exactly like [`crate::url`] — so it is
//! unit-testable without a [`crate::Terminal`] and adds zero deps (the `regex`
//! crate is deliberately NOT in the tree). It finds the non-overlapping, "useful"
//! tokens a user would want to copy or open with the keyboard: URLs (reusing the
//! `url.rs` scheme scan), IPv4 addresses, git-style hex hashes, and file paths.
//! Numbers are intentionally OFF in v1 (too noisy).
//!
//! [`assign_labels`] builds the home-row label alphabet the overlay draws over
//! each token; [`HintToken`] carries a scanned token's text/kind + its VIEWPORT
//! spans (filled in by `Terminal::hint_tokens`).

/// What kind of token the scanner matched. Priority order at any start index is
/// URL → IPv4 → Hash → Path (a URL wins a shared start, then an IP, etc.).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    Url,
    Ip,
    Hash,
    Path,
}

/// One scanned hint token: its full text, kind, and the VIEWPORT spans it covers
/// (`(row, col_start, col_end)` inclusive, visible rows only). The label anchors
/// on the first span; the copied/opened text is the complete `text` (which may
/// extend beyond the viewport for a wrapped token — see `Terminal::hint_tokens`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintToken {
    pub text: String,
    pub kind: TokenKind,
    pub spans: Vec<(usize, usize, usize)>,
}

/// Home-row-first label alphabet (fastest keys first).
const ALPHABET: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";

/// Find the non-overlapping hint tokens in one logical (unwrapped) line, left→
/// right, as half-open `(start, end, kind)` char ranges. O(N) over the line: URL
/// ranges are pre-collected scheme-to-scheme via [`crate::url::scan_urls_from`]
/// (authoritative — IP/Hash/Path never overlap a URL), then a single left→right
/// pass tries IPv4 → Hash → Path at every non-URL boundary.
pub fn scan_line(chars: &[char]) -> Vec<(usize, usize, TokenKind)> {
    let n = chars.len();
    // Pre-collect the URL ranges (each authoritative; they never overlap).
    let mut urls: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    while let Some((s, e)) = crate::url::scan_urls_from(chars, from) {
        urls.push((s, e));
        from = e.max(s + 1);
    }

    let mut out: Vec<(usize, usize, TokenKind)> = Vec::new();
    let mut ui = 0usize;
    let mut i = 0usize;
    while i < n {
        // Advance the URL pointer past any range that ends at/before i.
        while ui < urls.len() && urls[ui].1 <= i {
            ui += 1;
        }
        // At (or inside) a URL range → emit it whole and jump past it.
        if ui < urls.len() && i >= urls[ui].0 {
            let (s, e) = urls[ui];
            out.push((s, e, TokenKind::Url));
            i = e;
            ui += 1;
            continue;
        }
        // The non-URL scanners must never cross into the upcoming URL.
        let limit = if ui < urls.len() { urls[ui].0 } else { n };
        if let Some(e) = match_ipv4(chars, i, limit) {
            out.push((i, e, TokenKind::Ip));
            i = e;
            continue;
        }
        if let Some(e) = match_hash(chars, i, limit) {
            out.push((i, e, TokenKind::Hash));
            i = e;
            continue;
        }
        if let Some(e) = match_path(chars, i, limit) {
            out.push((i, e, TokenKind::Path));
            i = e;
            continue;
        }
        i += 1;
    }
    out
}

/// IPv4 `d{1,3}(\.d{1,3}){3}` starting at `i` (each octet ≤ 255), boundary-
/// delimited; returns the half-open end, or `None`.
fn match_ipv4(chars: &[char], i: usize, limit: usize) -> Option<usize> {
    // Before: not part of a larger number/word (no digit/dot/alnum to the left).
    if i > 0 {
        let p = chars[i - 1];
        if p.is_ascii_alphanumeric() || p == '.' {
            return None;
        }
    }
    let mut pos = i;
    for octet in 0..4 {
        if octet > 0 {
            if pos >= limit || chars[pos] != '.' {
                return None;
            }
            pos += 1;
        }
        let mut val: u32 = 0;
        let mut digits = 0;
        while pos < limit && digits < 3 && chars[pos].is_ascii_digit() {
            val = val * 10 + (chars[pos] as u32 - '0' as u32);
            pos += 1;
            digits += 1;
        }
        if digits == 0 || val > 255 {
            return None;
        }
    }
    // After: a following digit — or a `.digit` (a 5th octet / version) — rejects.
    if pos < limit {
        let c = chars[pos];
        if c.is_ascii_digit() {
            return None;
        }
        if c == '.' && pos + 1 < limit && chars[pos + 1].is_ascii_digit() {
            return None;
        }
    }
    Some(pos)
}

/// A git-style hex hash: 7–40 chars of `[0-9a-f]` (at least one letter, to avoid
/// labelling plain long numbers), boundary-delimited. Returns the end, or `None`.
fn match_hash(chars: &[char], i: usize, limit: usize) -> Option<usize> {
    if i > 0 && chars[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    let mut e = i;
    while e < limit {
        let c = chars[e];
        // Lowercase hex only (`is_ascii_hexdigit` would also accept A–F).
        if c.is_ascii_digit() || matches!(c, 'a'..='f') {
            e += 1;
        } else {
            break;
        }
    }
    let len = e - i;
    if !(7..=40).contains(&len) {
        return None;
    }
    if e < limit && chars[e].is_ascii_alphanumeric() {
        return None;
    }
    // Require at least one a–f so a run of digits is not mislabelled as a hash.
    if !chars[i..e].iter().any(|c| matches!(c, 'a'..='f')) {
        return None;
    }
    Some(e)
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+' | '@' | '%')
}

/// A conservative file path: a run of path-safe chars that CONTAINS a `/`
/// (absolute, relative, or `~/`), optionally with a trailing `:line[:col]`
/// (grep/compiler output). Start must be a natural boundary. Returns the end,
/// or `None`.
fn match_path(chars: &[char], i: usize, limit: usize) -> Option<usize> {
    if i > 0 {
        let p = chars[i - 1];
        let ok = p.is_whitespace()
            || matches!(p, '(' | '[' | '{' | '<' | '"' | '\'' | '=' | ',' | ':');
        if !ok {
            return None;
        }
    }
    if i >= limit || !is_path_char(chars[i]) {
        return None;
    }
    let mut e = i;
    while e < limit && is_path_char(chars[e]) {
        e += 1;
    }
    // Absorb up to two trailing `:digits` groups (line[:col]).
    let mut k = e;
    for _ in 0..2 {
        if k < limit && chars[k] == ':' && k + 1 < limit && chars[k + 1].is_ascii_digit() {
            k += 1;
            while k < limit && chars[k].is_ascii_digit() {
                k += 1;
            }
            e = k;
        } else {
            break;
        }
    }
    // Trim trailing punctuation that is never part of a path.
    while e > i && matches!(chars[e - 1], '.' | ',' | ')' | ']' | '}' | '>' | ':' | '\'' | '"') {
        e -= 1;
    }
    if e <= i + 1 {
        return None;
    }
    if !chars[i..e].contains(&'/') {
        return None;
    }
    Some(e)
}

/// Assign short labels to `n` tokens. `n ≤ 26` → single home-row chars in scan
/// order; `n > 26` → fixed-width base-26 strings (uniform width so no label is a
/// prefix of another — the partial-narrowing is unambiguous). Collision-free by
/// construction.
pub fn assign_labels(n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let base = ALPHABET.len();
    if n <= base {
        return (0..n).map(|i| (ALPHABET[i] as char).to_string()).collect();
    }
    // Uniform width = smallest w with base^w >= n.
    let mut width = 1usize;
    let mut cap = base;
    while cap < n {
        width += 1;
        cap = cap.saturating_mul(base);
    }
    (0..n)
        .map(|idx| {
            let mut buf = vec![b'a'; width];
            let mut v = idx;
            for pos in (0..width).rev() {
                buf[pos] = ALPHABET[v % base];
                v /= base;
            }
            buf.iter().map(|&b| b as char).collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }
    /// Render a scan back to (text, kind) pairs.
    fn scan(s: &str) -> Vec<(String, TokenKind)> {
        let c = cs(s);
        scan_line(&c)
            .into_iter()
            .map(|(a, b, k)| (c[a..b].iter().collect(), k))
            .collect()
    }

    #[test]
    fn finds_a_url() {
        assert_eq!(
            scan("see https://example.com/page today"),
            vec![("https://example.com/page".to_string(), TokenKind::Url)]
        );
    }

    #[test]
    fn finds_ipv4_with_octet_bound() {
        assert_eq!(scan("ping 192.168.1.1 now"), vec![("192.168.1.1".to_string(), TokenKind::Ip)]);
        // 999 > 255 → not an IP (and 999.1.1.1 has no other token).
        assert!(scan("999.1.1.1").is_empty());
        // A 5-part dotted number is not an IPv4.
        assert!(scan("1.2.3.4.5").iter().all(|(_, k)| *k != TokenKind::Ip));
        // Trailing sentence dot is not consumed / does not break the match.
        assert_eq!(scan("host 10.0.0.1."), vec![("10.0.0.1".to_string(), TokenKind::Ip)]);
    }

    #[test]
    fn finds_git_hash_within_length_bounds() {
        assert_eq!(scan("commit e4a599f done"), vec![("e4a599f".to_string(), TokenKind::Hash)]);
        // 6 chars is too short.
        assert!(scan("abc123").iter().all(|(_, k)| *k != TokenKind::Hash));
        // A pure 7+ digit number is NOT a hash (needs a-f).
        assert!(scan("1234567").iter().all(|(_, k)| *k != TokenKind::Hash));
        // Uppercase hex is not a git hash.
        assert!(scan("ABC1234").iter().all(|(_, k)| *k != TokenKind::Hash));
        // A 40-char sha.
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(scan(sha), vec![(sha.to_string(), TokenKind::Hash)]);
    }

    #[test]
    fn finds_paths_requiring_a_slash() {
        assert_eq!(scan("edit /usr/bin/foo now"), vec![("/usr/bin/foo".to_string(), TokenKind::Path)]);
        assert_eq!(scan("~/.config/jetty/x"), vec![("~/.config/jetty/x".to_string(), TokenKind::Path)]);
        assert_eq!(scan("at ./src/app.rs:8568"), vec![("./src/app.rs:8568".to_string(), TokenKind::Path)]);
        // No slash → not a path (avoids labelling every dotted word).
        assert!(scan("README.md is here").iter().all(|(_, k)| *k != TokenKind::Path));
        // Trailing punctuation trimmed.
        assert_eq!(scan("(see /etc/hosts)"), vec![("/etc/hosts".to_string(), TokenKind::Path)]);
    }

    #[test]
    fn priority_and_non_overlap_on_a_dense_line() {
        // A URL, an IP, a hash and a path on one line: four non-overlapping tokens
        // in reading order.
        let got = scan("go https://a.io/x host 8.8.8.8 sha deadbeef1 file /tmp/a/b");
        assert_eq!(
            got,
            vec![
                ("https://a.io/x".to_string(), TokenKind::Url),
                ("8.8.8.8".to_string(), TokenKind::Ip),
                ("deadbeef1".to_string(), TokenKind::Hash),
                ("/tmp/a/b".to_string(), TokenKind::Path),
            ]
        );
    }

    #[test]
    fn two_urls_on_one_line() {
        let got = scan("https://a.io/x and https://b.io/y");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "https://a.io/x");
        assert_eq!(got[1].0, "https://b.io/y");
    }

    #[test]
    fn empty_and_no_token_lines() {
        assert!(scan("").is_empty());
        assert!(scan("   just plain words, nothing here   ").is_empty());
    }

    #[test]
    fn wrapped_token_across_a_logical_line() {
        // scan_line sees the ASSEMBLED logical line (Terminal::hint_tokens
        // concatenates wrapped rows). A URL that would visually wrap is one token.
        let s = format!("go {}", "https://example.com/".to_owned() + &"a".repeat(60));
        let got = scan(&s);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, TokenKind::Url);
        assert!(got[0].0.len() > 60);
    }

    #[test]
    fn labels_single_char_up_to_26() {
        assert_eq!(assign_labels(0), Vec::<String>::new());
        assert_eq!(assign_labels(1), vec!["a"]);
        let l = assign_labels(26);
        assert_eq!(l.len(), 26);
        assert!(l.iter().all(|s| s.chars().count() == 1));
        // Home-row first.
        assert_eq!(&l[0..4], &["a", "s", "d", "f"]);
    }

    #[test]
    fn labels_uniform_width_and_collision_free_above_26() {
        for n in [27usize, 100, 700] {
            let l = assign_labels(n);
            assert_eq!(l.len(), n);
            let w = l[0].chars().count();
            // Uniform width (so no label is a prefix of another).
            assert!(l.iter().all(|s| s.chars().count() == w), "n={n} not uniform width");
            // Collision-free.
            let set: std::collections::HashSet<&String> = l.iter().collect();
            assert_eq!(set.len(), n, "n={n} has duplicate labels");
        }
        // 27 needs width 2; 700 needs width 2 (26*26=676 < 700 → width 3).
        assert_eq!(assign_labels(27)[0].chars().count(), 2);
        assert_eq!(assign_labels(700)[0].chars().count(), 3);
    }
}
