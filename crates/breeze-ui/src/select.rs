//! Text selection over the grid: extract the selected text from visible rows
//! given start/end cell coordinates (row, col). Linear (stream) selection, like
//! a normal terminal. Pure — the window tracks the drag and copies the result.

fn slice_from(line: &str, from: usize) -> String {
    line.chars().skip(from).collect()
}

fn slice_to_incl(line: &str, to: usize) -> String {
    line.chars().take(to + 1).collect()
}

fn slice_incl(line: &str, from: usize, to: usize) -> String {
    line.chars().skip(from).take(to.saturating_sub(from) + 1).collect()
}

/// Selected text between `start` and `end` (each `(row, col)`), inclusive of the
/// end cell. Order-independent. `lines` are the visible rows.
pub fn selection_text(lines: &[String], start: (usize, usize), end: (usize, usize)) -> String {
    let (a, b) = if start <= end { (start, end) } else { (end, start) };
    let (sr, sc) = a;
    let (er, ec) = b;

    if sr == er {
        let line = lines.get(sr).map(|s| s.as_str()).unwrap_or("");
        return slice_incl(line, sc, ec);
    }

    let mut out = String::new();
    for r in sr..=er {
        let line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
        if r == sr {
            out.push_str(&slice_from(line, sc));
        } else if r == er {
            out.push_str(&slice_to_incl(line, ec));
        } else {
            out.push_str(line);
        }
        if r != er {
            out.push('\n');
        }
    }
    out
}

/// Inclusive `(row, col_start, col_end)` spans the selection highlight covers,
/// given the grid width `cols`. Mirrors `selection_text`'s stream semantics
/// (first row runs to the edge, middle rows are full, last row from 0).
/// Order-independent.
pub fn selection_spans(
    start: (usize, usize),
    end: (usize, usize),
    cols: usize,
) -> Vec<(usize, usize, usize)> {
    if cols == 0 {
        return Vec::new();
    }
    let (a, b) = if start <= end { (start, end) } else { (end, start) };
    let (sr, sc) = a;
    let (er, ec) = b;
    let last = cols - 1;
    if sr == er {
        return vec![(sr, sc.min(last), ec.min(last))];
    }
    (sr..=er)
        .map(|r| {
            if r == sr {
                (r, sc.min(last), last)
            } else if r == er {
                (r, 0, ec.min(last))
            } else {
                (r, 0, last)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_single_row() {
        assert_eq!(selection_spans((0, 2), (0, 5), 80), vec![(0, 2, 5)]);
    }

    #[test]
    fn spans_multi_row_first_to_edge_middle_full_last_from_zero() {
        assert_eq!(selection_spans((0, 3), (2, 4), 10), vec![(0, 3, 9), (1, 0, 9), (2, 0, 4)]);
    }

    #[test]
    fn spans_reversed_is_normalized() {
        assert_eq!(selection_spans((2, 4), (0, 3), 10), selection_spans((0, 3), (2, 4), 10));
    }

    #[test]
    fn single_row_inclusive() {
        let lines = vec!["hello world".to_string()];
        assert_eq!(selection_text(&lines, (0, 0), (0, 4)), "hello");
        assert_eq!(selection_text(&lines, (0, 6), (0, 10)), "world");
    }

    #[test]
    fn multi_row_spans_full_middle_rows() {
        let lines = vec!["abc".to_string(), "def".to_string(), "ghi".to_string()];
        assert_eq!(selection_text(&lines, (0, 1), (2, 1)), "bc\ndef\ngh");
    }

    #[test]
    fn reversed_order_is_normalized() {
        let lines = vec!["abc".to_string(), "def".to_string()];
        assert_eq!(selection_text(&lines, (1, 1), (0, 1)), selection_text(&lines, (0, 1), (1, 1)));
    }

    #[test]
    fn out_of_range_rows_are_empty() {
        let lines = vec!["abc".to_string()];
        assert_eq!(selection_text(&lines, (5, 0), (6, 2)), "\n");
    }
}
