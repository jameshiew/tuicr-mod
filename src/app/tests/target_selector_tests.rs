use crate::app::*;
use crate::model::FileStatus;
use crate::vcs::traits::{VcsChangeStatus, VcsType};

struct DummyVcs {
    info: VcsInfo,
    commits: Vec<CommitInfo>,
    change_status: VcsChangeStatus,
}

impl VcsBackend for DummyVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, _highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        Err(TuicrError::NoChanges)
    }

    fn fetch_context_lines(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
        _start_line: u32,
        _end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn get_change_status(&self) -> Result<VcsChangeStatus> {
        Ok(self.change_status)
    }

    fn get_recent_commits(&self, offset: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        Ok(self
            .commits
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect())
    }

    fn file_line_count(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> Result<u32> {
        Ok(0)
    }
}

fn build_app() -> App {
    build_app_with_commits(Vec::new())
}

#[test]
fn comment_vim_command_line_q_cancels_w_saves() {
    let mut app = build_app();
    app.comment_vim_enabled = true;

    // `:q` exits the comment box.
    app.enter_review_comment_mode();
    assert_eq!(app.input_mode, InputMode::Comment);
    app.start_comment_vim_command();
    assert!(app.comment_vim_command_active());
    app.comment_vim_command_push('q');
    assert_eq!(
        app.comment_vim_mode_label(),
        Some((":q".to_string(), false))
    );
    app.run_comment_vim_command();
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(!app.comment_vim_command_active());

    // `:w` reaches save_comment; on an empty buffer save rejects it and the
    // box stays open — proving the mapping without touching disk.
    app.enter_review_comment_mode();
    app.start_comment_vim_command();
    app.comment_vim_command_push('w');
    app.run_comment_vim_command();
    assert_eq!(app.input_mode, InputMode::Comment);
    assert!(!app.comment_vim_command_active());
}

#[test]
fn comment_vim_double_enter_saves() {
    let mut app = build_app();
    app.comment_vim_enabled = true;
    app.enter_review_comment_mode();

    // First Enter arms (header would show the hint); second routes to
    // save_comment (empty buffer rejected, box stays open) — double-Enter == :w.
    app.comment_vim_enter_normal();
    assert_eq!(app.comment_vim_pending, CommentVimPending::Save);
    assert_eq!(
        app.comment_vim_mode_label(),
        Some(("Enter again to save".to_string(), false))
    );
    app.comment_vim_enter_normal();
    assert_eq!(app.comment_vim_pending, CommentVimPending::None);
    assert_eq!(app.input_mode, InputMode::Comment);

    // A non-Enter key between the two presses breaks the sequence.
    app.comment_vim_enter_normal();
    app.comment_vim_reset_pending();
    assert_eq!(app.comment_vim_pending, CommentVimPending::None);
}

#[test]
fn comment_vim_double_esc_cancels() {
    let mut app = build_app();
    app.comment_vim_enabled = true;
    app.enter_review_comment_mode();
    assert_eq!(app.input_mode, InputMode::Comment);

    // First Esc arms cancel + header hint; second exits the comment box.
    app.comment_vim_esc_normal();
    assert_eq!(app.comment_vim_pending, CommentVimPending::Cancel);
    assert_eq!(
        app.comment_vim_mode_label(),
        Some(("Esc/q again to cancel".to_string(), true))
    );
    app.comment_vim_esc_normal();
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(app.comment_vim_pending, CommentVimPending::None);
}

#[test]
fn comment_vim_soft_tab_inserts_configured_spaces() {
    let mut app = build_app();
    app.comment_vim_enabled = true;
    app.comment_tab_width = 2;
    app.enter_review_comment_mode();
    app.ensure_comment_vim_editor(); // Insert mode, empty buffer
    app.comment_vim_insert_soft_tab();
    assert_eq!(app.comment_buffer, "  ");
    assert_eq!(app.comment_cursor, 2);
}

#[test]
fn comment_block_start_finds_first_row_of_comment() {
    let mut app = build_app();
    app.line_annotations = vec![
        AnnotatedLine::ReviewComment { comment_idx: 0 },
        AnnotatedLine::ReviewComment { comment_idx: 1 },
        AnnotatedLine::ReviewComment { comment_idx: 1 },
        AnnotatedLine::ReviewComment { comment_idx: 1 },
        AnnotatedLine::ReviewComment { comment_idx: 2 },
    ];
    assert_eq!(app.comment_block_start(3), 1);
    assert_eq!(app.comment_block_start(1), 1);
    assert_eq!(app.comment_block_start(0), 0);
    assert_eq!(app.comment_block_start(4), 4);
}

#[test]
fn comment_current_line_cursor_targets_the_cursor_line() {
    let mut app = build_app();
    app.comment_buffer = "alpha\nbravo\ncharlie".to_string();
    app.diff_state.viewport_width = 200; // wide => no wrapping
    let block_start = 10;

    // 2nd content line "bravo" (block_start + top-border + 1): start 6, end 11.
    app.diff_state.cursor_line = block_start + 2;
    assert_eq!(app.comment_current_line_cursor(block_start, false), 6);
    assert_eq!(app.comment_current_line_cursor(block_start, true), 11);

    // 1st content line "alpha": start 0, end 5.
    app.diff_state.cursor_line = block_start + 1;
    assert_eq!(app.comment_current_line_cursor(block_start, false), 0);
    assert_eq!(app.comment_current_line_cursor(block_start, true), 5);

    // Top border row maps to the first line.
    app.diff_state.cursor_line = block_start;
    assert_eq!(app.comment_current_line_cursor(block_start, false), 0);

    // Bottom border / beyond maps to the last line "charlie": start 12, end 19.
    app.diff_state.cursor_line = block_start + 99;
    assert_eq!(app.comment_current_line_cursor(block_start, false), 12);
    assert_eq!(app.comment_current_line_cursor(block_start, true), 19);
}

#[test]
fn comment_vim_command_backspace_past_colon_closes() {
    let mut app = build_app();
    app.comment_vim_enabled = true;
    app.enter_review_comment_mode();
    app.start_comment_vim_command();
    app.comment_vim_command_push('q');
    app.comment_vim_command_backspace(); // -> ":"
    assert!(app.comment_vim_command_active());
    app.comment_vim_command_backspace(); // past ':' -> closed
    assert!(!app.comment_vim_command_active());
}

fn build_app_with_commits(commits: Vec<CommitInfo>) -> App {
    build_app_full(commits, None)
}

fn build_app_with_comment_types(configs: Vec<crate::config::CommentTypeConfig>) -> App {
    build_app_full(Vec::new(), Some(configs))
}

fn comment_type_config(id: &str) -> crate::config::CommentTypeConfig {
    crate::config::CommentTypeConfig {
        id: id.to_string(),
        ..Default::default()
    }
}

fn build_app_full(
    commits: Vec<CommitInfo>,
    comment_type_configs: Option<Vec<crate::config::CommentTypeConfig>>,
) -> App {
    build_app_rooted(PathBuf::from("/tmp"), commits, comment_type_configs)
}

/// `build_app_full` with an explicit VCS root, for tests that need the app
/// rooted at a real on-disk checkout.
fn build_app_rooted(
    root_path: PathBuf,
    commits: Vec<CommitInfo>,
    comment_type_configs: Option<Vec<crate::config::CommentTypeConfig>>,
) -> App {
    build_app_rooted_with_status(
        root_path,
        commits,
        comment_type_configs,
        VcsChangeStatus::default(),
    )
}

fn build_app_rooted_with_status(
    root_path: PathBuf,
    commits: Vec<CommitInfo>,
    comment_type_configs: Option<Vec<crate::config::CommentTypeConfig>>,
    change_status: VcsChangeStatus,
) -> App {
    let vcs_info = VcsInfo {
        root_path,
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
        Box::new(DummyVcs {
            info: vcs_info.clone(),
            commits,
            change_status,
        }),
        vcs_info,
        Theme::dark(),
        comment_type_configs,
        false,
        Vec::new(),
        session,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
    )
    .expect("failed to build test app")
}

#[test]
fn default_comment_type_is_none_without_config() {
    let mut app = build_app();
    app.enter_comment_mode(false, None);
    assert_eq!(app.input_mode, InputMode::Comment);
    // Out of the box the only type is None — untyped, no prefix.
    assert!(app.comment_type.is_none());
    assert_eq!(app.comment_type.id(), "none");

    // With a single type there is nothing to cycle to; stays on None.
    app.cycle_comment_type();
    assert!(app.comment_type.is_none());
}

#[test]
fn should_cycle_comment_type_on_tab_action() {
    // Configuring types overrides the None default (first configured type
    // becomes the default) but None stays available, appended to the cycle.
    let mut app = build_app_with_comment_types(vec![
        comment_type_config("note"),
        comment_type_config("suggestion"),
    ]);
    app.enter_comment_mode(false, None);
    assert_eq!(app.input_mode, InputMode::Comment);
    assert_eq!(app.comment_type.id(), "note");

    app.cycle_comment_type();
    assert_eq!(app.comment_type.id(), "suggestion");

    // None is appended and reachable by cycling.
    app.cycle_comment_type();
    assert_eq!(app.comment_type.id(), "none");
    assert!(app.comment_type.is_none());

    // Wraps back around to the first configured type.
    app.cycle_comment_type();
    assert_eq!(app.comment_type.id(), "note");
}

fn dummy_commit(id: &str) -> CommitInfo {
    CommitInfo {
        id: id.to_string(),
        short_id: id.to_string(),
        branch_name: None,
        summary: format!("commit {id}"),
        body: None,
        author: "tester".to_string(),
        time: Utc::now(),
    }
}

#[test]
fn should_quit_on_q_when_only_reviewed_files_dirty() {
    // given: a session dirtied only by a reviewed-file marker (no comments)
    let mut app = build_app();
    let path = PathBuf::from("src/main.rs");
    app.session.add_file(path.clone(), FileStatus::Modified, 0);
    app.session.get_file_mut(&path).unwrap().reviewed = true;
    app.dirty = true;
    assert!(!app.session.has_comments());
    app.command_buffer = "q".to_string();

    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

    // then: `:q` quits even though reviewed-only state is dirty, no `:q!` needed
    assert!(app.should_quit);
    assert!(
        !matches!(
            app.message.as_ref().map(|m| &m.message_type),
            Some(MessageType::Error)
        ),
        "should not surface a no-write error"
    );
}

#[test]
fn should_block_q_when_unsaved_comments_exist() {
    // given: a session with an unsaved comment
    let mut app = build_app();
    let path = PathBuf::from("src/main.rs");
    app.session.add_file(path.clone(), FileStatus::Modified, 0);
    app.session
        .get_file_mut(&path)
        .unwrap()
        .add_file_comment(crate::model::Comment::new(
            "needs work".to_string(),
            crate::model::CommentType::from_id("note"),
            None,
        ));
    app.dirty = true;
    assert!(app.session.has_comments());
    app.command_buffer = "q".to_string();

    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

    // then: the guard still requires `:q!`
    assert!(!app.should_quit);
    assert!(app.dirty);
    assert_eq!(
        app.message.as_ref().map(|m| m.message_type.clone()),
        Some(MessageType::Error)
    );
}

#[test]
fn should_open_local_selector_on_targets_command() {
    // given
    let mut app = build_app_with_commits(vec![dummy_commit("abc")]);
    app.command_buffer = "targets".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);
    // then
    assert_eq!(app.target_tab, TargetTab::Local);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
}

