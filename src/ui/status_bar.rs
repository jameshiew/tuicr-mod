use std::borrow::Cow;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DiffSource, FocusedPanel, InputMode, Message, MessageType};
use crate::theme::Theme;
use crate::ui::commit_row::CURSOR_GLYPH;
use crate::ui::styles;

/// Maximum visible completion candidates in the command prompt popup.
const COMMAND_COMPLETION_MAX_ROWS: usize = 7;

pub fn build_message_span(message: Option<&Message>, theme: &Theme) -> (Span<'static>, usize) {
    if let Some(msg) = message {
        let (fg, bg) = match msg.message_type {
            MessageType::Info => (theme.message_info_fg, theme.message_info_bg),
            MessageType::Warning => (theme.message_warning_fg, theme.message_warning_bg),
            MessageType::Error => (theme.message_error_fg, theme.message_error_bg),
        };
        let detail = msg.content.replace(['\n', '\r'], " ");
        let content = if msg.message_type == MessageType::Error {
            format!(" [:messages] {detail} ")
        } else {
            format!(" {detail} ")
        };
        let width = content.width();
        (
            Span::styled(
                content,
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            width,
        )
    } else {
        (Span::raw(""), 0)
    }
}

pub fn build_right_aligned_spans<'a>(
    mut left_spans: Vec<Span<'a>>,
    message_span: Span<'a>,
    message_width: usize,
    total_width: usize,
) -> Vec<Span<'a>> {
    let left_width: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let padding_width = total_width.saturating_sub(left_width + message_width);
    let padding = Span::raw(" ".repeat(padding_width));

    left_spans.push(padding);
    if message_width > 0 {
        left_spans.push(message_span);
    }
    left_spans
}

pub fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    let brand = Span::styled(
        " tuicr ",
        Style::default()
            .fg(theme.fg_primary)
            .add_modifier(Modifier::BOLD),
    );

    // Right-cluster: source/context chunks, bullet-separated:
    // `<vcs>:<branch> · <source>`.
    let mut chunks: Vec<String> = Vec::new();
    let vcs_type = &app.vcs_info.vcs_type;
    let branch = app.vcs_info.branch_name.as_deref().unwrap_or("detached");
    chunks.push(format!("{vcs_type}:{branch}"));
    if let Some(source) = header_source_chunk(app) {
        chunks.push(source);
    }
    if app.is_single_file_view {
        chunks.push("FOCUS".to_string());
    }
    if app.is_pristine_mode {
        // The pristine session key has shape `pristine:<head_or_none>:<hash>`,
        // so the middle segment is the short SHA of the HEAD we're reviewing.
        // "none" renders as `uncommitted` so empty repos read sensibly. A
        // missing prefix falls back to `?` rather than crashing the chip.
        let head_label = app
            .vcs_info
            .head_commit
            .strip_prefix("pristine:")
            .and_then(|rest| rest.split(':').next())
            .map(|raw| if raw == "none" { "uncommitted" } else { raw })
            .unwrap_or("?");
        chunks.push(format!(
            "PRISTINE \u{00b7} {} \u{00b7} {} files",
            head_label,
            app.diff_files.len()
        ));
    }
    let source_text = if chunks.is_empty() {
        String::new()
    } else {
        format!(" {} ", chunks.join(" \u{00b7} "))
    };
    let source_width = source_text.chars().count();
    let source_span = Span::styled(source_text, Style::default().fg(theme.fg_secondary));

    let total_width = area.width as usize;
    let brand_width = brand.content.chars().count();
    let right_width = source_width;
    let pad_width = total_width.saturating_sub(brand_width + right_width);

    let spans = vec![brand, Span::raw(" ".repeat(pad_width)), source_span];

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(styles::status_bar_style(theme)),
        area,
    );
}

/// Short form of the HEAD sha, or `None` when there is no real commit to name.
///
/// Pristine sessions store a synthetic `pristine:<head>:<hash>` key rather than
/// a sha, and that mode already renders its own chip further down the header.
/// An empty repository has no HEAD at all.
fn head_commit_label(app: &App) -> Option<String> {
    if app.is_pristine_mode {
        return None;
    }
    let head = app.vcs_info.head_commit.as_str();
    (!head.is_empty()).then(|| head[..7.min(head.len())].to_string())
}

/// Suffix a working-tree label with the commit being diffed against.
fn with_head_commit(label: &str, app: &App) -> String {
    match head_commit_label(app) {
        Some(head) => format!("{label} \u{00b7} commit {head}"),
        None => label.to_string(),
    }
}

