//! Pure, dependency-free fuzzy subsequence scorer for the command palette.
//!
//! Like [`crate::url`], this is a hand-rolled, unit-testable core primitive with
//! zero dependencies: [`fuzzy_match`] takes two `&str`s and returns a score plus
//! the matched character positions in the haystack (for highlight), or `None`
//! when the needle is not a subsequence of the haystack.
//!
//! It prefers the BEST-scoring alignment (a small O(m·N²) DP over the tiny
//! palette strings — titles < ~48 chars, needles < ~24), not merely the first
//! greedy subsequence: a contiguous run and a word-boundary/prefix start win, so
//! typing the start of a word ranks its command first. Case-insensitive on ASCII
//! (other scripts compared as-is, matching `url.rs`'s convention).

/// The outcome of a successful fuzzy match: a `score` (higher is better) and the
/// matched character positions `indices` into the haystack, in ascending order
/// (used to highlight the matched glyphs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub indices: Vec<usize>,
}

/// Base reward for every matched character.
const MATCH_BASE: i32 = 16;
/// Bonus when a match sits immediately after the previous match (contiguous run).
const ADJACENT: i32 = 18;
/// Bonus when a match starts a word (index 0, after a separator, or a camelCase
/// boundary) — rewards typing the start of a word.
const BOUNDARY: i32 = 14;
/// Extra bonus for a match at index 0 (stacks with BOUNDARY → exact-prefix wins).
const PREFIX: i32 = 8;
/// Penalty per skipped haystack char in a gap between two matches.
const GAP: i32 = 1;
/// Cap on the leading-skip penalty so a later (but word-boundary) start is not
/// crushed just for beginning deeper into a long haystack.
const LEADING_MAX: i32 = 3;

/// A sentinel "unreachable" DP cell; kept well away from `i32::MIN` so adding a
/// finite bonus never overflows.
const INVALID: i32 = i32::MIN / 4;

/// Characters that begin a new "word" for the boundary bonus.
fn is_sep(c: char) -> bool {
    matches!(c, ' ' | '_' | '-' | ':' | '.' | '/')
}

/// The start-of-word bonus for placing a match at haystack index `j`.
fn boundary_bonus(h: &[char], j: usize) -> i32 {
    let at_boundary = j == 0
        || is_sep(h[j - 1])
        || (h[j - 1].is_lowercase() && h[j].is_uppercase());
    let mut b = 0;
    if at_boundary {
        b += BOUNDARY;
    }
    if j == 0 {
        b += PREFIX;
    }
    b
}

