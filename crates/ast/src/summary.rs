//! Structural outline for large files: keep signatures and structure, elide
//! long bodies, so a whole-file read fits without losing the shape of the code.

use crate::{Lang, doc_for};
use ast_grep_core::Node;

/// One rendered span of the outline, in ascending line order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub kept: bool,
    /// 1-based inclusive first line.
    pub start_line: usize,
    /// 1-based inclusive last line.
    pub end_line: usize,
}

/// Builds an outline of `source`: top-level item headers and closing lines are
/// kept, and any body longer than `min_body_lines` is elided.
///
/// Returns `None` when the language cannot be parsed or nothing would be
/// elided, because a summary that is identical to the file is not a summary.
pub fn summarize(source: &str, lang: Lang, min_body_lines: usize) -> Option<Vec<Segment>> {
    let doc = doc_for(source, lang).ok()?;
    let total = source.lines().count();
    if total == 0 {
        return None;
    }
    let mut kept: Vec<(usize, usize)> = vec![(1, total)];

    // Turn each long body into an elided hole. Restricting the walk to the
    // root's named children keeps the outline at item granularity instead of
    // drilling into nested statements.
    let mut holes: Vec<(usize, usize)> = Vec::new();
    collect_holes(doc.root(), total, min_body_lines, &mut holes);
    // Keep only the outermost holes; a nested hole inside an already elided
    // body adds nothing.
    holes.sort();
    let mut outer: Vec<(usize, usize)> = Vec::new();
    for hole in holes {
        if outer.iter().any(|(start, end)| hole.0 >= *start && hole.1 <= *end) {
            continue;
        }
        outer.push(hole);
    }
    if outer.is_empty() {
        return None;
    }

    for (start, end) in &outer {
        if start > end {
            continue;
        }
        let mut next: Vec<(usize, usize)> = Vec::with_capacity(kept.len() + 1);
        for (keep_start, keep_end) in kept {
            if keep_end < *start || keep_start > *end {
                next.push((keep_start, keep_end));
                continue;
            }
            if keep_start < *start {
                next.push((keep_start, start - 1));
            }
            if keep_end > *end {
                next.push((end + 1, keep_end));
            }
        }
        kept = next;
    }

    let mut segments = segments_from(kept, outer);
    segments.retain(|segment| segment.start_line <= segment.end_line);
    if !segments.iter().any(|segment| !segment.kept) {
        return None;
    }
    Some(segments)
}

fn collect_holes(
    node: Node<'_, ast_grep_core::tree_sitter::StrDoc<Lang>>,
    total: usize,
    min_body_lines: usize,
    holes: &mut Vec<(usize, usize)>,
) {
    for child in node.named_children() {
        let body = child.field("body").or_else(|| child.field("declaration_list"));
        if let Some(body) = body {
            let body_start = body.start_pos().line() + 1;
            let body_end = (body.end_pos().line() + 1).min(total);
            if body_end > body_start && body_end - body_start + 1 > min_body_lines {
                let hole_start = body_start + 1;
                let hole_end = body_end.saturating_sub(1);
                if hole_end >= hole_start {
                    holes.push((hole_start, hole_end));
                }
            }
            continue;
        }
        collect_holes(child, total, min_body_lines, holes);
    }
}

fn segments_from(kept: Vec<(usize, usize)>, holes: Vec<(usize, usize)>) -> Vec<Segment> {
    let mut segments: Vec<Segment> = kept
        .into_iter()
        .map(|(start, end)| Segment {
            kept: true,
            start_line: start,
            end_line: end,
        })
        .collect();
    segments.extend(holes.into_iter().map(|(start, end)| Segment {
        kept: false,
        start_line: start,
        end_line: end,
    }));
    segments.sort_by_key(|segment| segment.start_line);
    segments
}