#[test]
fn should_treat_commits_as_alias_for_local_target_selector() {
    // given
    let mut app = build_app_with_commits(vec![dummy_commit("abc")]);
    app.command_buffer = "commits".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);
    // then
    assert_eq!(app.target_tab, TargetTab::Local);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
}

#[test]
fn should_treat_right_as_enter_on_local_commit_expand_row() {
    let commits = vec![dummy_commit("bbb"), dummy_commit("aaa")];
    let mut app = build_app_with_commits(commits);
    app.enter_target_selector(TargetTab::Local).unwrap();
    app.visible_commit_count = 1;
    app.commit_list_cursor = 1;

    crate::handler::handle_commit_select_action(&mut app, crate::input::Action::FocusRight);

    assert_eq!(app.visible_commit_count, 2);
}

#[test]
fn should_focus_diff_on_right_from_file_list() {
    let mut app = build_app();
    app.focused_panel = FocusedPanel::FileList;

    crate::handler::handle_file_list_action(&mut app, crate::input::Action::FocusRight);

    assert_eq!(app.focused_panel, FocusedPanel::Diff);
}

#[test]
fn should_focus_file_list_on_left_from_diff() {
    let mut app = build_app();
    app.show_file_list = false;
    app.focused_panel = FocusedPanel::Diff;

    crate::handler::handle_diff_action(&mut app, crate::input::Action::FocusLeft);

    assert!(app.show_file_list);
    assert_eq!(app.focused_panel, FocusedPanel::FileList);
}

