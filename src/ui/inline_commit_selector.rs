use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, FocusedPanel};
use crate::ui::commit_row::{CommitRowSpec, render_commit_header, render_commit_row};
use crate::ui::styles;

pub(super) fn render_inline_commit_selector(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focused_panel == FocusedPanel::CommitSelector;
    let theme = &app.theme;

    let block = Block::default()
        .title(" Commits ")
        .borders(Borders::ALL)
        .style(styles::panel_style(theme))
        .border_style(styles::border_style(theme, focused));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    let header_area = chunks[0];
    let rows_area = chunks[1];

    frame.render_widget(
        Paragraph::new(render_commit_header(inner.width, theme)).style(styles::panel_style(theme)),
        header_area,
    );

    app.commit_list_viewport_height = rows_area.height as usize;
    app.commit_list_inner_area = Some(rows_area);

    // Rows are built in data order (index into newest-first `review_commits`),
    // then reversed for ascending display so the oldest commit sits on top.
    let mut items: Vec<Line> = app
        .review_commits
        .iter()
        .enumerate()
        .map(|(i, commit)| {
            render_commit_row(&CommitRowSpec {
                commit,
                available_width: rows_area.width,
                is_cursor: i == app.commit_list_cursor,
                is_selected: app.is_commit_selected(i),
                is_reviewed: false,
                theme,
            })
        })
        .collect();

    let height = rows_area.height as usize;
    let n = app.review_commits.len();
    if app.commits_ascending() {
        items.reverse();
        // The renderer owns the scroll offset in display space for ascending
        // order (descending order lets commit_select_up/down maintain it):
        // clamp it so the cursor's display row stays visible.
        if n > 0 && height > 0 {
            let cursor_row = app.commit_data_index(app.commit_list_cursor);
            let offset = app.commit_list_scroll_offset;
            let offset = if cursor_row < offset {
                cursor_row
            } else if cursor_row >= offset + height {
                cursor_row + 1 - height
            } else {
                offset
            };
            app.commit_list_scroll_offset = offset.min(n.saturating_sub(1));
        }
    }

    let visible_items: Vec<Line> = items
        .into_iter()
        .skip(app.commit_list_scroll_offset)
        .take(height)
        .collect();

    frame.render_widget(
        Paragraph::new(visible_items).style(styles::panel_style(theme)),
        rows_area,
    );
}
