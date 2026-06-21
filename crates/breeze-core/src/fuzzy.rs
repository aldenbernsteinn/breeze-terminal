//! Fuzzy matching for the command/settings palette: subsequence scoring that
//! rewards contiguous runs and an early first match.

/// Score `text` against query chars `q` (lower = better). Returns `None` unless
/// every char of `q` appears in `text` in order. Callers lowercase both sides
/// for case-insensitive matching.
pub fn fuzzy_score(q: &[char], text: &str) -> Option<i32> {
    let t: Vec<char> = text.chars().collect();
    let mut qi = 0usize;
    let mut ti = 0usize;
    let mut score = 0i32;
    let mut last_match = -2i32;
    let mut first_match = -1i32;
    while qi < q.len() && ti < t.len() {
        if q[qi] == t[ti] {
            if first_match < 0 {
                first_match = ti as i32;
            }
            if ti as i32 != last_match + 1 {
                score += 3; // gap penalty
            }
            last_match = ti as i32;
            qi += 1;
        }
        ti += 1;
    }
    if qi != q.len() {
        return None;
    }
    Some(score + first_match)
}

/// Filter and rank `items` by `query` (case-insensitive). Items whose text
/// contains every query char in order are kept, ordered by score ascending;
/// ties keep their original order. An empty query keeps everything in order.
pub fn fuzzy_filter<'a>(query: &str, items: &[&'a str]) -> Vec<&'a str> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let mut scored: Vec<(usize, i32, &'a str)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, &item)| fuzzy_score(&q, &item.to_lowercase()).map(|s| (i, s, item)))
        .collect();
    scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert_eq!(fuzzy_score(&chars("xyz"), "abc"), None);
        assert_eq!(fuzzy_score(&chars("abcd"), "abc"), None); // q longer than available
    }

    #[test]
    fn empty_query_matches_with_low_score() {
        // qi == 0 == q.len() immediately → score 0 + first_match (-1) = -1.
        assert_eq!(fuzzy_score(&[], "anything"), Some(-1));
    }

    #[test]
    fn contiguous_beats_gappy() {
        let contig = fuzzy_score(&chars("abc"), "abc").unwrap();
        let gappy = fuzzy_score(&chars("abc"), "axbxc").unwrap();
        assert!(contig < gappy, "contig {contig} should beat gappy {gappy}");
    }

    #[test]
    fn earlier_first_match_beats_later() {
        let early = fuzzy_score(&chars("a"), "a__").unwrap();
        let late = fuzzy_score(&chars("a"), "__a").unwrap();
        assert!(early < late, "early {early} should beat late {late}");
    }

    #[test]
    fn filter_ranks_and_drops_non_matches() {
        let items = ["New Tab", "New Window", "Close Pane", "Settings"];
        // "nw" → "New Window" (contiguous-ish) ranks above "New Tab"? both match n..w.
        let got = fuzzy_filter("nw", &items);
        assert!(got.contains(&"New Window"));
        assert!(!got.contains(&"Settings")); // no subsequence n,w
    }

    #[test]
    fn filter_is_case_insensitive_and_empty_keeps_order() {
        let items = ["Alpha", "beta", "Gamma"];
        assert_eq!(fuzzy_filter("", &items), vec!["Alpha", "beta", "Gamma"]);
        assert_eq!(fuzzy_filter("BETA", &items), vec!["beta"]);
    }
}
