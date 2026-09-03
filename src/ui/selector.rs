use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::ui::commit_row::{CURSOR_GLYPH, CommitRowSpec, render_commit_row};
use crate::ui::status_bar;
use crate::ui::styles;

const TAB_LOCAL: &str = "Local";

pub(super) fn render_commit_select(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Layout: top bar (brand + tab + right-slot status), bordered body,
    // footer. The tab lives INSIDE the top bar — no separate strip row.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top bar (brand + tab + status)
            Constraint::Min(0),    // body block (full borders)
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_top_bar(frame, app, chunks[0]);

    let body_area = chunks[1];
    let body_block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles::border_style(&app.theme, true))
        .style(styles::panel_style(&app.theme));
    let inner = body_block.inner(body_area);
    frame.render_widget(body_block, body_area);

    render_local_target_tab(frame, app, inner);

    render_target_selector_footer(frame, app, chunks[2]);
}

/// Combined top bar: brand on the left, the tab chip, then a right slot
/// carrying `git:<branch>`. The entire row uses `status_bar_bg` so the
/// active tab's `bg_highlight` reads as a chip popping out of the strip.
fn render_top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    let strip_bg = theme.status_bar_bg;
    let strip_style = Style::default().bg(strip_bg).fg(theme.fg_dim);
    let brand_style = Style::default()
        .bg(strip_bg)
        .fg(theme.fg_primary)
        .add_modifier(Modifier::BOLD);
    let active_chip = Style::default()
        .bg(theme.bg_highlight)
        .fg(theme.fg_primary)
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(" tuicr  ", brand_style));
    spans.push(Span::styled(format!(" {TAB_LOCAL} "), active_chip));

    let left_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();

    let vcs_type = &app.vcs_info.vcs_type;
    let branch = app.vcs_info.branch_name.as_deref().unwrap_or("detached");
    let content = format!(" {vcs_type}:{branch} ");
    let right_width = content.chars().count();
    let right_span = Span::styled(content, strip_style);

    let total_width = area.width as usize;
    let pad = total_width.saturating_sub(left_width + right_width);
    spans.push(Span::styled(" ".repeat(pad), strip_style));
    if right_width > 0 {
        spans.push(right_span);
    }

    frame.render_widget(Paragraph::new(Line::from(spans)).style(strip_style), area);
}

fn render_local_target_tab(frame: &mut Frame, app: &mut App, area: Rect) {
    // Update viewport height for scroll calculations
    app.commit_list_viewport_height = area.height as usize;
    app.commit_list_inner_area = Some(area);

    let total_commits = app.commit_list.len();
    let visible_count = app.visible_commit_count.min(total_commits);

    let mut items: Vec<Line> = app
        .commit_list
        .iter()
        .take(visible_count)
        .enumerate()
        .map(|(i, commit)| {
            render_commit_row(&CommitRowSpec {
                commit,
                available_width: area.width,
                is_cursor: i == app.commit_list_cursor,
                is_selected: app.is_commit_selected(i),
                is_reviewed: false,
                theme: &app.theme,
            })
        })
        .collect();

    if app.can_show_more_commits() {
        items.push(overflow_row(
            &app.theme,
            app.commit_list_cursor == visible_count,
            "show more commits",
        ));
    }

    let visible_items: Vec<Line> = items
        .into_iter()
        .skip(app.commit_list_scroll_offset)
        .take(area.height as usize)
        .collect();

    let list = Paragraph::new(visible_items).style(styles::panel_style(&app.theme));
    frame.render_widget(list, area);
}

fn overflow_row<'a>(theme: &crate::theme::Theme, is_cursor: bool, label: &'a str) -> Line<'a> {
    let style = if is_cursor {
        styles::selected_style(theme)
    } else {
        Style::default().fg(theme.fg_dim)
    };
    let pointer = if is_cursor {
        format!("{CURSOR_GLYPH} ")
    } else {
        "  ".to_string()
    };
    Line::from(vec![
        Span::styled(pointer, style),
        Span::styled(format!("    \u{2026} {label}"), style),
    ])
}

