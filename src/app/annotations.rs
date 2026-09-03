use super::*;

impl App {
    /// Ensure the file line count cache is populated for a given file.
    pub(in crate::app) fn ensure_file_line_count_cached(&mut self, file_idx: usize) {
        if !self.eof_gap_enabled() || self.file_line_count_cache.contains_key(&file_idx) {
            return;
        }
        if let Some(file) = self.diff_files.get(file_idx) {
            let old_path = file.old_path.clone();
            let new_path = file.new_path.clone();
            let status = file.status;

            let path = new_path.or(old_path).unwrap_or_default();
            let ref_commit = self.ref_commit().map(|s| s.to_string());
            let count = self
                .vcs
                .file_line_count(&path, status, ref_commit.as_deref())
                .ok();

            if let Some(c) = count {
                self.file_line_count_cache.insert(file_idx, c);
            }
        }
    }

    /// Populate the file line count cache for all eligible files.
    /// Only enabled for diff sources where the worktree/index is the correct snapshot.
    pub(in crate::app) fn populate_file_line_count_cache(&mut self) {
        self.file_line_count_cache.clear();
        if self.eof_gap_enabled() {
            for file_idx in 0..self.diff_files.len() {
                let file = &self.diff_files[file_idx];
                if !file.hunks.is_empty() && file.status != FileStatus::Deleted {
                    self.ensure_file_line_count_cached(file_idx);
                }
            }
        }
    }

