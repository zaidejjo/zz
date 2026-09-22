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
        return len_diff;
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
///
/// Ranking heuristics (applied after raw distance):
/// 1. **Prefix bonus**: candidates sharing a common prefix with `name` get a
///    half-distance bonus (rounded down). This ensures "printn" → "println"
///    (prefix "print") beats "print" (also distance 1 but shorter).
/// 2. **Longer-is-better**: among candidates with equal effective distance,
///    prefer the longer name (more specific match).
pub fn suggest<'a>(name: &str, candidates: &[&'a str]) -> Option<(&'a str, u32)> {
    let threshold = max_distance(name);
    let mut best: Option<(&str, u32, u32)> = None; // (name, raw_dist, effective_dist)

    for &cand in candidates {
        if cand == name {
            return None; // Exact match — no suggestion needed.
        }
        let dist = levenshtein(name, cand);
        if dist > threshold {
            continue;
        }

        // Compute a shared-prefix length for ranking.
        let prefix_len = name
            .chars()
            .zip(cand.chars())
            .take_while(|(a, b)| a == b)
            .count() as u32;

        // Effective distance: subtract floor(prefix_len / 2) as a bonus.
        // This means a candidate sharing 2+ prefix chars beats one sharing 0.
        let prefix_bonus = prefix_len / 2;
        let effective = dist.saturating_sub(prefix_bonus);

        match &best {
            None => best = Some((cand, dist, effective)),
            Some((_, _, best_eff)) if effective < *best_eff => best = Some((cand, dist, effective)),
            Some((_, _, best_eff)) if effective == *best_eff => {
                let (_, _, _) = best.unwrap();
                // On effective-distance tie, prefer longer candidate (more specific).
                if cand.len() > best.unwrap().0.len() {
                    best = Some((cand, dist, effective));
                }
            }
            _ => {}
        }
    }

    best.map(|(name, dist, _)| (name, dist))
}

/// Find all candidates within the Levenshtein threshold, sorted by effective
/// distance then length. Used for ambiguous-fix detection.
///
/// Returns `Vec<(candidate, distance)>` — empty if nothing is close enough.
pub fn suggest_all<'a>(name: &str, candidates: &[&'a str]) -> Vec<(&'a str, u32)> {
    let threshold = max_distance(name);
    let mut results: Vec<(&str, u32, u32)> = Vec::new();

    for &cand in candidates {
        if cand == name {
            continue;
        }
        let dist = levenshtein(name, cand);
        if dist > threshold {
            continue;
        }
        let prefix_len = name
            .chars()
            .zip(cand.chars())
            .take_while(|(a, b)| a == b)
            .count() as u32;
        let prefix_bonus = prefix_len / 2;
        let effective = dist.saturating_sub(prefix_bonus);
        results.push((cand, dist, effective));
    }

    // Sort by effective distance, then by length descending.
    results.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| b.0.len().cmp(&a.0.len())));
    results
        .into_iter()
        .map(|(cand, dist, _)| (cand, dist))
        .collect()
}

/// Suggest a correction for a dotted path (`env.tmp_dir` → `env.temp_dir`).
///
/// Plain Levenshtein over the whole string misses these: the shared
/// `env.` prefix plus a 1-char tail typo already exceeds the threshold.
/// Instead, match segment-wise — a candidate qualifies when its head
/// segments equal the name's (or are themselves within threshold) and its
/// final segment is within threshold of the name's final segment.
/// Returns the full corrected path.
pub fn suggest_dotted<'a>(name: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let mut name_segs: Vec<&str> = name.split('.').collect();
    if name_segs.len() < 2 {
        return None;
    }
    let name_tail = name_segs.pop().unwrap();
    let mut best: Option<(&'a str, u32)> = None;
    for &cand in candidates {
        if cand == name {
            continue;
        }
        let mut cand_segs: Vec<&str> = cand.split('.').collect();
        if cand_segs.len() != name_segs.len() + 1 {
            continue;
        }
        let cand_tail = cand_segs.pop().unwrap();
        // Heads must match closely (exact, or a small typo like `evn`).
        let mut head_dist = 0u32;
        let mut heads_ok = true;
        for (a, b) in name_segs.iter().zip(cand_segs.iter()) {
            if a != b {
                let d = levenshtein(a, b);
                if d > max_distance(a) {
                    heads_ok = false;
                    break;
                }
                head_dist += d;
            }
        }
        if !heads_ok {
            continue;
        }
        let tail_dist = levenshtein(name_tail, cand_tail);
        if tail_dist > max_distance(name_tail) || (head_dist == 0 && tail_dist == 0) {
            continue;
        }
        let total = head_dist + tail_dist;
        if best.map(|(_, d)| total < d).unwrap_or(true) {
            best = Some((cand, total));
        }
    }
    best.map(|(cand, _)| cand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_tail_typo() {
        let cands = vec![
            "std.env.temp_dir",
            "env.temp_dir",
            "std.env.cwd",
            "temp_dir",
        ];
        assert_eq!(suggest_dotted("env.tmp_dir", &cands), Some("env.temp_dir"));
        assert_eq!(
            suggest_dotted("std.env.tmp_dir", &cands),
            Some("std.env.temp_dir")
        );
    }

    #[test]
    fn dotted_rejects_wrong_head_and_far_tails() {
        let cands = vec!["std.env.temp_dir", "env.temp_dir"];
        assert_eq!(suggest_dotted("fs.tmp_dir", &cands), None);
        assert_eq!(suggest_dotted("env.zzzzzz", &cands), None);
        assert_eq!(suggest_dotted("tmp_dir", &cands), None);
        assert_eq!(suggest_dotted("env.temp_dir", &cands), None);
    }

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
        assert_eq!(suggestion, "abcde");
    }

    #[test]
    fn suggest_prefix_beats_shorter() {
        // "printn" → "println" (prefix "print", dist 1) should beat
        // "print" (dist 1, no prefix bonus beyond what "println" gets).
        let candidates = vec!["print", "println", "format", "panic"];
        let (suggestion, _) = suggest("printn", &candidates).unwrap();
        assert_eq!(suggestion, "println");
    }

    #[test]
    fn suggest_prefix_beats_nonprefix() {
        // "prntln" should suggest "println" (shares prefix "prn"), not "print".
        let candidates = vec!["print", "println", "format", "panic"];
        let (suggestion, _) = suggest("prntln", &candidates).unwrap();
        assert_eq!(suggestion, "println");
    }

    #[test]
    fn suggest_all_returns_multiple_candidates() {
        // "heigth" → both "heighth" (dist=1) and "height" (dist=2) qualify.
        let candidates = vec!["height", "heighth", "weight", "width"];
        let all = suggest_all("heigth", &candidates);
        assert!(
            all.len() >= 2,
            "expected at least 2 candidates, got {}",
            all.len()
        );
        // Both present.
        let names: Vec<&str> = all.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"height"));
        assert!(names.contains(&"heighth"));
    }

    #[test]
    fn suggest_all_empty_when_no_match() {
        let candidates = vec!["foo", "bar"];
        let all = suggest_all("xyz", &candidates);
        assert!(all.is_empty());
    }
}