#[test]
fn should_open_local_selector_on_left_from_file_list() {
    let mut app = build_app_with_commits(vec![dummy_commit("abc")]);
    app.focused_panel = FocusedPanel::FileList;

    crate::handler::handle_file_list_action(&mut app, crate::input::Action::FocusLeft);

    assert_eq!(app.target_tab, TargetTab::Local);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
}

#[test]
fn should_open_local_selector_on_escape_from_diff_without_search_highlights() {
    let mut app = build_app_with_commits(vec![dummy_commit("abc")]);

    crate::handler::handle_diff_action(&mut app, crate::input::Action::ClearSearchHighlight);

    assert_eq!(app.target_tab, TargetTab::Local);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
}

#[test]
fn should_highlight_reviewed_commit_on_escape_to_local_selector() {
    let reviewed_commit = dummy_commit("bbb");
    let mut app = build_app_with_commits(vec![
        dummy_commit("ccc"),
        reviewed_commit.clone(),
        dummy_commit("aaa"),
    ]);
    app.diff_source = DiffSource::CommitRange(vec![reviewed_commit.id.clone()]);
    app.review_commits = vec![reviewed_commit];
    app.commit_list = app.review_commits.clone();

    crate::handler::handle_diff_action(&mut app, crate::input::Action::ClearSearchHighlight);

    assert_eq!(app.input_mode, InputMode::CommitSelect);
    assert_eq!(app.commit_list[app.commit_list_cursor].id, "bbb");
}

