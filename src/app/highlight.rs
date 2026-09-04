//! Viewport-driven syntax highlighting.
//!
//! Backends hand over diff lines without spans. Before each frame the diff
//! view asks for the hunks its rows touch, plus a margin above and below, to
//! be highlighted, and the work is capped per frame so a jump into a huge
//! hunk never freezes the interface: the rows show in plain diff colours and
//! fill in over the next few frames.

use super::*;

/// Most diff lines highlighted in one frame. A hunk is only ever advanced a
/// window at a time, so the cap is what stops several long hunks from being
/// parsed in the same frame.
const HIGHLIGHT_LINES_PER_FRAME: usize = 1_000;

/// Rows highlighted beyond each edge of the viewport, so ordinary scrolling
/// lands on lines that already have their spans.
const HIGHLIGHT_MARGIN_ROWS: usize = 60;

impl App {
    /// Highlight the hunks whose lines the next frame will show, within
    /// `HIGHLIGHT_LINES_PER_FRAME`. Sets `highlight_pending` when the budget
    /// ran out before every visible line had its spans, so the event loop
    /// schedules another frame to continue.
    pub(crate) fn highlight_visible_lines(&mut self, viewport_rows: usize) {
        let first = self
            .diff_state
            .scroll_offset
            .saturating_sub(HIGHLIGHT_MARGIN_ROWS);
        let last = self
            .diff_state
            .scroll_offset
            .saturating_add(viewport_rows)
            .saturating_add(HIGHLIGHT_MARGIN_ROWS)
            .min(self.line_annotations.len());
        if first >= last {
            self.highlight_pending = false;
            return;
        }

        // Highest line index needed per hunk, in first-seen order so the
        // rows nearest the top of the range are served first.
        let mut needs: Vec<((usize, usize), usize)> = Vec::new();
        for annotation in &self.line_annotations[first..last] {
            let (key, line_idx) = match annotation {
                AnnotatedLine::DiffLine {
                    file_idx,
                    hunk_idx,
                    line_idx,
                    ..
                } => ((*file_idx, *hunk_idx), *line_idx),
                AnnotatedLine::SideBySideLine {
                    file_idx,
                    hunk_idx,
                    del_line_idx,
                    add_line_idx,
                    ..
                } => (
                    (*file_idx, *hunk_idx),
                    del_line_idx.unwrap_or(0).max(add_line_idx.unwrap_or(0)),
                ),
                _ => continue,
            };
            match needs.iter_mut().find(|(k, _)| *k == key) {
                Some((_, upto)) => *upto = (*upto).max(line_idx),
                None => needs.push((key, line_idx)),
            }
        }

        let highlighter = self.theme.syntax_highlighter();
        let mut budget = HIGHLIGHT_LINES_PER_FRAME;
        let mut pending = false;
        for ((file_idx, hunk_idx), upto) in needs {
            let Some(file) = self.diff_files.get_mut(file_idx) else {
                continue;
            };
            if file
                .hunks
                .get(hunk_idx)
                .is_none_or(|hunk| hunk.highlight.covers(upto))
            {
                continue;
            }
            if budget == 0 {
                pending = true;
                break;
            }
            let processed = highlighter.advance_file(file, hunk_idx, upto, budget);
            budget = budget.saturating_sub(processed);
            if !file.hunks[hunk_idx].highlight.covers(upto) {
                pending = true;
            }
        }
        self.highlight_pending = pending;
    }
}