    /// Rebuild the line annotations cache. Call this when:
    /// - Diff files change (load/reload)
    /// - Expansion state changes (expand/collapse gap)
    /// - Comments are added/removed
    /// - Diff view mode changes
    pub fn rebuild_annotations(&mut self) {
        if self.file_line_count_cache.is_empty() {
            self.populate_file_line_count_cache();
        }

        self.line_annotations.clear();

        // Commit-selection filter: comments scoped to a commit outside the
        // current inline selection are hidden. `None` => no selector, show all.
        let commit_set = self.selected_commit_set();

        // The review-comments header is omitted in single-file view and
        // until the section has content (see the matching guard in
        // `src/ui/diff_unified.rs`), so the annotation list mirrors the
        // render.
        if self.show_review_comments_header() {
            self.line_annotations
                .push(AnnotatedLine::ReviewCommentsHeader);
        }
        for (comment_idx, comment) in self.session.review_comments.iter().enumerate() {
            let comment_lines =
                Self::comment_display_lines(comment, self.diff_state.viewport_width);
            for _ in 0..comment_lines {
                self.line_annotations
                    .push(AnnotatedLine::ReviewComment { comment_idx });
            }
        }

        for (file_idx, file) in self.diff_files.iter().enumerate() {
            // Single-file view renders only the currently focused file,
            // so the annotation stream must skip every other file or
            // click handling lands on lines that aren't visible.
            if self.is_single_file_view && file_idx != self.diff_state.current_file_idx {
                continue;
            }
            // Hidden by an include/exclude filter: emit no annotations, the
            // same way the renderers emit no lines.
            if !self.file_passes_filter(file) {
                continue;
            }
            let path = file.display_path();

            // File header (only when shown — same gate as the renderer).
            if !self.is_single_file_view {
                self.line_annotations
                    .push(AnnotatedLine::FileHeader { file_idx });
            }

            // Reviewed files normally collapse to their header in continuous
            // view. A summary jump can reveal its target file without clearing
            // the persisted marker. Single-file view also shows reviewed file
            // bodies, under a banner that needs a matching annotation row.
            if self.should_collapse_file(file_idx) {
                continue;
            }
            if self.session.is_file_reviewed(path) && self.is_single_file_view {
                self.line_annotations
                    .push(AnnotatedLine::ReviewedBanner { file_idx });
            }

            // File comments
            if let Some(review) = self.session.files.get(path) {
                for (comment_idx, comment) in review.file_comments.iter().enumerate() {
                    if !Self::comment_visible_with(comment, commit_set.as_ref()) {
                        continue;
                    }
                    let comment_lines =
                        Self::comment_display_lines(comment, self.diff_state.viewport_width);
                    for _ in 0..comment_lines {
                        self.line_annotations.push(AnnotatedLine::FileComment {
                            file_idx,
                            comment_idx,
                        });
                    }
                }
            }

            if file.is_binary || file.hunks.is_empty() {
                self.line_annotations
                    .push(AnnotatedLine::BinaryOrEmpty { file_idx });
            } else {
                // Get line comments for this file
                let line_comments = self
                    .session
                    .files
                    .get(path)
                    .map(|r| &r.line_comments)
                    .cloned()
                    .unwrap_or_default();

                for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                    // Calculate gap before this hunk
                    let prev_hunk = if hunk_idx > 0 {
                        file.hunks.get(hunk_idx - 1)
                    } else {
                        None
                    };
                    let gap = calculate_gap(
                        prev_hunk.map(|h| (&h.new_start, &h.new_count)),
                        hunk.new_start,
                    );

                    let gap_id = GapId { file_idx, hunk_idx };

                    if gap > 0 && self.should_render_gap_before_hunk(file_idx, hunk_idx) {
                        let top_len = self.expanded_top.get(&gap_id).map_or(0, |v| v.len());
                        let bot_len = self.expanded_bottom.get(&gap_id).map_or(0, |v| v.len());
                        let remaining = (gap as usize).saturating_sub(top_len + bot_len);
                        let is_top_of_file = hunk_idx == 0;

                        // Sequential line_idx counter across top + bottom
                        let mut ctx_idx = 0;

                        // --- Top expanded lines (↓ direction) ---
                        for _ in 0..top_len {
                            self.line_annotations.push(AnnotatedLine::ExpandedContext {
                                gap_id: gap_id.clone(),
                                line_idx: ctx_idx,
                            });
                            ctx_idx += 1;
                        }

                        // --- Expanders / hidden lines ---
                        if remaining > 0 {
                            if is_top_of_file {
                                // Top-of-file: HiddenLines (if > batch) + ↑
                                if remaining > GAP_EXPAND_BATCH {
                                    self.line_annotations.push(AnnotatedLine::HiddenLines {
                                        gap_id: gap_id.clone(),
                                        count: remaining,
                                    });
                                }
                                self.line_annotations.push(AnnotatedLine::Expander {
                                    gap_id: gap_id.clone(),
                                    direction: ExpandDirection::Up,
                                });
                            } else if remaining >= GAP_EXPAND_BATCH {
                                // Between-hunk, large: ↓ + HiddenLines + ↑
                                self.line_annotations.push(AnnotatedLine::Expander {
                                    gap_id: gap_id.clone(),
                                    direction: ExpandDirection::Down,
                                });
                                self.line_annotations.push(AnnotatedLine::HiddenLines {
                                    gap_id: gap_id.clone(),
                                    count: remaining,
                                });
                                self.line_annotations.push(AnnotatedLine::Expander {
                                    gap_id: gap_id.clone(),
                                    direction: ExpandDirection::Up,
                                });
                            } else {
                                // Between-hunk, small: merged ↕
                                self.line_annotations.push(AnnotatedLine::Expander {
                                    gap_id: gap_id.clone(),
                                    direction: ExpandDirection::Both,
                                });
                            }
                        }

                        // --- Bottom expanded lines (↑ direction) ---
                        for _ in 0..bot_len {
                            self.line_annotations.push(AnnotatedLine::ExpandedContext {
                                gap_id: gap_id.clone(),
                                line_idx: ctx_idx,
                            });
                            ctx_idx += 1;
                        }
                    }

                    // Hunk header
                    self.line_annotations
                        .push(AnnotatedLine::HunkHeader { file_idx, hunk_idx });
                    if self.should_collapse_hunk(file_idx, hunk_idx) {
                        continue;
                    }

                    // Diff lines - handle differently based on view mode
                    match self.diff_view_mode {
                        DiffViewMode::Unified => {
                            Self::build_unified_diff_annotations(
                                &mut self.line_annotations,
                                file_idx,
                                hunk_idx,
                                &hunk.lines,
                                &line_comments,
                                self.diff_state.viewport_width,
                                commit_set.as_ref(),
                            );
                        }
                        DiffViewMode::SideBySide => {
                            Self::build_side_by_side_annotations(
                                &mut self.line_annotations,
                                file_idx,
                                hunk_idx,
                                &hunk.lines,
                                &line_comments,
                                self.diff_state.viewport_width,
                                commit_set.as_ref(),
                            );
                        }
                    }
                }

                // End-of-file gap (after all hunks, not for deleted files)
                if file.status != FileStatus::Deleted
                    && let Some(last_hunk) = file.hunks.last()
                {
                    let eof_start = last_hunk.new_start + last_hunk.new_count;
                    if let Some(&total) = self.file_line_count_cache.get(&file_idx)
                        && eof_start <= total
                    {
                        let gap = (total - eof_start + 1) as usize;
                        let eof_gap_id = GapId {
                            file_idx,
                            hunk_idx: file.hunks.len(),
                        };

                        let top_len = self.expanded_top.get(&eof_gap_id).map_or(0, |v| v.len());
                        let bot_len = self.expanded_bottom.get(&eof_gap_id).map_or(0, |v| v.len());
                        let remaining = gap.saturating_sub(top_len + bot_len);

                        let mut ctx_idx = 0;

                        // Top expanded lines (↓ direction)
                        for _ in 0..top_len {
                            self.line_annotations.push(AnnotatedLine::ExpandedContext {
                                gap_id: eof_gap_id.clone(),
                                line_idx: ctx_idx,
                            });
                            ctx_idx += 1;
                        }

                        // Expanders / hidden lines
                        if remaining > 0 {
                            self.line_annotations.push(AnnotatedLine::Expander {
                                gap_id: eof_gap_id.clone(),
                                direction: ExpandDirection::Down,
                            });
                            if remaining > GAP_EXPAND_BATCH {
                                self.line_annotations.push(AnnotatedLine::HiddenLines {
                                    gap_id: eof_gap_id.clone(),
                                    count: remaining,
                                });
                            }
                        }

                        // Bottom expanded lines (↑ direction)
                        for _ in 0..bot_len {
                            self.line_annotations.push(AnnotatedLine::ExpandedContext {
                                gap_id: eof_gap_id.clone(),
                                line_idx: ctx_idx,
                            });
                            ctx_idx += 1;
                        }
                    }
                }
            }

            // Spacing line
            self.line_annotations.push(AnnotatedLine::Spacing);
        }