#[test]
fn should_highlight_staged_changes_when_returning_to_local_selector() {
    let mut app = build_app_rooted_with_status(
        PathBuf::from("/tmp"),
        vec![dummy_commit("abc")],
        None,
        VcsChangeStatus {
            staged: true,
            unstaged: true,
        },
    );
    app.diff_source = DiffSource::Staged;

    app.enter_target_selector(TargetTab::Local).unwrap();

    assert_eq!(
        app.commit_list[app.commit_list_cursor].id,
        STAGED_SELECTION_ID
    );
}

#[test]
fn should_drop_stale_staged_changes_and_highlight_first_local_target() {
    let mut app = build_app_rooted_with_status(
        PathBuf::from("/tmp"),
        vec![dummy_commit("abc")],
        None,
        VcsChangeStatus {
            staged: false,
            unstaged: true,
        },
    );
    app.diff_source = DiffSource::Staged;
    app.commit_list = vec![
        App::staged_commit_entry(),
        App::unstaged_commit_entry(),
        dummy_commit("old"),
    ];

    app.enter_target_selector(TargetTab::Local).unwrap();

    assert!(!app.commit_list.iter().any(App::is_staged_commit));
    assert_eq!(app.commit_list_cursor, 0);
    assert_eq!(app.commit_list[0].id, UNSTAGED_SELECTION_ID);
}

