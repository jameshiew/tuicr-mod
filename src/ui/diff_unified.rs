use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    App, DiffSource, ExpandDirection, FocusedPanel, GAP_EXPAND_BATCH, GapId, InputMode,
};
use crate::model::{FileStatus, LineOrigin, LineRange, LineSide};
use crate::theme::Theme;
use crate::ui::comment_panel;
use crate::ui::diff_view::{
    apply_horizontal_scroll, comment_box_visible, comment_type_presentation, cursor_indicator,
    cursor_indicator_spaced, diff_stat_title, hunk_header_text_and_style,
    paint_cursor_line_highlight, paint_unified_diff_rows_with, paint_visual_selection_overlay,
    populate_row_to_annotation, push_comment_bar, render_expander_line, render_hidden_lines,
    scroll_comment_input_into_view, skip_comment_box, unified_line_bg_style,
};
use crate::ui::styles;
use crate::vcs::git::calculate_gap;

pub(super) fn render_unified_diff(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focused_panel == FocusedPanel::Diff;

    let title = crate::ui::diff_view::diff_title(app, area.width);

    let block = Block::default()
        .title(title)
        .title_top(diff_stat_title(app).right_aligned())
        .borders(Borders::ALL)
        .style(styles::panel_style(&app.theme))
        .border_style(styles::border_style(&app.theme, focused));

    let inner = block.inner(area);
    let comment_width = inner.width.saturating_sub(1) as usize;
    frame.render_widget(block, area);

    // Update viewport height for scroll calculations
    app.diff_state.viewport_height = inner.height as usize;
    app.diff_inner_area = Some(inner);

    // Reset comment input annotation offset (will be set if a comment input box is rendered)
    app.comment_input_annotation_offset = None;

    let lw = app.lineno_width();

    // Build all diff lines for infinite scroll
    // Track line index to mark the current line (cursor position)
    let mut lines: Vec<Line> = Vec::new();
    let mut line_idx: usize = 0;
    let current_line_idx = app.diff_state.cursor_line;

    // Only build the expensive per-diff-line spans for lines that are actually
    // visible. Everything else still pushes (cheap) so `lines.len()` keeps
    // matching `line_idx`, but the hot inner loops push `Line::default()` for
    // off-screen rows. In Comment mode the scroll offset may be adjusted after
    // building, so fall back to a full build there.
    let (visible_start, visible_end) = crate::ui::diff_view::diff_visible_range(app, inner);
    let search_style = styles::search_match_style(&app.theme);

    // Track cursor position for IME when in Comment mode
    // Store the logical line index and column where the cursor should be
    let mut comment_cursor_logical_line: Option<usize> = None;
    let mut comment_cursor_column: u16 = 0;
    // Track the full extent of the comment input box so we can auto-scroll
    // the viewport to keep it visible while the user types.
    let mut comment_input_box_range: Option<(usize, usize)> = None;
    // Records per-comment bar info — populated at each line-level comment
    // call site and consumed by the bar paint pass at the end of render.
    let mut comment_bars: Vec<crate::ui::diff_view::CommentBarAnchor> = Vec::new();

    let is_review_comment_mode =
        app.input_mode == InputMode::Comment && app.comment_is_review_level;

    // The `═══ Review Comments ═══` label is redundant in single-file
    // view (review-level comments are still rendered below; they just
    // don't need a banner that confuses horizontal scroll). It's also hidden
    // while the section has no content.
    if app.show_review_comments_header() {
        let general_indicator = cursor_indicator_spaced(line_idx, current_line_idx);
        lines.push(Line::from(vec![
            Span::styled(
                general_indicator,
                styles::current_line_indicator_style(&app.theme),
            ),
            Span::styled(
                crate::ui::diff_view::REVIEW_COMMENTS_HEADER_PREFIX,
                styles::file_header_style(&app.theme),
            ),
            Span::styled(
                crate::ui::diff_view::HEADER_RULE,
                styles::file_header_style(&app.theme),
            ),
        ]));
        line_idx += 1;
    }

    for comment in &app.session.review_comments {
        let is_being_edited =
            app.editing_comment_id.as_ref() == Some(&comment.id) && is_review_comment_mode;

        if is_being_edited {
            let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                &app.theme,
                comment_type_presentation(app, &app.comment_type),
                &app.comment_buffer,
                app.comment_cursor,
                None,
                true,
                comment_width,
                app.comment_vim_mode_label()
                    .as_ref()
                    .map(|(t, w)| (t.as_str(), *w)),
                app.supports_keyboard_enhancement,
            );
            comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
            comment_cursor_column = 1 + cursor_info.column;
            comment_input_box_range =
                Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
            let annotations_replaced = App::comment_display_lines(comment, inner.width as usize);
            app.comment_input_annotation_offset =
                Some((line_idx, input_lines.len(), annotations_replaced));

            for mut input_line in input_lines {
                let indicator = cursor_indicator(line_idx, current_line_idx);
                input_line.spans.insert(
                    0,
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                );
                lines.push(input_line);
                line_idx += 1;
            }
        } else {
            let rows = App::comment_display_lines(comment, inner.width as usize);
            if !comment_box_visible(line_idx, rows, (visible_start, visible_end)) {
                skip_comment_box(&mut lines, &mut line_idx, rows);
                continue;
            }
            let comment_lines = comment_panel::format_comment_lines(
                &app.theme,
                comment_type_presentation(app, &comment.comment_type),
                &comment.content,
                None,
                comment_width,
                (comment.author != app.username).then_some(comment.author.as_str()),
            );
            for mut comment_line in comment_lines {
                let indicator = cursor_indicator(line_idx, current_line_idx);
                comment_line.spans.insert(
                    0,
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                );
                lines.push(comment_line);
                line_idx += 1;
            }
        }
    }

    if is_review_comment_mode && app.editing_comment_id.is_none() {
        let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
            &app.theme,
            comment_type_presentation(app, &app.comment_type),
            &app.comment_buffer,
            app.comment_cursor,
            None,
            false,
            comment_width,
            app.comment_vim_mode_label()
                .as_ref()
                .map(|(t, w)| (t.as_str(), *w)),
            app.supports_keyboard_enhancement,
        );
        comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
        comment_cursor_column = 1 + cursor_info.column;
        comment_input_box_range = Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
        app.comment_input_annotation_offset = Some((line_idx, input_lines.len(), 0));

        for mut input_line in input_lines {
            let indicator = cursor_indicator(line_idx, current_line_idx);
            input_line.spans.insert(
                0,
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
            );
            lines.push(input_line);
            line_idx += 1;
        }
    }

    for (file_idx, file) in app.diff_files.iter().enumerate() {
        // Single-file view hides every file except the one the cursor is
        // currently on. Navigation (`}`/`{`, file list) flips
        // `current_file_idx` and the next render shows the new file.
        if app.is_single_file_view && file_idx != app.diff_state.current_file_idx {
            continue;
        }
        // File-tree include/exclude filters hide files from the diff too.
        // Must stay in lockstep with `App::file_render_height`, which counts
        // these files as zero lines.
        if !app.file_passes_filter(file) {
            continue;
        }
        let path = file.display_path();
        let is_reviewed = app.session.is_file_reviewed(path);

        // The `═══ filename ═══` separator is redundant in single-file
        // view: the status bar and file list already name the file, and
        // the wide bar of `═` characters confuses horizontal scrolling.
        if !app.is_single_file_view {
            let indicator = cursor_indicator_spaced(line_idx, current_line_idx);
            let header_text = crate::ui::diff_view::file_header_prefix_text(app, file);
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(header_text, styles::file_header_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::HEADER_RULE,
                    styles::file_header_style(&app.theme),
                ),
            ]));
            line_idx += 1;
        }

        // Reviewed files normally collapse in continuous view. A summary jump
        // may reveal one target body without changing its reviewed marker.
        if app.should_collapse_file(file_idx) {
            continue;
        }
        if is_reviewed && app.is_single_file_view {
            let indicator = cursor_indicator(line_idx, current_line_idx);
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::REVIEWED_BANNER_TEXT,
                    Style::default()
                        .fg(app.theme.fg_secondary)
                        .add_modifier(Modifier::DIM),
                ),
            ]));
            line_idx += 1;
        }

        // Check if we're editing/adding a file-level comment for this file
        let is_file_comment_mode = app.input_mode == InputMode::Comment
            && app.comment_is_file_level
            && file_idx == app.diff_state.current_file_idx;

        // Show file-level comments right after the header
        if let Some(review) = app.session.files.get(path) {
            for comment in &review.file_comments {
                if !app.comment_visible(comment) {
                    continue;
                }
                // Skip rendering this comment if it's being edited
                let is_being_edited =
                    app.editing_comment_id.as_ref() == Some(&comment.id) && is_file_comment_mode;

                if is_being_edited {
                    // Render the inline input instead
                    let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                        &app.theme,
                        comment_type_presentation(app, &app.comment_type),
                        &app.comment_buffer,
                        app.comment_cursor,
                        None,
                        true,
                        comment_width,
                        app.comment_vim_mode_label()
                            .as_ref()
                            .map(|(t, w)| (t.as_str(), *w)),
                        app.supports_keyboard_enhancement,
                    );
                    // Track cursor position: logical line = current line_idx + cursor offset within input
                    comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
                    // Column = indicator (1) + cursor_info.column
                    comment_cursor_column = 1 + cursor_info.column;
                    comment_input_box_range =
                        Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
                    let annotations_replaced =
                        App::comment_display_lines(comment, inner.width as usize);
                    app.comment_input_annotation_offset =
                        Some((line_idx, input_lines.len(), annotations_replaced));

                    for mut input_line in input_lines {
                        let indicator = cursor_indicator(line_idx, current_line_idx);
                        input_line.spans.insert(
                            0,
                            Span::styled(
                                indicator,
                                styles::current_line_indicator_style(&app.theme),
                            ),
                        );
                        lines.push(input_line);
                        line_idx += 1;
                    }
                } else {
                    let rows = App::comment_display_lines(comment, inner.width as usize);
                    if !comment_box_visible(line_idx, rows, (visible_start, visible_end)) {
                        skip_comment_box(&mut lines, &mut line_idx, rows);
                        continue;
                    }
                    let comment_lines = comment_panel::format_comment_lines(
                        &app.theme,
                        comment_type_presentation(app, &comment.comment_type),
                        &comment.content,
                        None,
                        comment_width,
                        (comment.author != app.username).then_some(comment.author.as_str()),
                    );
                    for mut comment_line in comment_lines {
                        let indicator = cursor_indicator(line_idx, current_line_idx);
                        comment_line.spans.insert(
                            0,
                            Span::styled(
                                indicator,
                                styles::current_line_indicator_style(&app.theme),
                            ),
                        );
                        lines.push(comment_line);
                        line_idx += 1;
                    }
                }
            }
        }

        // Render inline input for new file-level comment
        if is_file_comment_mode && app.editing_comment_id.is_none() {
            let (input_lines, cursor_info) = comment_panel::format_comment_input_lines(
                &app.theme,
                comment_type_presentation(app, &app.comment_type),
                &app.comment_buffer,
                app.comment_cursor,
                None,
                false,
                comment_width,
                app.comment_vim_mode_label()
                    .as_ref()
                    .map(|(t, w)| (t.as_str(), *w)),
                app.supports_keyboard_enhancement,
            );
            // Track cursor position
            comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
            comment_cursor_column = 1 + cursor_info.column;
            comment_input_box_range =
                Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
            app.comment_input_annotation_offset = Some((line_idx, input_lines.len(), 0));

            for mut input_line in input_lines {
                let indicator = cursor_indicator(line_idx, current_line_idx);
                input_line.spans.insert(
                    0,
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                );
                lines.push(input_line);
                line_idx += 1;
            }
        }

        if file.is_too_large || file.is_binary || file.hunks.is_empty() {
            let indicator = cursor_indicator_spaced(line_idx, current_line_idx);
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::binary_or_empty_label(file),
                    styles::dim_style(&app.theme),
                ),
            ]));
            line_idx += 1;
        } else {
            // Get line comments for this file
            let line_comments = app
                .session
                .files
                .get(path)
                .map(|r| &r.line_comments)
                .unwrap_or(&crate::ui::diff_view::EMPTY_LINE_COMMENTS);

            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                // Calculate and render gap before this hunk
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

                if gap > 0 && app.should_render_gap_before_hunk(file_idx, hunk_idx) {
                    let top_lines = app.expanded_top.get(&gap_id);
                    let bot_lines = app.expanded_bottom.get(&gap_id);
                    let top_len = top_lines.map_or(0, |v| v.len());
                    let bot_len = bot_lines.map_or(0, |v| v.len());
                    let remaining = (gap as usize).saturating_sub(top_len + bot_len);
                    let is_top_of_file = hunk_idx == 0;

                    // Render top expanded lines
                    if let Some(top) = top_lines {
                        for expanded_line in top {
                            if line_idx < visible_start || line_idx >= visible_end {
                                lines.push(Line::default());
                                line_idx += 1;
                                continue;
                            }
                            let line_search = app
                                .search_paint_at(line_idx)
                                .map(|needle| (needle, search_style));
                            render_expanded_context_line(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                expanded_line,
                                &app.theme,
                                lw,
                                app.relative_line_numbers,
                                line_search,
                            );
                        }
                    }

                    // Render expanders / hidden lines
                    if remaining > 0 {
                        if is_top_of_file {
                            if remaining > GAP_EXPAND_BATCH {
                                render_hidden_lines(
                                    &mut lines,
                                    &mut line_idx,
                                    current_line_idx,
                                    remaining,
                                    &app.theme,
                                );
                            }
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                ExpandDirection::Up,
                                remaining,
                                &app.theme,
                            );
                        } else if remaining >= GAP_EXPAND_BATCH {
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                ExpandDirection::Down,
                                remaining,
                                &app.theme,
                            );
                            render_hidden_lines(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                remaining,
                                &app.theme,
                            );
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                ExpandDirection::Up,
                                remaining,
                                &app.theme,
                            );
                        } else {
                            render_expander_line(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                ExpandDirection::Both,
                                remaining,
                                &app.theme,
                            );
                        }
                    }

                    // Render bottom expanded lines
                    if let Some(bot) = bot_lines {
                        for expanded_line in bot {
                            if line_idx < visible_start || line_idx >= visible_end {
                                lines.push(Line::default());
                                line_idx += 1;
                                continue;
                            }
                            let line_search = app
                                .search_paint_at(line_idx)
                                .map(|needle| (needle, search_style));
                            render_expanded_context_line(
                                &mut lines,
                                &mut line_idx,
                                current_line_idx,
                                expanded_line,
                                &app.theme,
                                lw,
                                app.relative_line_numbers,
                                line_search,
                            );
                        }
                    }
                }

                // Hunk header
                let is_hunk_reviewed = app.is_hunk_reviewed(file_idx, hunk_idx);
                let (hunk_header_text, hunk_header_style) =
                    hunk_header_text_and_style(&app.theme, hunk, is_hunk_reviewed);
                let indicator = cursor_indicator_spaced(line_idx, current_line_idx);
                lines.push(Line::from(vec![
                    Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                    Span::styled(hunk_header_text, hunk_header_style),
                ]));
                line_idx += 1;
                if app.should_collapse_hunk(file_idx, hunk_idx) {
                    continue;
                }

                // Diff lines
                for diff_line in &hunk.lines {
                    // Hot path: skip span/style allocation entirely for diff
                    // lines outside the viewport. Comment handling below still
                    // runs so `line_idx` stays exact and any comment box that
                    // crosses into the viewport is rendered.
                    if line_idx < visible_start || line_idx >= visible_end {
                        lines.push(Line::default());
                        line_idx += 1;
                    } else {
                        let base_style = match diff_line.origin {
                            LineOrigin::Addition => styles::diff_add_style(&app.theme),
                            LineOrigin::Deletion => styles::diff_del_style(&app.theme),
                            LineOrigin::Context => styles::diff_context_style(&app.theme),
                        };
                        let style = base_style;
                        // A commit message is prose, not code: render it without
                        // line numbers, matching the side-by-side view.
                        let line_num_str = if file.is_commit_message {
                            " ".repeat(lw + 1)
                        } else if app.relative_line_numbers {
                            crate::ui::diff_view::relative_line_number_field(
                                diff_line.new_lineno.or(diff_line.old_lineno),
                                line_idx,
                                current_line_idx,
                                lw,
                            )
                        } else {
                            crate::ui::diff_view::unified_line_number_field(diff_line, lw)
                        };
                        let prefix = crate::ui::diff_view::unified_line_origin_marker(diff_line);

                        let indicator = cursor_indicator(line_idx, current_line_idx);

                        let line_num_style = styles::dim_style(&app.theme);

                        let mut line_spans = vec![
                            Span::styled(
                                indicator,
                                styles::current_line_indicator_style(&app.theme),
                            ),
                            Span::styled(line_num_str, line_num_style),
                            Span::styled(format!("{prefix} "), style),
                        ];
                        let content_start = line_spans.len();

                        if let Some(ref highlighted) = diff_line.highlighted_spans {
                            for (span_style, span_text) in highlighted {
                                line_spans.push(Span::styled(span_text.clone(), *span_style));
                            }
                        } else {
                            line_spans.push(Span::styled(diff_line.content.clone(), style));
                        }

                        // Mark add/del lines with their effective EOL style so we can paint full
                        // row backgrounds later (including wrapped visual rows).
                        let eol_marker = matches!(
                            diff_line.origin,
                            LineOrigin::Addition | LineOrigin::Deletion
                        )
                        .then(|| {
                            let eol_style = match diff_line.highlighted_spans.as_ref() {
                                // For syntax-highlighted lines (including empty highlighted lines),
                                // use syntax diff background so row fill matches code spans.
                                Some(_) => {
                                    let syntax_bg = match diff_line.origin {
                                        LineOrigin::Addition => app.theme.syntax_add_bg,
                                        LineOrigin::Deletion => app.theme.syntax_del_bg,
                                        LineOrigin::Context => app.theme.panel_bg,
                                    };
                                    let base = line_spans.last().map(|s| s.style).unwrap_or(style);
                                    base.bg(syntax_bg)
                                }
                                // Non-highlighted lines keep classic diff background.
                                None => line_spans.last().map(|s| s.style).unwrap_or(style),
                            };
                            // Zero-width marker span carrying the background style.
                            Span::styled(String::new(), eol_style)
                        });

                        if let Some(needle) = app.search_paint_at(line_idx) {
                            let content_spans = line_spans.split_off(content_start);
                            line_spans.extend(crate::ui::text_utils::apply_search_highlight_spans(
                                content_spans,
                                needle,
                                search_style,
                            ));
                        }
                        line_spans.extend(eol_marker);

                        lines.push(Line::from(line_spans));
                        line_idx += 1;
                    }

                    // Show line comments for both old side (deleted lines) and new side (added/context)
                    // Old side comments (for deleted lines)
                    if let Some(old_ln) = diff_line.old_lineno {
                        // Check if we're adding/editing a comment on this line (old side)
                        let is_line_comment_mode = app.input_mode == InputMode::Comment
                            && !app.comment_is_file_level
                            && file_idx == app.diff_state.current_file_idx
                            && app.comment_line == Some((old_ln, LineSide::Old));

                        if let Some(comments) = line_comments.get(&old_ln) {
                            for comment in comments {
                                if comment.side == Some(LineSide::Old)
                                    && app.comment_visible(comment)
                                {
                                    // Skip if this comment is being edited
                                    let is_being_edited = is_line_comment_mode
                                        && app.editing_comment_id.as_ref() == Some(&comment.id);

                                    if is_being_edited {
                                        let line_range = app
                                            .comment_line_range
                                            .map(|(r, _)| r)
                                            .or_else(|| Some(LineRange::single(old_ln)));
                                        let (input_lines, cursor_info) =
                                            comment_panel::format_comment_input_lines(
                                                &app.theme,
                                                comment_type_presentation(app, &app.comment_type),
                                                &app.comment_buffer,
                                                app.comment_cursor,
                                                line_range,
                                                true,
                                                comment_width,
                                                app.comment_vim_mode_label()
                                                    .as_ref()
                                                    .map(|(t, w)| (t.as_str(), *w)),
                                                app.supports_keyboard_enhancement,
                                            );
                                        comment_cursor_logical_line =
                                            Some(line_idx + cursor_info.line_offset);
                                        comment_cursor_column = 1 + cursor_info.column;
                                        let box_top_row = line_idx;
                                        comment_input_box_range = Some((
                                            line_idx,
                                            line_idx + input_lines.len().saturating_sub(1),
                                        ));
                                        let annotations_replaced = App::comment_display_lines(
                                            comment,
                                            inner.width as usize,
                                        );
                                        app.comment_input_annotation_offset = Some((
                                            line_idx,
                                            input_lines.len(),
                                            annotations_replaced,
                                        ));

                                        for mut input_line in input_lines {
                                            let indicator =
                                                cursor_indicator(line_idx, current_line_idx);
                                            input_line.spans.insert(
                                                0,
                                                Span::styled(
                                                    indicator,
                                                    styles::current_line_indicator_style(
                                                        &app.theme,
                                                    ),
                                                ),
                                            );
                                            lines.push(input_line);
                                            line_idx += 1;
                                        }
                                        push_comment_bar(
                                            &mut comment_bars,
                                            box_top_row,
                                            line_range,
                                        );
                                    } else {
                                        let line_range = comment
                                            .line_range
                                            .or_else(|| Some(LineRange::single(old_ln)));
                                        let box_top_row = line_idx;
                                        let rows = App::comment_display_lines(
                                            comment,
                                            inner.width as usize,
                                        );
                                        // The bar is recorded either way: it is
                                        // painted above the box, so it can be on
                                        // screen while the box itself is not.
                                        if !comment_box_visible(
                                            line_idx,
                                            rows,
                                            (visible_start, visible_end),
                                        ) {
                                            skip_comment_box(&mut lines, &mut line_idx, rows);
                                        } else {
                                            let comment_lines = comment_panel::format_comment_lines(
                                                &app.theme,
                                                comment_type_presentation(
                                                    app,
                                                    &comment.comment_type,
                                                ),
                                                &comment.content,
                                                line_range,
                                                comment_width,
                                                (comment.author != app.username)
                                                    .then_some(comment.author.as_str()),
                                            );
                                            for mut comment_line in comment_lines {
                                                let is_current = line_idx == current_line_idx;
                                                let indicator =
                                                    if is_current { "▶" } else { " " };
                                                comment_line.spans.insert(
                                                    0,
                                                    Span::styled(
                                                        indicator,
                                                        styles::current_line_indicator_style(
                                                            &app.theme,
                                                        ),
                                                    ),
                                                );
                                                lines.push(comment_line);
                                                line_idx += 1;
                                            }
                                        }
                                        push_comment_bar(
                                            &mut comment_bars,
                                            box_top_row,
                                            line_range,
                                        );
                                    }
                                }
                            }
                        }

                        // Render inline input for new line comment (old side)
                        if is_line_comment_mode && app.editing_comment_id.is_none() {
                            let line_range = app
                                .comment_line_range
                                .map(|(r, _)| r)
                                .or_else(|| Some(LineRange::single(old_ln)));
                            let (input_lines, cursor_info) =
                                comment_panel::format_comment_input_lines(
                                    &app.theme,
                                    comment_type_presentation(app, &app.comment_type),
                                    &app.comment_buffer,
                                    app.comment_cursor,
                                    line_range,
                                    false,
                                    comment_width,
                                    app.comment_vim_mode_label()
                                        .as_ref()
                                        .map(|(t, w)| (t.as_str(), *w)),
                                    app.supports_keyboard_enhancement,
                                );
                            comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
                            comment_cursor_column = 1 + cursor_info.column;
                            let box_top_row = line_idx;
                            comment_input_box_range =
                                Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
                            app.comment_input_annotation_offset =
                                Some((line_idx, input_lines.len(), 0));

                            for mut input_line in input_lines {
                                let indicator = cursor_indicator(line_idx, current_line_idx);
                                input_line.spans.insert(
                                    0,
                                    Span::styled(
                                        indicator,
                                        styles::current_line_indicator_style(&app.theme),
                                    ),
                                );
                                lines.push(input_line);
                                line_idx += 1;
                            }
                            push_comment_bar(&mut comment_bars, box_top_row, line_range);
                        }
                    }

                    // New side comments (for added/context lines)
                    if let Some(new_ln) = diff_line.new_lineno {
                        // Check if we're adding/editing a comment on this line (new side)
                        let is_line_comment_mode = app.input_mode == InputMode::Comment
                            && !app.comment_is_file_level
                            && file_idx == app.diff_state.current_file_idx
                            && app.comment_line == Some((new_ln, LineSide::New));

                        if let Some(comments) = line_comments.get(&new_ln) {
                            for comment in comments {
                                if comment.side != Some(LineSide::Old)
                                    && app.comment_visible(comment)
                                {
                                    // Skip if this comment is being edited
                                    let is_being_edited = is_line_comment_mode
                                        && app.editing_comment_id.as_ref() == Some(&comment.id);

                                    if is_being_edited {
                                        let line_range = app
                                            .comment_line_range
                                            .map(|(r, _)| r)
                                            .or_else(|| Some(LineRange::single(new_ln)));
                                        let (input_lines, cursor_info) =
                                            comment_panel::format_comment_input_lines(
                                                &app.theme,
                                                comment_type_presentation(app, &app.comment_type),
                                                &app.comment_buffer,
                                                app.comment_cursor,
                                                line_range,
                                                true,
                                                comment_width,
                                                app.comment_vim_mode_label()
                                                    .as_ref()
                                                    .map(|(t, w)| (t.as_str(), *w)),
                                                app.supports_keyboard_enhancement,
                                            );
                                        comment_cursor_logical_line =
                                            Some(line_idx + cursor_info.line_offset);
                                        comment_cursor_column = 1 + cursor_info.column;
                                        let box_top_row = line_idx;
                                        comment_input_box_range = Some((
                                            line_idx,
                                            line_idx + input_lines.len().saturating_sub(1),
                                        ));
                                        let annotations_replaced = App::comment_display_lines(
                                            comment,
                                            inner.width as usize,
                                        );
                                        app.comment_input_annotation_offset = Some((
                                            line_idx,
                                            input_lines.len(),
                                            annotations_replaced,
                                        ));

                                        for mut input_line in input_lines {
                                            let indicator =
                                                cursor_indicator(line_idx, current_line_idx);
                                            input_line.spans.insert(
                                                0,
                                                Span::styled(
                                                    indicator,
                                                    styles::current_line_indicator_style(
                                                        &app.theme,
                                                    ),
                                                ),
                                            );
                                            lines.push(input_line);
                                            line_idx += 1;
                                        }
                                        push_comment_bar(
                                            &mut comment_bars,
                                            box_top_row,
                                            line_range,
                                        );
                                    } else {
                                        let line_range = comment
                                            .line_range
                                            .or_else(|| Some(LineRange::single(new_ln)));
                                        let box_top_row = line_idx;
                                        let rows = App::comment_display_lines(
                                            comment,
                                            inner.width as usize,
                                        );
                                        // The bar is recorded either way: it is
                                        // painted above the box, so it can be on
                                        // screen while the box itself is not.
                                        if !comment_box_visible(
                                            line_idx,
                                            rows,
                                            (visible_start, visible_end),
                                        ) {
                                            skip_comment_box(&mut lines, &mut line_idx, rows);
                                        } else {
                                            let comment_lines = comment_panel::format_comment_lines(
                                                &app.theme,
                                                comment_type_presentation(
                                                    app,
                                                    &comment.comment_type,
                                                ),
                                                &comment.content,
                                                line_range,
                                                comment_width,
                                                (comment.author != app.username)
                                                    .then_some(comment.author.as_str()),
                                            );
                                            for mut comment_line in comment_lines {
                                                let indicator =
                                                    cursor_indicator(line_idx, current_line_idx);
                                                comment_line.spans.insert(
                                                    0,
                                                    Span::styled(
                                                        indicator,
                                                        styles::current_line_indicator_style(
                                                            &app.theme,
                                                        ),
                                                    ),
                                                );
                                                lines.push(comment_line);
                                                line_idx += 1;
                                            }
                                        }
                                        push_comment_bar(
                                            &mut comment_bars,
                                            box_top_row,
                                            line_range,
                                        );
                                    }
                                }
                            }
                        }

                        // Render inline input for new line comment (new side)
                        if is_line_comment_mode && app.editing_comment_id.is_none() {
                            let line_range = app
                                .comment_line_range
                                .map(|(r, _)| r)
                                .or_else(|| Some(LineRange::single(new_ln)));
                            let (input_lines, cursor_info) =
                                comment_panel::format_comment_input_lines(
                                    &app.theme,
                                    comment_type_presentation(app, &app.comment_type),
                                    &app.comment_buffer,
                                    app.comment_cursor,
                                    line_range,
                                    false,
                                    comment_width,
                                    app.comment_vim_mode_label()
                                        .as_ref()
                                        .map(|(t, w)| (t.as_str(), *w)),
                                    app.supports_keyboard_enhancement,
                                );
                            comment_cursor_logical_line = Some(line_idx + cursor_info.line_offset);
                            comment_cursor_column = 1 + cursor_info.column;
                            let box_top_row = line_idx;
                            comment_input_box_range =
                                Some((line_idx, line_idx + input_lines.len().saturating_sub(1)));
                            app.comment_input_annotation_offset =
                                Some((line_idx, input_lines.len(), 0));

                            for mut input_line in input_lines {
                                let indicator = cursor_indicator(line_idx, current_line_idx);
                                input_line.spans.insert(
                                    0,
                                    Span::styled(
                                        indicator,
                                        styles::current_line_indicator_style(&app.theme),
                                    ),
                                );
                                lines.push(input_line);
                                line_idx += 1;
                            }
                            push_comment_bar(&mut comment_bars, box_top_row, line_range);
                        }
                    }
                }
            }
        }

        // End-of-file gap (after all hunks, not for deleted files)
        if file.status != FileStatus::Deleted
            && matches!(
                app.diff_source,
                DiffSource::WorkingTree
                    | DiffSource::Unstaged
                    | DiffSource::StagedAndUnstaged
                    | DiffSource::StagedUnstagedAndCommits(_)
                    | DiffSource::CommitRange(_)
            )
            && let Some(last_hunk) = file.hunks.last()
        {
            let eof_start = last_hunk.new_start + last_hunk.new_count;
            if let Some(&total) = app.file_line_count_cache.get(&file_idx)
                && eof_start <= total
            {
                let gap = (total - eof_start + 1) as usize;
                let eof_gap_id = GapId {
                    file_idx,
                    hunk_idx: file.hunks.len(),
                };
                let top_lines = app.expanded_top.get(&eof_gap_id);
                let bot_lines = app.expanded_bottom.get(&eof_gap_id);
                let top_len = top_lines.map_or(0, |v| v.len());
                let bot_len = bot_lines.map_or(0, |v| v.len());
                let remaining = gap.saturating_sub(top_len + bot_len);

                // Render top expanded lines (↓ direction)
                if let Some(top) = top_lines {
                    for expanded_line in top {
                        let line_search = app
                            .search_paint_at(line_idx)
                            .map(|needle| (needle, search_style));
                        render_expanded_context_line(
                            &mut lines,
                            &mut line_idx,
                            current_line_idx,
                            expanded_line,
                            &app.theme,
                            lw,
                            app.relative_line_numbers,
                            line_search,
                        );
                    }
                }

                // Expander / hidden lines
                if remaining > 0 {
                    render_expander_line(
                        &mut lines,
                        &mut line_idx,
                        current_line_idx,
                        ExpandDirection::Down,
                        remaining,
                        &app.theme,
                    );
                    if remaining > GAP_EXPAND_BATCH {
                        render_hidden_lines(
                            &mut lines,
                            &mut line_idx,
                            current_line_idx,
                            remaining,
                            &app.theme,
                        );
                    }
                }

                // Render bottom expanded lines
                if let Some(bot) = bot_lines {
                    for expanded_line in bot {
                        let line_search = app
                            .search_paint_at(line_idx)
                            .map(|needle| (needle, search_style));
                        render_expanded_context_line(
                            &mut lines,
                            &mut line_idx,
                            current_line_idx,
                            expanded_line,
                            &app.theme,
                            lw,
                            app.relative_line_numbers,
                            line_search,
                        );
                    }
                }
            }
        }

        // Inter-file spacing. In single-file view, the row doubles as a
        // hint pointing at whichever file `j` would walk into next, so
        // the user always knows what's on the other side of the edge.
        // Falls back to a plain blank on the last file (or in multi-file
        // mode) where the indicator is already pulling its weight.
        let indicator = cursor_indicator(line_idx, current_line_idx);
        let next_hint_path = if app.is_single_file_view {
            app.diff_files
                .get(app.diff_state.current_file_idx + 1)
                .map(|f| f.display_path().display().to_string())
        } else {
            None
        };
        if let Some(next_path) = next_hint_path {
            lines.push(Line::from(vec![
                Span::styled(indicator, styles::current_line_indicator_style(&app.theme)),
                Span::styled(
                    crate::ui::diff_view::spacing_next_file_hint_text(&next_path),
                    Style::default()
                        .fg(app.theme.fg_secondary)
                        .add_modifier(Modifier::DIM),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                indicator,
                styles::current_line_indicator_style(&app.theme),
            )));
        }
        line_idx += 1;
    }

    // Auto-scroll so the comment input box stays visible while the user types.
    // Without this, adding a comment near the bottom/top of the viewport would
    // place the input box off-screen and the user couldn't see what they type.
    scroll_comment_input_into_view(
        &mut app.diff_state.scroll_offset,
        comment_input_box_range,
        comment_cursor_logical_line,
        inner.height as usize,
        lines.len(),
    );

    let visible_lines_unscrolled: Vec<Line> = lines
        .into_iter()
        .skip(app.diff_state.scroll_offset)
        .take(inner.height as usize)
        .collect();

    // Calculate the width of each line for max_content_width and visible line count
    let line_widths: Vec<usize> = visible_lines_unscrolled
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.width())
                .sum::<usize>()
        })
        .collect();

    let max_content_width = line_widths.iter().copied().max().unwrap_or(0);

    app.sync_viewport_width(inner.width as usize);
    app.diff_state.max_content_width = max_content_width;

    let scroll_offset = app.diff_state.scroll_offset;
    let wrap = app.diff_state.wrap_lines;
    let viewport_width = inner.width as usize;
    let visible_lines_unscrolled_for_bg = visible_lines_unscrolled.clone();
    // Single pass: wrap each logical line once, producing both the visual
    // rows to render and the per-line height used by every row-mapping
    // consumer below, so the two can't disagree.
    let (row_heights, wrapped_lines): (Vec<usize>, Option<Vec<Line>>) =
        if wrap && viewport_width > 0 {
            let mut heights = Vec::with_capacity(visible_lines_unscrolled_for_bg.len());
            let mut out: Vec<Line> = Vec::new();
            for line in &visible_lines_unscrolled_for_bg {
                let rows = crate::ui::text_utils::wrap_spans(&line.spans, viewport_width);
                heights.push(rows.len());
                out.extend(rows.into_iter().map(Line::from));
            }
            (heights, Some(out))
        } else {
            (vec![1; visible_lines_unscrolled_for_bg.len()], None)
        };
    app.diff_state.visible_line_count = populate_row_to_annotation(
        &mut app.diff_row_to_annotation,
        &row_heights,
        viewport_width,
        inner.height as usize,
        wrap,
        scroll_offset,
    );

    let max_scroll_x = max_content_width.saturating_sub(viewport_width);
    if app.diff_state.scroll_x > max_scroll_x {
        app.diff_state.scroll_x = max_scroll_x;
    }
    if app.diff_state.wrap_lines {
        app.diff_state.scroll_x = 0;
    }

    let scroll_x = app.diff_state.scroll_x;
    let visible_lines: Vec<Line> = match wrapped_lines {
        Some(out) => out,
        None => visible_lines_unscrolled
            .into_iter()
            .map(|line| apply_horizontal_scroll(line, scroll_x))
            .collect(),
    };

    // Paint per-visual-row add/del backgrounds across full row width.
    paint_unified_diff_rows_with(
        frame,
        inner,
        &visible_lines_unscrolled_for_bg,
        &row_heights,
        |_idx, line| unified_line_bg_style(line, &app.theme),
    );

    let overlay_ctx = crate::ui::diff_view::DiffOverlayPaint {
        inner,
        visible_lines_unscrolled: &visible_lines_unscrolled_for_bg,
        line_widths: &line_widths,
        row_heights: &row_heights,
        wrap_lines: app.diff_state.wrap_lines,
        viewport_width: inner.width as usize,
        scroll_x,
        scroll_offset: app.diff_state.scroll_offset,
        theme: &app.theme,
        comment_bars: &comment_bars,
    };

    // Section-marker row tint (hunk headers + expand/hidden stubs). Painted
    // before the paragraph so cursor-line and selection overlays still win
    // on the active row.
    crate::ui::diff_view::paint_section_highlight(frame, &overlay_ctx);

    // Keep paragraph bg unset so pre-painted per-row diff backgrounds remain visible.
    let diff = Paragraph::new(visible_lines).style(Style::default().fg(app.theme.fg_primary));
    frame.render_widget(diff, inner);

    // Cursor-line bg has to land after the paragraph: spans on +/- lines carry
    // explicit diff_add_bg/diff_del_bg that would mask a pre-paint over the code.
    paint_cursor_line_highlight(
        frame,
        inner,
        &visible_lines_unscrolled_for_bg,
        &row_heights,
        app,
    );

    if let Some(sel) = app.visual_selection {
        paint_visual_selection_overlay(frame, inner, app, sel, &app.theme);
    }

    // File-section header rules extended to the full viewport width.
    crate::ui::diff_view::paint_file_header_fill(frame, &overlay_ctx);

    // Comment-box overlays painted last so the box + bar always win on their
    // single cells regardless of cursor-line / selection underlays.
    crate::ui::diff_view::paint_comment_box_bar(frame, &overlay_ctx);
    crate::ui::diff_view::paint_comment_box_right_border(frame, &overlay_ctx);

    // Calculate screen position for comment cursor if in Comment mode
    if let Some(cursor_logical_line) = comment_cursor_logical_line {
        let scroll_offset = app.diff_state.scroll_offset;
        // Use visible_line_count which accounts for line wrapping
        let visible_lines_count = app.diff_state.visible_line_count.max(1);

        // Check if the cursor line is visible (after scrolling)
        if cursor_logical_line >= scroll_offset
            && cursor_logical_line < scroll_offset + visible_lines_count
        {
            // Calculate screen row - need to account for wrapping
            let logical_offset = cursor_logical_line - scroll_offset;

            // Calculate visual row by summing wrapped line heights
            let mut visual_row: u16 = 0;
            let viewport_width = inner.width as usize;

            if app.diff_state.wrap_lines && viewport_width > 0 {
                // Sum the word-wrap-accurate heights of the lines before the
                // cursor so the terminal cursor lands on the right visual row.
                for i in 0..logical_offset {
                    visual_row += row_heights.get(i).copied().unwrap_or(1) as u16;
                }
            } else {
                visual_row = logical_offset as u16;
            }

            // Account for diff area position (inner starts at diff block's inner area)
            let screen_col = inner.x + comment_cursor_column;
            let screen_row_abs = inner.y + visual_row;

            app.comment_cursor_screen_pos = Some((screen_col, screen_row_abs));
        }
    }
}