        self.refresh_search_matches();
    }

    fn push_comments(
        annotations: &mut Vec<AnnotatedLine>,
        file_idx: usize,
        line_no: Option<u32>,
        line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
        side: LineSide,
        viewport_width: usize,
        commit_set: Option<&std::collections::HashSet<String>>,
    ) {
        let Some(ln) = line_no else {
            return;
        };

        let Some(comments) = line_comments.get(&ln) else {
            return;
        };

        for (idx, comment) in comments.iter().enumerate() {
            let matches_side =
                comment.side == Some(side) || (side == LineSide::New && comment.side.is_none());

            if !matches_side {
                continue;
            }

            // Hide comments scoped to a commit outside the current selection.
            // Uses the shared predicate so height math and rendering agree.
            if !Self::comment_visible_with(comment, commit_set) {
                continue;
            }

            let comment_lines = Self::comment_display_lines(comment, viewport_width);
            for _ in 0..comment_lines {
                annotations.push(AnnotatedLine::LineComment {
                    file_idx,
                    line: ln,
                    comment_idx: idx,
                    side,
                });
            }
        }
    }

    /// Build annotations for unified diff mode (one annotation per diff line)
    fn build_unified_diff_annotations(
        annotations: &mut Vec<AnnotatedLine>,
        file_idx: usize,
        hunk_idx: usize,
        lines: &[crate::model::DiffLine],
        line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
        viewport_width: usize,
        commit_set: Option<&std::collections::HashSet<String>>,
    ) {
        for (line_idx, diff_line) in lines.iter().enumerate() {
            annotations.push(AnnotatedLine::DiffLine {
                file_idx,
                hunk_idx,
                line_idx,
                old_lineno: diff_line.old_lineno,
                new_lineno: diff_line.new_lineno,
            });

            // Line comments on old side (delete lines)
            if let Some(old_ln) = diff_line.old_lineno {
                Self::push_comments(
                    annotations,
                    file_idx,
                    Some(old_ln),
                    line_comments,
                    LineSide::Old,
                    viewport_width,
                    commit_set,
                );
            }

            // Line comments on new side (added/context lines)
            if let Some(new_ln) = diff_line.new_lineno {
                Self::push_comments(
                    annotations,
                    file_idx,
                    Some(new_ln),
                    line_comments,
                    LineSide::New,
                    viewport_width,
                    commit_set,
                );
            }
        }
    }

    /// Build annotations for side-by-side diff mode, pairing deletions and additions into aligned rows.
    fn build_side_by_side_annotations(
        annotations: &mut Vec<AnnotatedLine>,
        file_idx: usize,
        hunk_idx: usize,
        lines: &[crate::model::DiffLine],
        line_comments: &std::collections::HashMap<u32, Vec<crate::model::Comment>>,
        viewport_width: usize,
        commit_set: Option<&std::collections::HashSet<String>>,
    ) {
        let mut i = 0;
        while i < lines.len() {
            let diff_line = &lines[i];

            match diff_line.origin {
                LineOrigin::Context => {
                    annotations.push(AnnotatedLine::SideBySideLine {
                        file_idx,
                        hunk_idx,
                        del_line_idx: Some(i),
                        add_line_idx: Some(i),
                        old_lineno: diff_line.old_lineno,
                        new_lineno: diff_line.new_lineno,
                    });

                    Self::push_comments(
                        annotations,
                        file_idx,
                        diff_line.new_lineno,
                        line_comments,
                        LineSide::New,
                        viewport_width,
                        commit_set,
                    );
                    i += 1
                }

                LineOrigin::Deletion => {
                    // Find consecutive deletions
                    let del_start = i;
                    let mut del_end = i + 1;
                    while del_end < lines.len() && lines[del_end].origin == LineOrigin::Deletion {
                        del_end += 1;
                    }

                    // Find consecutive additions following deletions
                    let add_start = del_end;
                    let mut add_end = add_start;
                    while add_end < lines.len() && lines[add_end].origin == LineOrigin::Addition {
                        add_end += 1;
                    }

                    let del_count = del_end - del_start;
                    let add_count = add_end - add_start;
                    let max_lines = del_count.max(add_count);

                    for offset in 0..max_lines {
                        let del_idx = if offset < del_count {
                            Some(del_start + offset)
                        } else {
                            None
                        };
                        let add_idx = if offset < add_count {
                            Some(add_start + offset)
                        } else {
                            None
                        };

                        let old_lineno = del_idx.and_then(|idx| lines[idx].old_lineno);
                        let new_lineno = add_idx.and_then(|idx| lines[idx].new_lineno);

                        annotations.push(AnnotatedLine::SideBySideLine {
                            file_idx,
                            hunk_idx,
                            del_line_idx: del_idx,
                            add_line_idx: add_idx,
                            old_lineno,
                            new_lineno,
                        });

                        Self::push_comments(
                            annotations,
                            file_idx,
                            old_lineno,
                            line_comments,
                            LineSide::Old,
                            viewport_width,
                            commit_set,
                        );
                        Self::push_comments(
                            annotations,
                            file_idx,
                            new_lineno,
                            line_comments,
                            LineSide::New,
                            viewport_width,
                            commit_set,
                        );
                    }

                    i = add_end;
                }
                LineOrigin::Addition => {
                    annotations.push(AnnotatedLine::SideBySideLine {
                        file_idx,
                        hunk_idx,
                        del_line_idx: None,
                        add_line_idx: Some(i),
                        old_lineno: None,
                        new_lineno: diff_line.new_lineno,
                    });

                    Self::push_comments(
                        annotations,
                        file_idx,
                        diff_line.new_lineno,
                        line_comments,
                        LineSide::New,
                        viewport_width,
                        commit_set,
                    );
                    i += 1;
                }
            }
        }
    }
}
