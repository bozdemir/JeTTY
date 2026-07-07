//! Plain-text URL detection over an unwrapped (logical) grid line.
//!
//! Pure and dependency-free: [`find_url_at`] operates on a `&[char]` slice
//! (one char per grid cell, wide-char spacers already blanked to `' '`) so it
//! is unit-testable without a terminal and adds zero deps (the `regex` crate
//! is deliberately NOT in the tree).

/// Cap on wrapped-row walking (each direction) when assembling the logical
/// line around the hovered cell. Bounds pathological single-logical-line
/// output (e.g. `cat minified.js`); do not raise casually — the walk runs on
/// hovered-cell change with the link modifier held (speed-first rule).
pub(crate) const MAX_WRAP_WALK: usize = 64;

/// Allowed URL schemes, matched case-insensitively. Deliberately restricted
/// to what the click-to-open path may spawn (`xdg-open`/`open`).
const SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// RFC-3986 URI charset (unreserved + gen/sub-delims + `%` escapes). Space is
/// NOT included: grid rows are space-padded, and a URL must never expand
/// through the padding into neighboring text.
fn in_charset(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

/// If one of [`SCHEMES`] starts (case-insensitively) at `chars[p..]`, return
/// its length in chars.
fn scheme_len_at(chars: &[char], p: usize) -> Option<usize> {
    SCHEMES.iter().find_map(|s| {
        (chars.len() >= p + s.len()
            && chars[p..p + s.len()]
                .iter()
                .zip(s.chars())
                .all(|(&a, b)| a.to_ascii_lowercase() == b))
        .then_some(s.len())
    })
}

/// Return the half-open char/cell range `(start, end)` of a URL covering
/// index `idx`, or `None` when the hovered char is not part of one.
///
/// Algorithm: expand the maximal charset window around `idx`; locate the LAST
/// scheme start at or before `idx` inside it; require at least one charset
/// char after `://`; trim trailing punctuation (`. , ; : ! ? '`) and
/// unbalanced closers (`)` / `]`); finally require `start <= idx < end`.
pub fn find_url_at(chars: &[char], idx: usize) -> Option<(usize, usize)> {
    if idx >= chars.len() || !in_charset(chars[idx]) {
        return None;
    }
    // (1) Maximal charset window around idx.
    let mut l = idx;
    while l > 0 && in_charset(chars[l - 1]) {
        l -= 1;
    }
    let mut r = idx + 1;
    while r < chars.len() && in_charset(chars[r]) {
        r += 1;
    }
    // (2) Latest scheme start at or before idx (handles `?u=https://…` tails:
    // hovering the embedded URL matches it, hovering earlier matches the outer).
    let (start, scheme_len) = (l..=idx)
        .rev()
        .find_map(|p| scheme_len_at(chars, p).map(|n| (p, n)))?;
    // (3) At least one charset char after "://" (a bare scheme is not a URL).
    let mut end = r;
    if end <= start + scheme_len {
        return None;
    }
    // (4) Trim trailing punctuation. Closers strip only while unbalanced
    // within the candidate, so `/wiki/Foo_(bar)` keeps its `)` but the `)` of
    // `(see https://x.io/a)` is dropped. Bracket counts are computed ONCE and
    // maintained incrementally as chars leave the window, keeping the whole
    // trim O(N): recounting per stripped closer was O(N²) and a logical line
    // ending in tens of thousands of `)` chars stalled the event loop
    // (speed-first rule — this runs synchronously on hover/click).
    let mut paren_bal = 0isize; // ')' count minus '(' count in [start, end)
    let mut brack_bal = 0isize; // ']' count minus '[' count in [start, end)
    for &x in &chars[start..end] {
        match x {
            '(' => paren_bal -= 1,
            ')' => paren_bal += 1,
            '[' => brack_bal -= 1,
            ']' => brack_bal += 1,
            _ => {}
        }
    }
    loop {
        let c = chars[end - 1];
        let strip = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' => true,
            ')' => paren_bal > 0, // more ')' than '(' → unbalanced closer
            ']' => brack_bal > 0,
            _ => false,
        };
        if !strip {
            break;
        }
        // The stripped char leaves the window; keep the balances in sync.
        match c {
            ')' => paren_bal -= 1,
            ']' => brack_bal -= 1,
            _ => {}
        }
        end -= 1;
        if end <= start + scheme_len {
            return None;
        }
    }
    // (5) The hovered cell must still lie inside the trimmed URL.
    (idx >= start && idx < end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// Convenience: run the matcher and render the hit back to a string.
    fn url_at(s: &str, idx: usize) -> Option<String> {
        let cs = chars(s);
        find_url_at(&cs, idx).map(|(a, b)| cs[a..b].iter().collect())
    }

    #[test]
    fn finds_url_at_every_index_within_it() {
        let s = "see https://example.com/page today";
        let url = "https://example.com/page";
        let start = 4;
        for i in start..start + url.len() {
            assert_eq!(url_at(s, i).as_deref(), Some(url), "hover idx {i}");
        }
    }

    #[test]
    fn none_on_surrounding_text() {
        let s = "see https://example.com/page today";
        for i in [0, 1, 2, 3, 29, 30, 33] {
            assert_eq!(url_at(s, i), None, "hover idx {i} should miss");
        }
        // Out-of-range index never panics.
        assert_eq!(url_at(s, 1000), None);
    }

    #[test]
    fn trailing_punctuation_is_trimmed() {
        for p in ['.', ',', ';', ':', '!', '?', '\''] {
            let s = format!("go to https://x.io/a{p} now");
            assert_eq!(url_at(&s, 8).as_deref(), Some("https://x.io/a"), "sep {p:?}");
            // Hovering the trimmed separator itself is a miss.
            assert_eq!(url_at(&s, 20), None, "sep {p:?} hover");
        }
        // A run of mixed trailing punctuation all trims away.
        assert_eq!(url_at("https://x.io/a).,", 3).as_deref(), Some("https://x.io/a"));
    }

    #[test]
    fn balanced_parens_kept_unbalanced_stripped() {
        let wiki = "https://en.wikipedia.org/wiki/Foo_(bar)";
        assert_eq!(url_at(wiki, 10).as_deref(), Some(wiki), "balanced ) kept");
        assert_eq!(
            url_at("(see https://x.io/a)", 8).as_deref(),
            Some("https://x.io/a"),
            "unbalanced ) stripped"
        );
    }

    #[test]
    fn brackets_balanced_kept_unbalanced_stripped() {
        let v6 = "http://[::1]:8080/x";
        assert_eq!(url_at(v6, 3).as_deref(), Some(v6), "balanced ] kept");
        assert_eq!(
            url_at("[link](https://x.io/a] end", 10).as_deref(),
            Some("https://x.io/a"),
            "unbalanced ] stripped"
        );
    }

    #[test]
    fn pathological_trailing_closers_trim_in_linear_time() {
        // A URL followed by tens of thousands of unbalanced ')' (all in the
        // URI charset, so the candidate window absorbs them). The quadratic
        // recount-per-stripped-closer version does ~4e8 char compares here;
        // the incremental-balance version is a single pass (F5 regression).
        let s = format!("https://x.io/a{}", ")".repeat(20_000));
        assert_eq!(url_at(&s, 3).as_deref(), Some("https://x.io/a"));
        // Same for ']' (the other closer kind).
        let s = format!("https://x.io/a{}", "]".repeat(20_000));
        assert_eq!(url_at(&s, 3).as_deref(), Some("https://x.io/a"));
    }

    #[test]
    fn incremental_balance_matches_recount_semantics() {
        // Balanced pair inside, one unbalanced closer outside: only the
        // trailing unbalanced ')' strips.
        assert_eq!(
            url_at("https://x.io/(a))", 3).as_deref(),
            Some("https://x.io/(a)"),
            "balanced pair kept, trailing unbalanced ')' stripped"
        );
        // Mixed trailing closers with interleaved punctuation all trim, and
        // each kind's balance is tracked independently.
        assert_eq!(
            url_at("https://x.io/a).],)", 3).as_deref(),
            Some("https://x.io/a"),
            "mixed trailing closers + punctuation fully trimmed"
        );
        // Nested balanced brackets survive a run of unbalanced tails.
        assert_eq!(
            url_at("https://x.io/[a(b)]))]]", 3).as_deref(),
            Some("https://x.io/[a(b)]"),
            "nested balanced brackets kept"
        );
    }

    #[test]
    fn allowed_schemes_only() {
        assert_eq!(url_at("file:///tmp/report.html", 2).as_deref(), Some("file:///tmp/report.html"));
        assert_eq!(url_at("http://x.io", 2).as_deref(), Some("http://x.io"));
        assert_eq!(url_at("ftp://x.io/a", 2), None, "ftp rejected");
        assert_eq!(url_at("mailto:me@x.io", 2), None, "mailto rejected");
    }

    #[test]
    fn bare_scheme_rejected() {
        assert_eq!(url_at("http://", 2), None);
        assert_eq!(url_at("see https:// end", 6), None);
        // A scheme whose rest fully trims away is also rejected.
        assert_eq!(url_at("https://...", 2), None);
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert_eq!(url_at("HTTPS://X.IO", 4).as_deref(), Some("HTTPS://X.IO"));
        assert_eq!(url_at("HtTp://x.io/A", 0).as_deref(), Some("HtTp://x.io/A"));
    }

    #[test]
    fn url_at_line_start_and_end() {
        let s = "https://x.io/a";
        // First and last char of a line-spanning URL both hit.
        assert_eq!(url_at(s, 0).as_deref(), Some(s));
        assert_eq!(url_at(s, s.len() - 1).as_deref(), Some(s));
        // URL flush at the line end after leading text.
        let t = "open https://x.io/a";
        assert_eq!(url_at(t, t.len() - 1).as_deref(), Some("https://x.io/a"));
    }

    #[test]
    fn embedded_url_prefers_the_inner_scheme() {
        let s = "https://a.io/r?u=https://b.io/x";
        // Hovering the embedded tail matches the LAST scheme at or before idx.
        assert_eq!(url_at(s, 20).as_deref(), Some("https://b.io/x"));
        // Hovering the outer part matches the whole composite URL.
        assert_eq!(url_at(s, 2).as_deref(), Some(s));
    }
}