/// Case-insensitive fuzzy subsequence match of `needle` against `haystack`.
///
/// Returns `None` when `needle`'s characters are not a subsequence of
/// `haystack`. An empty `needle` matches everything with score 0 and no indices
/// (the caller then keeps its own registry order). A higher score is a better
/// match; `indices` are the matched character positions in `haystack`.
pub fn fuzzy_match(needle: &str, haystack: &str) -> Option<FuzzyMatch> {
    if needle.is_empty() {
        return Some(FuzzyMatch { score: 0, indices: Vec::new() });
    }
    let h: Vec<char> = haystack.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    let m = n.len();
    let big = h.len();
    if big < m {
        return None;
    }
    // ASCII-lowercased views for comparison; non-ASCII kept verbatim.
    let hl: Vec<char> = h.iter().map(|c| c.to_ascii_lowercase()).collect();
    let nl: Vec<char> = n.iter().map(|c| c.to_ascii_lowercase()).collect();

    // dp[k][j] = best score for matching needle[0..=k] with needle[k] placed at
    // haystack[j]; parent[k][j] = the haystack index chosen for needle[k-1].
    let mut dp = vec![vec![INVALID; big]; m];
    let mut parent = vec![vec![usize::MAX; big]; m];

    // First needle char: valid at any haystack position that matches it.
    for j in 0..big {
        if hl[j] == nl[0] {
            let leading = (j as i32).min(LEADING_MAX);
            dp[0][j] = MATCH_BASE + boundary_bonus(&h, j) - leading;
        }
    }
    // Remaining needle chars.
    for k in 1..m {
        for j in k..big {
            if hl[j] != nl[k] {
                continue;
            }
            let base = MATCH_BASE + boundary_bonus(&h, j);
            let mut best = INVALID;
            let mut best_prev = usize::MAX;
            // A genuine DP scan over the previous row's reachable positions.
            #[allow(clippy::needless_range_loop)]
            for jp in (k - 1)..j {
                if dp[k - 1][jp] <= INVALID {
                    continue;
                }
                let link = if j == jp + 1 {
                    ADJACENT
                } else {
                    -GAP * (j - jp - 1) as i32
                };
                let cand = dp[k - 1][jp] + link;
                if cand > best {
                    best = cand;
                    best_prev = jp;
                }
            }
            if best > INVALID {
                dp[k][j] = base + best;
                parent[k][j] = best_prev;
            }
        }
    }
    // Best final placement of the last needle char.
    let mut best_score = INVALID;
    let mut best_j = usize::MAX;
    for (j, &s) in dp[m - 1].iter().enumerate().skip(m - 1) {
        if s > best_score {
            best_score = s;
            best_j = j;
        }
    }
    if best_j == usize::MAX || best_score <= INVALID {
        return None;
    }
    // Reconstruct the chosen positions.
    let mut indices = vec![0usize; m];
    let mut j = best_j;
    for k in (0..m).rev() {
        indices[k] = j;
        if k > 0 {
            j = parent[k][j];
        }
    }
    Some(FuzzyMatch { score: best_score, indices })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(n: &str, h: &str) -> i32 {
        fuzzy_match(n, h).expect("expected a match").score
    }

    #[test]
    fn empty_needle_matches_everything() {
        let m = fuzzy_match("", "whatever").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.indices.is_empty());
    }

    #[test]
    fn subsequence_hit_and_miss() {
        assert!(fuzzy_match("nt", "New tab").is_some());
        assert!(fuzzy_match("ntb", "New tab").is_some());
        assert!(fuzzy_match("zzz", "New tab").is_none());
        // A char not present at all → no match.
        assert!(fuzzy_match("newx", "New tab").is_none());
    }

    #[test]
    fn case_insensitive() {
        assert!(fuzzy_match("NT", "New tab").is_some());
        assert!(fuzzy_match("nt", "NEW TAB").is_some());
        assert_eq!(fuzzy_match("new", "New tab").unwrap().indices, vec![0, 1, 2]);
    }

    #[test]
    fn out_of_order_needle_is_none() {
        // "ba" is not a subsequence of "ab".
        assert!(fuzzy_match("ba", "ab").is_none());
    }

    #[test]
    fn indices_point_at_matched_chars() {
        // "New tab": n@0, e@1, w@2, ' '@3, t@4, a@5, b@6.
        let m = fuzzy_match("nt", "New tab").unwrap();
        assert_eq!(m.indices, vec![0, 4]);
    }

    #[test]
    fn prefix_beats_mid_word() {
        assert!(score("cat", "cat food") > score("cat", "a cat"));
    }

    #[test]
    fn contiguous_beats_gapped() {
        assert!(score("abc", "abcxx") > score("abc", "axbxc"));
    }

    #[test]
    fn word_boundary_beats_scattered() {
        // "gd" — g at the start, d after a space (a word boundary) vs d buried
        // inside a single word.
        assert!(score("gd", "git diff") > score("gd", "grinded"));
    }

    #[test]
    fn empty_haystack_and_multibyte_do_not_panic() {
        assert!(fuzzy_match("x", "").is_none());
        assert!(fuzzy_match("", "").unwrap().indices.is_empty());
        // Multibyte haystack + needle: no panic, indices are char (not byte) idx.
        let m = fuzzy_match("cé", "café").unwrap();
        assert_eq!(m.indices, vec![0, 3]);
    }
}
