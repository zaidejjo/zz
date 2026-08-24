//! Optimized Levenshtein distance with candidate matching.
//!
//! Used for typo suggestions in diagnostics (e.g. "undefined variable" →
//! "did you mean `println`?"). The distance is capped at a small threshold
//! (2–3 depending on name length) for O(n·m) performance on short strings.

/// Maximum edit distance allowed for a suggestion. Scales with name length
/// to avoid noise on very short identifiers.
pub fn max_distance(name: &str) -> u32 {
    max_distance_by_len(name.len())
}

/// Maximum distance based on a numeric length (avoids borrowing the string).
fn max_distance_by_len(len: usize) -> u32 {
    match len {
        0..=3 => 1,
        4..=7 => 2,
        _ => 3,
    }
}

/// Compute the Levenshtein edit distance between two strings.
///
/// Uses a single-row DP approach (O(min(a,b)) space).
pub fn levenshtein(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let alen = a.len();
    let blen = b.len();

    if alen == 0 {
        return blen as u32;
    }
    if blen == 0 {
        return alen as u32;
    }

    // Early exit if length difference already exceeds threshold.
    let len_diff = (alen as i32 - blen as i32).unsigned_abs();
    let shorter_len = alen.min(blen);
    let threshold = max_distance_by_len(shorter_len);
    if len_diff > threshold {
        return len_diff as u32;
    }

    // Single-row DP.
    let mut prev: Vec<u32> = (0..=blen as u32).collect();
    let mut curr: Vec<u32> = vec![0; blen + 1];

    for i in 1..=alen {
        curr[0] = i as u32;
        for j in 1..=blen {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j - 1] + cost).min(prev[j] + 1).min(curr[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[blen]
}

/// Find the best suggestion from a list of candidates.
///
/// Returns `(candidate, distance)` for the closest match within the allowed
/// threshold, or `None` if nothing is close enough.
pub fn suggest<'a>(name: &str, candidates: &[&'a str]) -> Option<(&'a str, u32)> {
    let threshold = max_distance(name);
    let mut best: Option<(&str, u32)> = None;

    for &cand in candidates {
        if cand == name {
            return None; // Exact match — no suggestion needed.
        }
        let dist = levenshtein(name, cand);
        if dist <= threshold {
            match &best {
                None => best = Some((cand, dist)),
                Some((_, best_dist)) if dist < *best_dist => best = Some((cand, dist)),
                Some((_, best_dist))
                    if dist == *best_dist && cand.len() < best.unwrap().0.len() =>
                {
                    best = Some((cand, dist))
                }
                _ => {}
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_zero() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn single_insertion() {
        assert_eq!(levenshtein("hell", "hello"), 1);
    }

    #[test]
    fn single_deletion() {
        assert_eq!(levenshtein("hello", "hell"), 1);
    }

    #[test]
    fn single_substitution() {
        assert_eq!(levenshtein("hello", "hallo"), 1);
    }

    #[test]
    fn two_edits() {
        assert_eq!(levenshtein("hello", "halo"), 2);
    }

    #[test]
    fn empty_string() {
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn both_empty() {
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn suggest_finds_close_match() {
        let candidates = vec!["println", "print", "format", "panic"];
        let (suggestion, dist) = suggest("prntln", &candidates).unwrap();
        assert_eq!(suggestion, "println");
        assert!(dist <= 2, "distance {dist} should be <= 2");
    }

    #[test]
    fn suggest_no_match_too_far() {
        let candidates = vec!["alpha", "bravo"];
        assert!(suggest("zzz", &candidates).is_none());
    }

    #[test]
    fn suggest_exact_match_returns_none() {
        let candidates = vec!["hello"];
        assert!(suggest("hello", &candidates).is_none());
    }

    #[test]
    fn suggest_prefers_shorter_on_tie() {
        let candidates = vec!["abcd", "abcde"];
        let (suggestion, _) = suggest("abcdf", &candidates).unwrap();
        assert_eq!(suggestion, "abcd");
    }
}