/// Render a single expanded context line (shared by unified + side-by-side via unified path)
#[allow(clippy::too_many_arguments)]
fn render_expanded_context_line(
    lines: &mut Vec<Line<'_>>,
    line_idx: &mut usize,
    current_line_idx: usize,
    expanded_line: &crate::model::DiffLine,
    theme: &Theme,
    lw: usize,
    relative_line_numbers: bool,
    search: Option<(&str, Style)>,
) {
    let indicator = cursor_indicator(*line_idx, current_line_idx);
    let line_num = if relative_line_numbers {
        crate::ui::diff_view::relative_line_number_field(
            expanded_line.new_lineno,
            *line_idx,
            current_line_idx,
            lw,
        )
    } else {
        crate::ui::diff_view::expanded_context_lineno_field(expanded_line, lw)
    };
    let mut line_spans = vec![
        Span::styled(indicator, styles::current_line_indicator_style(theme)),
        Span::styled(line_num, styles::expanded_context_style(theme)),
        Span::styled("  ", styles::expanded_context_style(theme)),
    ];
    let content_start = line_spans.len();
    line_spans.push(Span::styled(
        expanded_line.content.clone(),
        styles::expanded_context_style(theme),
    ));
    if let Some((needle, hl)) = search {
        let content_spans = line_spans.split_off(content_start);
        line_spans.extend(crate::ui::text_utils::apply_search_highlight_spans(
            content_spans,
            needle,
            hl,
        ));
    }
    lines.push(Line::from(line_spans));
    *line_idx += 1;
}
