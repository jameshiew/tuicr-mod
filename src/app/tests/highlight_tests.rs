//! Viewport-driven highlighting as seen through real frames: only the part of
//! the diff near the viewport gets spans, a jump into a huge hunk stays
//! responsive and fills in over later frames, and a reload starts the new
//! lines from scratch.

use crate::app::*;
use crate::model::{DiffFile, DiffHunk, DiffLine, FileStatus, LineOrigin};
use crate::vcs::traits::{VcsBackend, VcsInfo, VcsType};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;

struct StubVcs(VcsInfo);
impl VcsBackend for StubVcs {
    fn info(&self) -> &VcsInfo {
        &self.0
    }
    fn get_working_tree_diff(&self) -> crate::error::Result<Vec<DiffFile>> {
        Ok(Vec::new())
    }
    fn fetch_context_lines(
        &self,
        _path: &std::path::Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
        _start: u32,
        _end: u32,
    ) -> crate::error::Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }
    fn file_line_count(
        &self,
        _path: &std::path::Path,
        _status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> crate::error::Result<u32> {
        Ok(0)
    }
}

fn rust_file(path: &str, lines: usize) -> DiffFile {
    let lines: Vec<DiffLine> = (0..lines)
        .map(|i| DiffLine {
            origin: LineOrigin::Addition,
            content: format!("    let value_{i} = compute(input, {i}); // line"),
            old_lineno: None,
            new_lineno: Some(i as u32 + 1),
            highlighted_spans: None,
        })
        .collect();
    let n = lines.len() as u32;
    let hunks = vec![DiffHunk {
        header: format!("@@ -0,0 +1,{n} @@"),
        lines,
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: n,
        highlight: Default::default(),
    }];
    let content_hash = DiffFile::compute_content_hash(&hunks);
    DiffFile {
        old_path: None,
        new_path: Some(PathBuf::from(path)),
        status: FileStatus::Added,
        hunks,
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash,
        whole_file_text: None,
    }
}

fn app_with(files: Vec<DiffFile>) -> App {
    let vcs_info = VcsInfo {
        root_path: PathBuf::from("/tmp"),
        head_commit: "head".into(),
        branch_name: Some("main".into()),
        vcs_type: VcsType::Git,
    };
    let session = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );
    let mut app = App::build(
        Box::new(StubVcs(vcs_info.clone())),
        vcs_info,
        crate::theme::Theme::dark(),
        None,
        false,
        files,
        session,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
    )
    .expect("build app");
    if app.is_single_file_view {
        app.toggle_single_file_view();
    }
    app
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .expect("draw frame");
}

fn highlighted_count(file: &DiffFile) -> usize {
    file.hunks[0]
        .lines
        .iter()
        .filter(|l| l.highlighted_spans.is_some())
        .count()
}

#[test]
fn should_highlight_only_rows_near_the_viewport() {
    let mut app = app_with(vec![rust_file("a.rs", 5_000), rust_file("b.rs", 50)]);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();

    draw(&mut app, &mut terminal);

    let first = highlighted_count(&app.diff_files[0]);
    assert!(
        first > 0,
        "visible rows must be highlighted after one frame"
    );
    // A hunk is highlighted a window at a time rather than a row at a time,
    // so the frame pays for one window, not for the 5,000-line hunk.
    assert!(
        first < 2_000,
        "one frame should not highlight the whole hunk, got {first}"
    );
    assert_eq!(
        highlighted_count(&app.diff_files[1]),
        0,
        "a file below the fold is untouched"
    );
    assert!(!app.highlight_pending);
}

#[test]
fn jump_into_a_huge_hunk_should_stay_bounded_and_fill_in_over_frames() {
    let mut app = app_with(vec![rust_file("a.rs", 20_000)]);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    draw(&mut app, &mut terminal);

    app.jump_to_bottom();
    draw(&mut app, &mut terminal);

    let after_jump = highlighted_count(&app.diff_files[0]);
    assert!(
        after_jump < 3_000,
        "one frame must not highlight the whole hunk, got {after_jump}"
    );
    assert!(
        app.highlight_pending,
        "the frame must ask for another one to keep going"
    );

    let mut frames = 0;
    while app.highlight_pending && frames < 100 {
        draw(&mut app, &mut terminal);
        frames += 1;
    }
    assert!(!app.highlight_pending, "highlighting must finish");
    let file = &app.diff_files[0];
    assert!(file.hunks[0].highlight.is_complete());
    assert!(
        file.hunks[0]
            .lines
            .last()
            .unwrap()
            .highlighted_spans
            .is_some(),
        "the row under the cursor ends up highlighted"
    );
}

#[test]
fn reloaded_files_should_start_unhighlighted_and_catch_up() {
    let mut app = app_with(vec![rust_file("a.rs", 100)]);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    draw(&mut app, &mut terminal);
    assert!(highlighted_count(&app.diff_files[0]) > 0);

    app.diff_files = vec![rust_file("a.rs", 120)];
    app.rebuild_annotations();
    assert_eq!(highlighted_count(&app.diff_files[0]), 0);

    draw(&mut app, &mut terminal);
    assert!(highlighted_count(&app.diff_files[0]) > 0);
}