/// Short, lowercase description of the active review source, including the
/// commit it is diffed against. Returns `None` only when there is nothing to
/// add beyond `vcs:branch`, which now means an empty repository.
fn header_source_chunk(app: &App) -> Option<String> {
    match &app.diff_source {
        // The working-tree family all diff against HEAD but never named it, so
        // the commit under review was only visible via `-r <sha>`. The
        // commit-range arms below already identify their own revision.
        DiffSource::WorkingTree => head_commit_label(app).map(|head| format!("commit {head}")),
        DiffSource::Staged => Some(with_head_commit("staged", app)),
        DiffSource::Unstaged => Some(with_head_commit("unstaged", app)),
        DiffSource::StagedAndUnstaged => Some(with_head_commit("staged + unstaged", app)),
        DiffSource::CommitRange(commits) => {
            if commits.len() == 1 {
                Some(format!("commit {}", &commits[0][..7.min(commits[0].len())]))
            } else {
                Some(
                    app.commit_selection_summary()
                        .unwrap_or_else(|| format!("{} commits", commits.len())),
                )
            }
        }
        DiffSource::StagedUnstagedAndCommits(commits) => {
            if commits.len() == 1 {
                Some(format!(
                    "staged + unstaged + commit {}",
                    &commits[0][..7.min(commits[0].len())]
                ))
            } else {
                Some(format!("staged + unstaged + {} commits", commits.len()))
            }
        }
    }
}

pub fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    // In command/search mode, show the input on the left (vim-style)
    let left_spans = if matches!(app.input_mode, InputMode::Command | InputMode::Search) {
        let prefix = if app.input_mode == InputMode::Command {
            ":"
        } else {
            "/"
        };
        let buffer = if app.input_mode == InputMode::Command {
            &app.command_buffer
        } else {
            &app.search_buffer
        };
        let command_text = format!("{prefix}{buffer}");
        vec![Span::styled(
            command_text,
            Style::default().fg(theme.fg_primary),
        )]
    } else {
        let mode_str = match app.input_mode {
            InputMode::Normal => {
                if let Some(count) = app.pending_count {
                    format!(" NORMAL {count} ")
                } else {
                    " NORMAL ".to_string()
                }
            }
            InputMode::Command => " COMMAND ".to_string(),
            InputMode::Search => " SEARCH ".to_string(),
            InputMode::Comment => " COMMENT ".to_string(),
            InputMode::Help => " HELP ".to_string(),
            InputMode::MessageDetails => " ERROR ".to_string(),
            InputMode::Summary => " SUMMARY ".to_string(),
            InputMode::Confirm => " CONFIRM ".to_string(),
            InputMode::CommitSelect => " SELECT ".to_string(),
            InputMode::VisualSelect => {
                if let Some((range, _)) = app.visual_selection_line_range() {
                    if range.is_single() {
                        format!(" VISUAL L{} ", range.start)
                    } else {
                        format!(" VISUAL L{}-L{} ", range.start, range.end)
                    }
                } else {
                    " VISUAL ".to_string()
                }
            }
        };

        let mode_span = Span::styled(mode_str, styles::mode_style(theme));

        let hints: Cow<'static, str> = if app.message.is_some() {
            Cow::Borrowed("")
        } else if app.file_tree_prompt_editing() {
            // File-tree prompts are a sub-state of Normal, so the mode chip
            // still reads NORMAL; the hint is what tells the user Enter/Esc
            // are the way out.
            Cow::Borrowed("   \u{21b5} apply \u{00b7} esc cancel")
        } else {
            match app.input_mode {
                InputMode::Normal if app.focused_panel == FocusedPanel::FileList => Cow::Borrowed(
                    "   j/k move \u{00b7} \u{21b5} open \u{00b7} i/e filter \u{00b7} I/E clear \u{00b7} / search \u{00b7} r reviewed",
                ),
                InputMode::Normal => Cow::Borrowed(
                    "   j/k scroll \u{00b7} {/} file \u{00b7} m/M comment \u{00b7} r file \u{00b7} R hunk \u{00b7} c comment \u{00b7} ? help",
                ),
                InputMode::Command => {
                    Cow::Borrowed("   tab complete \u{00b7} \u{21b5} execute \u{00b7} esc cancel")
                }
                InputMode::Search => Cow::Borrowed("   \u{21b5} search \u{00b7} esc cancel"),
                InputMode::Comment => Cow::Borrowed("   ctrl-s save \u{00b7} esc cancel"),
                InputMode::Help => Cow::Borrowed("   / search · n/N match · q/?/esc close"),
                InputMode::MessageDetails => Cow::Borrowed("   j/k scroll · q/esc close"),
                InputMode::Summary => {
                    Cow::Borrowed("   j/k select \u{00b7} \u{21b5} jump \u{00b7} q/esc close")
                }
                InputMode::Confirm => Cow::Borrowed("   y yes \u{00b7} n no"),
                InputMode::CommitSelect => Cow::Borrowed(
                    "   j/k navigate \u{00b7} space select \u{00b7} \u{21b5} confirm \u{00b7} esc back",
                ),
                InputMode::VisualSelect => Cow::Borrowed(
                    "   j/k extend \u{00b7} c/\u{21b5} comment \u{00b7} y yank \u{00b7} esc/V cancel",
                ),
            }
        };
        let hints_span = Span::styled(hints, Style::default().fg(theme.fg_secondary));

        let mut spans = vec![mode_span, hints_span];
        if app.input_mode == InputMode::Normal
            && app.message.is_none()
            && let Some((current, total)) = app.search_match_position()
        {
            spans.push(Span::styled(
                format!("   [{current}/{total}]"),
                Style::default().fg(theme.fg_secondary),
            ));
        }
        spans
    };

    // Right-aligned slot priority: active message > modified indicator.
    let (right_span, right_width) = if app.message.is_some() {
        build_message_span(app.message.as_ref(), theme)
    } else if app.dirty {
        let content = " \u{2022} modified ".to_string();
        let width = content.chars().count();
        (
            Span::styled(content, Style::default().fg(theme.pending)),
            width,
        )
    } else {
        (Span::raw(""), 0)
    };
    let total_width = area.width as usize;
    let spans = build_right_aligned_spans(left_spans, right_span, right_width, total_width);

    let line = Line::from(spans);

    let status = Paragraph::new(line)
        .style(styles::status_bar_style(theme))
        .block(Block::default());

    frame.render_widget(status, area);
}