fn render_target_selector_footer(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    let mode_span = Span::styled(" SELECT ", styles::mode_style(theme));

    let hints = if app.message.is_some() {
        String::new()
    } else {
        "   j/k navigate \u{00b7} space range \u{00b7} \u{21b5} confirm \u{00b7} q quit".to_string()
    };
    let hints_span = Span::styled(hints, Style::default().fg(theme.fg_secondary));

    let selected_count = match app.commit_selection_range {
        Some((start, end)) => end - start + 1,
        _ => 0,
    };

    let (right_span, right_width) = if app.message.is_some() {
        status_bar::build_message_span(app.message.as_ref(), theme)
    } else if selected_count > 0 {
        let text = format!(" {selected_count} selected ");
        let width = text.len();
        (Span::styled(text, Style::default().fg(theme.fg_dim)), width)
    } else {
        (Span::raw(""), 0)
    };
    let (left_spans, right_span, right_width) = if app.message.is_some() {
        // Render the message next to the mode label so selector hints cannot
        // push it beyond the visible footer.
        (vec![mode_span, right_span], Span::raw(""), 0)
    } else {
        (vec![mode_span, hints_span], right_span, right_width)
    };

    let spans = status_bar::build_right_aligned_spans(
        left_spans,
        right_span,
        right_width,
        area.width as usize,
    );

    let footer = Paragraph::new(Line::from(spans))
        .style(styles::status_bar_style(theme))
        .block(Block::default());
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod selector_render_snapshot_tests {
    //! Render-snapshot tests for the review-target selector. We drive the
    //! real `render` against ratatui's `TestBackend` and assert on the
    //! resulting character grid (plus a few style checks for the active
    //! tab highlight).
    use crate::app::{App, DiffSource, InputMode};
    use crate::error::Result as TuicrResult;
    use crate::error::TuicrError;
    use crate::model::{DiffFile, DiffLine, FileStatus, ReviewSession, SessionDiffSource};
    use crate::syntax::SyntaxHighlighter;
    use crate::theme::Theme;
    use crate::ui::render;
    use crate::vcs::CommitInfo;
    use crate::vcs::traits::{VcsBackend, VcsChangeStatus, VcsInfo, VcsType};
    use chrono::{TimeZone, Utc};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;
    use std::path::{Path, PathBuf};

    struct SnapshotVcs {
        info: VcsInfo,
    }

    impl VcsBackend for SnapshotVcs {
        fn info(&self) -> &VcsInfo {
            &self.info
        }

        fn get_working_tree_diff(
            &self,
            _highlighter: &SyntaxHighlighter,
        ) -> TuicrResult<Vec<DiffFile>> {
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

        fn get_change_status(&self) -> TuicrResult<VcsChangeStatus> {
            Ok(VcsChangeStatus {
                staged: false,
                unstaged: false,
            })
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

    fn commit(i: usize) -> CommitInfo {
        CommitInfo {
            id: format!("abc{i}"),
            short_id: format!("abc{i}"),
            branch_name: None,
            summary: format!("commit {i}"),
            body: None,
            author: "tester".to_string(),
            time: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        }
    }

    fn make_app(commits: Vec<CommitInfo>) -> App {
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("/tmp"),
            head_commit: "head".to_string(),
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
            Box::new(SnapshotVcs {
                info: vcs_info.clone(),
            }),
            vcs_info,
            Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            DiffSource::WorkingTree,
            InputMode::CommitSelect,
            commits,
            None,
        )
        .expect("build app")
    }

    fn draw_at(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn draw(app: &mut App) -> Buffer {
        draw_at(app, 120, 24)
    }

    fn draw_inline_at(app: &mut App, width: u16) -> Buffer {
        let backend = TestBackend::new(width, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::inline_commit_selector::render_inline_commit_selector(frame, app, area);
            })
            .expect("draw inline selector");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    /// True when at least one cell in [x_start, x_end) on row `y` carries
    /// the BOLD modifier — the active-label cue in the new flat design.
    fn any_bold_in_range(buffer: &Buffer, y: u16, x_start: u16, x_end: u16) -> bool {
        (x_start..x_end.min(buffer.area.width))
            .any(|x| buffer[(x, y)].style().add_modifier.contains(Modifier::BOLD))
    }

    /// Locate the inclusive x-range of a substring on row `y`. Panics if
    /// the substring is not present.
    fn locate(buffer: &Buffer, y: u16, needle: &str) -> (u16, u16) {
        let line = row_text(buffer, y);
        let byte_idx = line
            .find(needle)
            .unwrap_or_else(|| panic!("expected to find {needle:?} on row {y}, got {line:?}"));
        let start = byte_idx as u16;
        let end = start + needle.len() as u16;
        (start, end)
    }

    const TAB_STRIP_ROW: u16 = 0;

    #[test]
    fn should_resize_branch_columns_on_fullscreen_and_inline_selectors() {
        // given
        let mut wide_commit = commit(0);
        wide_commit.branch_name = Some("feat/responsive-branch-alpha".to_string());
        let mut fullscreen_app = make_app(vec![wide_commit.clone()]);
        let mut inline_app = make_app(vec![wide_commit.clone()]);
        inline_app.review_commits = vec![wide_commit];

        // when
        let compact_fullscreen = draw_at(&mut fullscreen_app, 100, 24);
        let wide_fullscreen = draw_at(&mut fullscreen_app, 160, 24);
        let compact_inline = draw_inline_at(&mut inline_app, 100);
        let wide_inline = draw_inline_at(&mut inline_app, 160);

        // then
        assert!(!row_text(&compact_fullscreen, 2).contains("branch-alpha]"));
        assert!(row_text(&wide_fullscreen, 2).contains("branch-alpha]"));
        assert!(!row_text(&compact_inline, 1).contains("branch-alpha]"));
        assert!(row_text(&wide_inline, 1).contains("branch-alpha]"));
    }

    /// True when at least one cell in [x_start, x_end) on row `y` carries
    /// the given background color.
    fn any_bg_in_range(
        buffer: &Buffer,
        y: u16,
        x_start: u16,
        x_end: u16,
        bg: ratatui::style::Color,
    ) -> bool {
        (x_start..x_end.min(buffer.area.width)).any(|x| buffer[(x, y)].style().bg == Some(bg))
    }

    #[test]
    fn should_render_local_tab_label_with_active_chip_bg() {
        // given — plain app, Local is the only tab
        let mut app = make_app(vec![commit(0), commit(1)]);
        let highlight_bg = app.theme.bg_highlight;
        // when
        let buffer = draw(&mut app);
        // then — tab strip shows the Local label in the bg-filled row
        let strip = row_text(&buffer, TAB_STRIP_ROW);
        assert!(
            strip.contains("Local"),
            "tab strip missing label: {strip:?}"
        );
        // and — the active "Local" chip carries the highlight bg
        let (lo, hi) = locate(&buffer, TAB_STRIP_ROW, "Local");
        assert!(
            any_bg_in_range(&buffer, TAB_STRIP_ROW, lo, hi, highlight_bg),
            "active Local chip should carry bg_highlight"
        );
        // and the active label is BOLD
        assert!(
            any_bold_in_range(&buffer, TAB_STRIP_ROW, lo, hi),
            "active Local label should be BOLD"
        );
    }

    #[test]
    fn should_render_status_message_in_commit_selector_footer() {
        let mut app = make_app(vec![commit(0), commit(1)]);
        app.set_error("Failed to load commits");

        let buffer = draw(&mut app);

        let footer = row_text(&buffer, buffer.area.height - 1);
        assert!(
            footer.contains("Failed to load commits"),
            "expected status message in selector footer, got: {footer:?}"
        );
    }

    #[test]
    fn should_render_q_quit_hint_in_footer() {
        let mut app = make_app(vec![commit(0)]);
        let buffer = draw(&mut app);
        let footer = row_text(&buffer, buffer.area.height - 1);
        assert!(
            footer.contains("q quit"),
            "expected q quit hint in selector footer, got: {footer:?}"
        );
    }

    #[test]
    fn should_render_full_screen_messages_from_target_selector() {
        let mut app = make_app(vec![commit(0)]);
        app.set_error("forge API failure detail");
        app.enter_command_mode();
        app.command_buffer = "messages".to_string();
        crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

        let buffer = draw(&mut app);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");

        let width = buffer.area.width;
        let height = buffer.area.height;
        assert_eq!(buffer[(0, 0)].symbol(), "┌");
        assert_eq!(buffer[(width - 1, 0)].symbol(), "┐");
        assert_eq!(buffer[(0, height - 1)].symbol(), "└");
        assert_eq!(buffer[(width - 1, height - 1)].symbol(), "┘");
        assert!(rendered.contains("Messages"), "got:\n{rendered}");
        assert!(
            rendered.contains("forge API failure detail"),
            "got:\n{rendered}"
        );
    }

    #[test]
    fn should_keep_target_selector_behind_command_and_help_overlays() {
        let mut app = make_app(vec![commit(0)]);

        app.enter_command_mode();
        app.command_buffer = "mes".to_string();
        let command_buffer = draw(&mut app);
        assert!(row_text(&command_buffer, TAB_STRIP_ROW).contains("Local"));
        assert!(row_text(&command_buffer, command_buffer.area.height - 1).contains(":mes"));
        crate::handler::handle_command_action(&mut app, crate::input::Action::ExitMode);
        assert_eq!(app.input_mode, InputMode::CommitSelect);

        app.toggle_help();
        let help_buffer = draw(&mut app);
        let rendered = (0..help_buffer.area.height)
            .map(|y| row_text(&help_buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(row_text(&help_buffer, TAB_STRIP_ROW).contains("Local"));
        assert!(rendered.contains("Help"), "got:\n{rendered}");
        app.toggle_help();
        assert_eq!(app.input_mode, InputMode::CommitSelect);
    }
}