#[test]
fn should_clear_search_highlights_before_escape_opens_local_selector() {
    let mut app = build_app_with_commits(vec![dummy_commit("abc")]);
    app.search_highlight_visible = true;

    crate::handler::handle_diff_action(&mut app, crate::input::Action::ClearSearchHighlight);

    assert!(!app.search_highlight_visible);
    assert_eq!(app.input_mode, InputMode::Normal);

    crate::handler::handle_diff_action(&mut app, crate::input::Action::ClearSearchHighlight);

    assert_eq!(app.target_tab, TargetTab::Local);
    assert_eq!(app.input_mode, InputMode::CommitSelect);
}

#[test]
fn should_open_pending_comment_summary_from_command_mode() {
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "summary".to_string();

    crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

    assert_eq!(app.input_mode, InputMode::Summary);
    assert!(app.command_buffer.is_empty());
    assert_eq!(app.summary_state.selected_comment, 0);
    assert_eq!(app.summary_state.scroll_offset, 0);

    app.summary_state.selected_comment = 5;
    app.summary_state.scroll_offset = 15;
    app.enter_summary_mode();
    assert_eq!(app.summary_state.selected_comment, 0);
    assert_eq!(app.summary_state.scroll_offset, 0);

    crate::handler::handle_summary_action(&mut app, crate::input::Action::ExitMode);
    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn should_complete_summary_command() {
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "summ".to_string();

    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);

    assert_eq!(app.command_buffer, "summary");
    assert!(app.command_completion.is_none());
}

#[test]
fn should_complete_command_when_only_one_candidate_matches() {
    // given
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "vers".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);
    // then
    assert_eq!(app.command_buffer, "version");
    assert!(app.command_completion.is_none());
}

#[test]
fn should_extend_to_common_command_prefix_before_cycling() {
    // given
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "set rel".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);
    // then
    assert_eq!(app.command_buffer, "set relativenumber");
    assert!(app.command_completion.is_none());
}

#[test]
fn should_cycle_forward_through_command_matches() {
    // given
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "set reviewed".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);
    // then
    assert_eq!(app.command_buffer, "set reviewed!");
    assert_eq!(
        app.command_completion
            .as_ref()
            .map(|completion| completion.prefix.as_str()),
        Some("set reviewed")
    );
    // when — cycling forward through a 2-candidate match set wraps back around
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);
    // then
    assert_eq!(app.command_buffer, "set reviewed");
}

#[test]
fn should_cycle_backward_through_command_matches() {
    // given
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "set reviewed".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommandReverse);
    // then
    assert_eq!(app.command_buffer, "set reviewed!");
}

#[test]
fn should_leave_unknown_command_completion_unchanged() {
    // given
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "zz".to_string();
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);
    // then
    assert_eq!(app.command_buffer, "zz");
    assert!(app.command_completion.is_none());
}

#[test]
fn should_clear_command_completion_state_after_manual_edit() {
    // given
    let mut app = build_app();
    app.input_mode = InputMode::Command;
    app.command_buffer = "set ".to_string();
    crate::handler::handle_command_action(&mut app, crate::input::Action::CompleteCommand);
    assert_eq!(app.command_buffer, "set wrap");
    assert!(app.command_completion.is_some());
    // when
    crate::handler::handle_command_action(&mut app, crate::input::Action::InsertChar('x'));
    // then
    assert_eq!(app.command_buffer, "set wrapx");
    assert!(app.command_completion.is_none());
}

#[test]
fn should_quit_from_commit_select_mode() {
    // given
    let mut app = build_app();
    // when
    crate::handler::handle_commit_select_action(&mut app, crate::input::Action::Quit);
    // then
    assert!(app.should_quit);
}