pub fn render_command_completion_popup(frame: &mut Frame, app: &App, status_area: Rect) {
    if app.input_mode != InputMode::Command {
        return;
    }
    let Some(completion) = app.command_completion.as_ref() else {
        return;
    };
    if completion.matches.is_empty() || status_area.y < 3 || status_area.width < 4 {
        return;
    }

    let available_rows = status_area.y.saturating_sub(2) as usize;
    let visible_rows = completion
        .matches
        .len()
        .min(COMMAND_COMPLETION_MAX_ROWS)
        .min(available_rows);
    if visible_rows == 0 {
        return;
    }

    let selected = completion
        .selected
        .min(completion.matches.len().saturating_sub(1));
    let start = completion_window_start(selected, completion.matches.len(), visible_rows);
    let end = start + visible_rows;
    let visible_matches = &completion.matches[start..end];
    let content_width = visible_matches
        .iter()
        .map(|command| command.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let popup_width = (content_width as u16 + 2).min(status_area.width);
    if popup_width == 0 {
        return;
    }

    let popup_height = visible_rows as u16 + 2;
    let popup_area = Rect {
        x: status_area.x,
        y: status_area.y.saturating_sub(popup_height),
        width: popup_width,
        height: popup_height,
    };

    let rows: Vec<Line<'_>> = visible_matches
        .iter()
        .enumerate()
        .map(|(offset, command)| {
            let idx = start + offset;
            let marker = if idx == selected { CURSOR_GLYPH } else { " " };
            let style = if idx == selected {
                styles::selected_style(&app.theme)
            } else {
                Style::default().fg(app.theme.fg_secondary)
            };
            Line::from(Span::styled(format!("{marker} {command}"), style))
        })
        .collect();

    frame.render_widget(Clear, popup_area);
    frame.render_widget(
        Paragraph::new(rows)
            .style(styles::panel_style(&app.theme))
            .block(Block::default().borders(Borders::ALL)),
        popup_area,
    );
}

fn completion_window_start(selected: usize, match_count: usize, visible_rows: usize) -> usize {
    if visible_rows >= match_count {
        return 0;
    }
    let half = visible_rows / 2;
    selected
        .saturating_sub(half)
        .min(match_count.saturating_sub(visible_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_message(message_type: MessageType) -> Message {
        Message {
            content: "hello".to_string(),
            message_type,
            expires_at: None,
        }
    }

    #[test]
    fn should_style_info_message_using_theme_fields() {
        let theme = Theme::dark();
        let (span, width) = build_message_span(Some(&test_message(MessageType::Info)), &theme);
        assert_eq!(span.style.fg, Some(theme.message_info_fg));
        assert_eq!(span.style.bg, Some(theme.message_info_bg));
        assert_eq!(width, " hello ".len());
    }

    #[test]
    fn should_return_empty_span_when_message_is_none() {
        let theme = Theme::dark();
        let (span, width) = build_message_span(None, &theme);
        assert_eq!(span.content.as_ref(), "");
        assert_eq!(width, 0);
    }

    #[test]
    fn should_center_completion_window_when_possible() {
        assert_eq!(completion_window_start(5, 12, 7), 2);
    }

    #[test]
    fn should_pin_completion_window_to_top_near_start() {
        assert_eq!(completion_window_start(1, 12, 7), 0);
    }

    #[test]
    fn should_pin_completion_window_to_bottom_near_end() {
        assert_eq!(completion_window_start(10, 12, 7), 5);
    }

    #[test]
    fn should_show_all_completion_rows_when_they_fit() {
        assert_eq!(completion_window_start(3, 5, 7), 0);
    }

    #[test]
    fn should_style_warning_message_using_theme_fields() {
        let theme = Theme::dark();
        let (span, _) = build_message_span(Some(&test_message(MessageType::Warning)), &theme);
        assert_eq!(span.style.fg, Some(theme.message_warning_fg));
        assert_eq!(span.style.bg, Some(theme.message_warning_bg));
    }

    #[test]
    fn should_style_error_message_using_theme_fields() {
        let theme = Theme::dark();
        let (span, _) = build_message_span(Some(&test_message(MessageType::Error)), &theme);
        assert_eq!(span.style.fg, Some(theme.message_error_fg));
        assert_eq!(span.style.bg, Some(theme.message_error_bg));
    }
}

#[cfg(test)]
mod header_snapshot_tests {
    //! Render-snapshot coverage for the status bar header.
    //! Drives the full `render_header` against ratatui's `TestBackend`
    //! and asserts on the produced character grid.

    use crate::app::{App, DiffSource, InputMode};
    use crate::error::Result as TuicrResult;
    use crate::error::TuicrError;
    use crate::model::{DiffFile, DiffLine, FileStatus, ReviewSession, SessionDiffSource};
    use crate::theme::Theme;
    use crate::vcs::traits::{VcsBackend, VcsInfo, VcsType};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::path::{Path, PathBuf};

    struct NoopVcs {
        info: VcsInfo,
    }
    impl VcsBackend for NoopVcs {
        fn info(&self) -> &VcsInfo {
            &self.info
        }
        fn get_working_tree_diff(&self) -> TuicrResult<Vec<DiffFile>> {
            Err(TuicrError::NoChanges)
        }
        fn fetch_context_lines(
            &self,
            _file_path: &Path,
            _file_status: FileStatus,
            _ref_commit: Option<&str>,
            _start_line: u32,
            _end_line: u32,
        ) -> TuicrResult<Vec<DiffLine>> {
            Ok(Vec::new())
        }
        fn file_line_count(
            &self,
            _file_path: &Path,
            _file_status: FileStatus,
            _ref_commit: Option<&str>,
        ) -> TuicrResult<u32> {
            Ok(0)
        }
    }

    fn build_local_app(diff_source: DiffSource, head_commit: &str) -> App {
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("/repo"),
            head_commit: head_commit.to_string(),
            branch_name: Some("main".to_string()),
            vcs_type: VcsType::Git,
        };
        let session = ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            SessionDiffSource::WorkingTree,
        );
        App::build(
            Box::new(NoopVcs {
                info: vcs_info.clone(),
            }),
            vcs_info,
            Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            diff_source,
            InputMode::Normal,
            Vec::new(),
            None,
        )
        .expect("build local app")
    }

    fn draw_header(app: &App) -> Buffer {
        let backend = TestBackend::new(140, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::render_header(frame, app, area);
            })
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn draw_app(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn should_make_full_long_error_accessible_from_status_bar() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.set_error(
            "Submit failed: GitHub command failed: gh: Unprocessable Entity (HTTP 422)\n\
             {\"message\":\"Unprocessable Entity\",\"errors\":[\"Review Can not approve your own pull request\"]}",
        );

        let status = draw_app(&mut app, 60, 12);
        assert!(
            row_text(&status, 11).contains("[:messages]"),
            "status bar should advertise full error details"
        );

        app.open_message_details();
        let buffer = draw_app(&mut app, 60, 12);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        let compact = rendered
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        assert!(
            compact.contains("Submit failed: GitHub command failed: gh: Unprocessable Entity"),
            "error prefix should remain visible, got:\n{rendered}"
        );

        app.help_scroll_to_bottom();
        let buffer = draw_app(&mut app, 60, 12);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        let compact = rendered
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            compact.contains("Review Can not approve your own pull request"),
            "full error detail should remain reachable, got:\n{rendered}"
        );
    }

    #[test]
    fn should_scroll_to_end_of_oversized_error_details() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.set_error(format!(
            "Submit failed: {}UNIQUE_END",
            "long error ".repeat(200)
        ));
        app.open_message_details();

        let _ = draw_app(&mut app, 60, 12);
        app.help_scroll_to_bottom();
        let buffer = draw_app(&mut app, 60, 12);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("UNIQUE_END"),
            "last error detail should be reachable, got:\n{rendered}"
        );
    }

    #[test]
    fn should_scroll_error_details_with_mouse_wheel() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.set_error(format!("failed: {}", "long error ".repeat(200)));
        app.open_message_details();
        let _ = draw_app(&mut app, 60, 12);

        crate::handler::handle_mouse_event(
            &mut app,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );

        assert!(app.help_state.scroll_offset > 0);
    }

    #[test]
    fn should_open_error_details_with_messages_command_and_close_to_normal_mode() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.input_mode = InputMode::Command;
        app.command_buffer = "messages".to_string();
        app.set_error("failed to load target");

        crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);
        assert_eq!(app.input_mode, InputMode::MessageDetails);
        app.toggle_help();

        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn should_reset_scroll_when_another_error_replaces_the_open_message() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.set_error(format!("failed: {}", "long error ".repeat(200)));
        app.open_message_details();
        let _ = draw_app(&mut app, 60, 12);
        app.help_scroll_to_bottom();
        assert!(app.help_state.scroll_offset > 0);

        app.set_error("replacement error");

        assert_eq!(app.input_mode, InputMode::MessageDetails);
        assert_eq!(app.help_state.scroll_offset, 0);
    }

    #[test]
    fn should_report_when_messages_command_has_no_current_error() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.input_mode = InputMode::Command;
        app.command_buffer = "messages".to_string();

        crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(
            app.message.as_ref().map(|message| message.content.as_str()),
            Some("No current error")
        );
    }

    #[test]
    fn should_close_error_details_when_a_non_error_replaces_the_message() {
        let mut app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        app.set_error("request failed");
        app.open_message_details();

        app.set_message("request recovered");

        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn should_show_head_commit_when_reviewing_the_working_tree() {
        let app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        assert_eq!(
            super::header_source_chunk(&app),
            Some("commit abcdef0".to_string())
        );
    }

    #[test]
    fn should_show_head_commit_alongside_staged_labels() {
        let staged = build_local_app(DiffSource::Staged, "abcdef0123456789");
        assert_eq!(
            super::header_source_chunk(&staged),
            Some("staged \u{00b7} commit abcdef0".to_string())
        );

        let both = build_local_app(DiffSource::StagedAndUnstaged, "abcdef0123456789");
        assert_eq!(
            super::header_source_chunk(&both),
            Some("staged + unstaged \u{00b7} commit abcdef0".to_string())
        );
    }

    #[test]
    fn should_omit_head_commit_when_repository_has_no_head() {
        let app = build_local_app(DiffSource::WorkingTree, "");
        assert_eq!(super::header_source_chunk(&app), None);

        let staged = build_local_app(DiffSource::Staged, "");
        assert_eq!(
            super::header_source_chunk(&staged),
            Some("staged".to_string())
        );
    }

    #[test]
    fn should_not_duplicate_commit_for_a_revision_review() {
        // `-r <sha>` already names its own revision; HEAD must not be appended.
        let app = build_local_app(
            DiffSource::CommitRange(vec!["fedcba9876543210".to_string()]),
            "abcdef0123456789",
        );
        assert_eq!(
            super::header_source_chunk(&app),
            Some("commit fedcba9".to_string())
        );
    }

    #[test]
    fn should_render_head_commit_in_the_header() {
        let app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        let buffer = draw_header(&app);
        let line = row_text(&buffer, 0);
        assert!(line.contains("commit abcdef0"), "got: {line:?}");
    }
}
