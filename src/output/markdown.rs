use std::collections::HashSet;
use std::fmt::Write;
use std::io::Write as IoWrite;

use arboard::Clipboard;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::app::{CommentTypeDefinition, DiffSource};
use crate::config::ExportConfig;
use crate::error::{Result, TuicrError};
use crate::model::{CommentType, LineRange, LineSide, ReviewSession};
use crate::slug::short_sha;
/// (file_path, line_range, side, comment_type, content, commit_id)
type CommentEntry<'a> = (
    String,
    Option<LineRange>,
    Option<LineSide>,
    String,
    &'a str,
    Option<&'a str>,
);

/// Generate markdown content from the review session.
/// Returns the markdown string or an error if there are no comments.
pub fn generate_export_content(
    session: &ReviewSession,
    diff_source: &DiffSource,
    comment_types: &[CommentTypeDefinition],
    export: &ExportConfig,
) -> Result<String> {
    if !session.has_comments() {
        return Err(TuicrError::NoComments);
    }
    Ok(generate_markdown(
        session,
        diff_source,
        comment_types,
        export,
    ))
}

pub fn export_to_clipboard(
    session: &ReviewSession,
    diff_source: &DiffSource,
    comment_types: &[CommentTypeDefinition],
    export: &ExportConfig,
) -> Result<String> {
    let content = generate_export_content(session, diff_source, comment_types, export)?;
    let via_terminal = copy_text_to_clipboard(&content)?;
    Ok(if via_terminal {
        "Review copied to clipboard (via terminal)".to_string()
    } else {
        "Review copied to clipboard".to_string()
    })
}

/// Copy arbitrary text to the system clipboard. Returns `Ok(true)` if the
/// terminal-based fallback (tmux/OSC 52) was used, `Ok(false)` if the
/// platform clipboard handled it.
pub fn copy_text_to_clipboard(text: &str) -> Result<bool> {
    // On macOS, pbcopy writes straight to the system pasteboard and works even
    // inside tmux/SSH. OSC 52 (preferred below) instead relies on the outer
    // terminal honoring the escape, which Terminal.app does not, so the copy
    // would only reach the tmux buffer. Prefer pbcopy unconditionally here.
    if cfg!(target_os = "macos") && try_clipboard_cmd("pbcopy", &[], text) {
        return Ok(false);
    }
    if should_prefer_osc52() {
        copy_osc52(text)?;
        return Ok(true);
    }
    if try_copy_via_subprocess(text) {
        return Ok(false);
    }
    match Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
        Ok(_) => Ok(false),
        Err(_) => {
            copy_osc52(text)?;
            Ok(true)
        }
    }
}

/// Try xclip (X11) then wl-copy (Wayland). Returns true if either succeeds.
fn try_copy_via_subprocess(text: &str) -> bool {
    let session = std::env::var("XDG_SESSION_TYPE");
    if session.is_err() {
        // Session not specified
        return false;
    }
    let session = session.unwrap();
    if session == "wayland" {
        try_clipboard_cmd("wl-copy", &[], text)
    } else if session == "x11" {
        try_clipboard_cmd("xclip", &["-selection", "clipboard"], text)
    } else {
        // Session type unsupported
        false
    }
}

/// Try copying via a CLI tool that forks into the background and holds
/// clipboard ownership beyond tuicr's process lifetime. Returns true on success.
fn try_clipboard_cmd(program: &str, args: &[&str], text: &str) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, text.as_bytes());
    }
    matches!(child.wait(), Ok(s) if s.success())
}

/// Returns true if we should prefer OSC 52 over the system clipboard.
///
/// In tmux or SSH sessions, arboard may "succeed" but copy to an inaccessible
/// X11 clipboard, so we use OSC 52 which works reliably in these environments.
fn should_prefer_osc52() -> bool {
    std::env::var("TMUX").is_ok()
        || std::env::var("SSH_TTY").is_ok()
        || std::env::var("ZELLIJ").is_ok()
}

/// Copy text to clipboard using OSC 52 escape sequence.
/// In tmux, raw OSC 52 is intercepted and may not reach the outer terminal.
/// We use `tmux load-buffer -w` which tells tmux to handle the clipboard copy itself.
fn copy_osc52(text: &str) -> Result<()> {
    if std::env::var("TMUX").is_ok() {
        copy_via_tmux(text)
    } else {
        let mut stdout = std::io::stdout().lock();
        write_osc52(&mut stdout, text)
    }
}

/// Copy text to the system clipboard via `tmux load-buffer -w -`.
/// The `-w` flag tells tmux to also forward to the outer terminal's clipboard via OSC 52.
fn copy_via_tmux(text: &str) -> Result<()> {
    use std::process::{Command, Stdio};

    let mut child = Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| TuicrError::Clipboard(format!("Failed to run tmux: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| TuicrError::Clipboard(format!("Failed to write to tmux: {e}")))?;
    }

    let status = child
        .wait()
        .map_err(|e| TuicrError::Clipboard(format!("tmux load-buffer failed: {e}")))?;

    if !status.success() {
        return Err(TuicrError::Clipboard(
            "tmux load-buffer exited with error".to_string(),
        ));
    }

    Ok(())
}

/// Write OSC 52 escape sequence to the given writer.
/// Separated for testability.
fn write_osc52<W: IoWrite>(writer: &mut W, text: &str) -> Result<()> {
    let encoded = BASE64.encode(text);
    write!(writer, "\x1b]52;c;{encoded}\x07")
        .map_err(|e| TuicrError::Clipboard(format!("Failed to write OSC 52: {e}")))?;
    writer
        .flush()
        .map_err(|e| TuicrError::Clipboard(format!("Failed to flush: {e}")))?;
    Ok(())
}

fn review_scope_label(diff_source: &DiffSource) -> String {
    let scope = match diff_source {
        DiffSource::WorkingTree => "working tree changes".to_string(),
        DiffSource::StagedAndUnstaged => "staged + unstaged changes".to_string(),
        DiffSource::Staged => "staged changes".to_string(),
        DiffSource::Unstaged => "unstaged changes".to_string(),
        DiffSource::CommitRange(_) => "selected commit range".to_string(),
        DiffSource::StagedUnstagedAndCommits(_) => {
            "selected commit range + staged/unstaged changes".to_string()
        }
    };

    format!("Review Comment (scope: {scope})")
}

/// The "Reviewing …" banner. `None` for the working tree, which has never
/// carried one.
fn scope_banner(diff_source: &DiffSource) -> Option<String> {
    fn short_ids(commits: &[String]) -> String {
        commits
            .iter()
            .map(|c| &c[..7.min(c.len())])
            .collect::<Vec<_>>()
            .join(", ")
    }

    match diff_source {
        DiffSource::WorkingTree => None,
        DiffSource::Staged => Some("Reviewing staged changes".to_string()),
        DiffSource::Unstaged => Some("Reviewing unstaged changes".to_string()),
        DiffSource::StagedAndUnstaged => Some("Reviewing staged + unstaged changes".to_string()),
        DiffSource::CommitRange(commits) if commits.len() == 1 => Some(format!(
            "Reviewing commit: {}",
            &commits[0][..7.min(commits[0].len())]
        )),
        DiffSource::CommitRange(commits) => {
            Some(format!("Reviewing commits: {}", short_ids(commits)))
        }
        DiffSource::StagedUnstagedAndCommits(commits) => Some(format!(
            "Reviewing staged + unstaged + commits: {}",
            short_ids(commits)
        )),
    }
}

fn generate_markdown(
    session: &ReviewSession,
    diff_source: &DiffSource,
    comment_types: &[CommentTypeDefinition],
    export: &ExportConfig,
) -> String {
    let mut md = String::new();

    // Intro for agents. An empty string drops the line and its spacer so the
    // export opens directly on content.
    let intro = export.intro();
    if !intro.is_empty() {
        let _ = writeln!(md, "{intro}");
        let _ = writeln!(md);
    }

    // Scope banner: `scope_line` governs the prose "what am I reviewing" line.
    if export.scope_line()
        && let Some(banner) = scope_banner(diff_source)
    {
        let _ = writeln!(md, "{banner}");
        let _ = writeln!(md);
    }

    if export.legend() {
        let used_ids = collect_used_comment_type_ids(session);
        // The typeless `None` default never appears in the legend.
        let legend = comment_types
            .iter()
            .filter(|ct| ct.id != CommentType::NONE_ID)
            .filter(|ct| used_ids.is_empty() || used_ids.contains(&ct.id))
            .map(|comment_type| {
                let definition = comment_type
                    .definition
                    .as_deref()
                    .unwrap_or(comment_type.id.as_str());
                format!(
                    "{} ({})",
                    comment_type.label.to_ascii_uppercase(),
                    definition
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        // Omit the line entirely when there are no typed comments to document
        // (e.g. an untyped-only review).
        if !legend.is_empty() {
            let _ = writeln!(md, "Comment types: {legend}");
            let _ = writeln!(md);
        }
    }

    // Session notes/summary
    if let Some(notes) = &session.session_notes {
        let _ = writeln!(md, "Summary: {notes}");
        let _ = writeln!(md);
    }

    // Collect all comments into a flat list
    let mut all_comments: Vec<CommentEntry> = Vec::new();
    let review_comment_location = review_scope_label(diff_source);

    for comment in &session.review_comments {
        all_comments.push((
            review_comment_location.clone(),
            None,
            None,
            export_comment_type_label(&comment.comment_type, comment_types),
            &comment.content,
            None,
        ));
    }

    // Sort files by path for consistent output
    let mut files: Vec<_> = session.files.iter().collect();
    files.sort_by_key(|(path, _)| path.to_string_lossy().to_string());

    for (path, review) in files {
        let path_str = path.display().to_string();

        // File comments (no line number)
        for comment in &review.file_comments {
            all_comments.push((
                path_str.clone(),
                None,
                None,
                export_comment_type_label(&comment.comment_type, comment_types),
                &comment.content,
                comment.commit_id.as_deref(),
            ));
        }

        // Line comments (with line number, sorted)
        let mut line_comments: Vec<_> = review.line_comments.iter().collect();
        line_comments.sort_by_key(|(line, _)| *line);

        for (line, comments) in line_comments {
            for comment in comments {
                // Use comment's line_range if available, otherwise use the key line
                let line_range = comment
                    .line_range
                    .or_else(|| Some(LineRange::single(*line)));
                all_comments.push((
                    path_str.clone(),
                    line_range,
                    comment.side,
                    export_comment_type_label(&comment.comment_type, comment_types),
                    &comment.content,
                    comment.commit_id.as_deref(),
                ));
            }
        }
    }

    // Output numbered list. An empty header drops the heading but the
    // comments still follow.
    if !all_comments.is_empty() {
        let header = export.comments_header();
        if !header.is_empty() {
            let _ = writeln!(md, "{header}");
            let _ = writeln!(md);
        }
    }
    for (i, (file, line_range, side, comment_type, content, commit_id)) in
        all_comments.iter().enumerate()
    {
        let number = i + 1;
        let location = match (line_range, side) {
            // Range on deleted side (old lines)
            (Some(range), Some(LineSide::Old)) if range.is_single() => {
                format!("`{}:~{}`", file, range.start)
            }
            (Some(range), Some(LineSide::Old)) => {
                format!("`{}:~{}-~{}`", file, range.start, range.end)
            }
            // Range on new/context side
            (Some(range), _) if range.is_single() => {
                format!("`{}:{}`", file, range.start)
            }
            (Some(range), _) => {
                format!("`{}:{}-{}`", file, range.start, range.end)
            }
            // File comment
            (None, _) => format!("`{file}`"),
        };
        // Append the commit short SHA so the LLM knows which commit the
        // comment was made against — crucial for per-commit reviews.
        let commit_suffix = match commit_id {
            Some(sha) => format!(" (commit {})", short_sha(sha)),
            None => String::new(),
        };
        let marker = format!("{number}.");
        let continuation_indent = " ".repeat(marker.len() + 1);
        let mut content_lines = content.split('\n').map(|line| line.trim_end_matches('\r'));
        let first_line = content_lines.next().unwrap_or_default();
        // Untyped (`None`) comments export with no `**[TYPE]**` marker.
        let type_marker = if comment_type.is_empty() {
            String::new()
        } else {
            format!("**[{comment_type}]** ")
        };
        let _ = writeln!(
            md,
            "{marker} {type_marker}{location}{commit_suffix} - {first_line}"
        );
        for line in content_lines {
            let _ = writeln!(md, "{continuation_indent}{line}");
        }
    }

    md
}

fn collect_used_comment_type_ids(session: &ReviewSession) -> HashSet<String> {
    let mut ids = HashSet::new();
    for c in &session.review_comments {
        ids.insert(c.comment_type.id().to_string());
    }
    for review in session.files.values() {
        for c in &review.file_comments {
            ids.insert(c.comment_type.id().to_string());
        }
        for comments in review.line_comments.values() {
            for c in comments {
                ids.insert(c.comment_type.id().to_string());
            }
        }
    }
    ids
}

/// Export label for a comment type. Returns an empty string for
/// [`CommentType::None`] so the caller omits the `**[TYPE]**` marker.
fn export_comment_type_label(
    comment_type: &CommentType,
    comment_types: &[CommentTypeDefinition],
) -> String {
    if comment_type.is_none() {
        return String::new();
    }

    if let Some(definition) = comment_types
        .iter()
        .find(|definition| definition.id == comment_type.id())
    {
        return definition.label.to_ascii_uppercase();
    }

    comment_type.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CommentTypeDefinition;
    use crate::model::{Comment, CommentType, FileStatus, LineRange, LineSide, SessionDiffSource};
    use std::path::PathBuf;

    /// Export settings with only the legend suppressed.
    fn legend_off() -> ExportConfig {
        ExportConfig {
            legend: Some(false),
            ..Default::default()
        }
    }

    #[test]
    fn should_omit_scope_line_when_disabled() {
        let session = create_test_session();
        let export = ExportConfig {
            scope_line: Some(false),
            ..Default::default()
        };

        let markdown =
            generate_markdown(&session, &DiffSource::Unstaged, &comment_types(), &export);

        assert!(!markdown.contains("Reviewing unstaged changes"));
    }

    #[test]
    fn should_use_custom_export_intro() {
        let session = create_test_session();
        let export = ExportConfig {
            intro: Some("Code review comments:".to_string()),
            ..Default::default()
        };

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &export,
        );

        assert!(markdown.contains("Code review comments:"));
        assert!(!markdown.contains("I reviewed your code"));
    }

    #[test]
    fn should_omit_export_intro_and_its_spacer_when_empty() {
        let session = create_test_session();
        let export = ExportConfig {
            intro: Some(String::new()),
            ..Default::default()
        };

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &export,
        );

        assert!(!markdown.contains("I reviewed your code"));
        assert!(
            !markdown.starts_with('\n'),
            "the intro's trailing blank line should go with it, got:\n{markdown}"
        );
    }

    fn comment_types() -> Vec<CommentTypeDefinition> {
        vec![
            CommentTypeDefinition {
                id: "note".to_string(),
                label: "note".to_string(),
                definition: Some("observations".to_string()),
                color: None,
            },
            CommentTypeDefinition {
                id: "suggestion".to_string(),
                label: "suggestion".to_string(),
                definition: Some("improvements".to_string()),
                color: None,
            },
            CommentTypeDefinition {
                id: "issue".to_string(),
                label: "issue".to_string(),
                definition: Some("problems to fix".to_string()),
                color: None,
            },
            CommentTypeDefinition {
                id: "praise".to_string(),
                label: "praise".to_string(),
                definition: Some("positive feedback".to_string()),
                color: None,
            },
        ]
    }

    fn create_test_session() -> ReviewSession {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);

        // Add a file comment
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.reviewed = true;
            review.add_file_comment(Comment::new(
                "Consider adding documentation".to_string(),
                CommentType::from_id("suggestion"),
                None,
            ));
            review.add_line_comment(
                42,
                Comment::new(
                    "Magic number should be a constant".to_string(),
                    CommentType::from_id("issue"),
                    Some(LineSide::New),
                ),
            );
        }

        session
    }

    #[test]
    fn should_generate_valid_markdown() {
        // given
        let session = create_test_session();
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("I reviewed your code and have the following comments"));
        assert!(
            markdown.contains("Comment types: SUGGESTION (improvements), ISSUE (problems to fix)")
        );
        assert!(!markdown.contains("NOTE"));
        assert!(!markdown.contains("PRAISE"));
        assert!(markdown.contains("[SUGGESTION]"));
        assert!(markdown.contains("`src/main.rs`"));
        assert!(markdown.contains("Consider adding documentation"));
        assert!(markdown.contains("[ISSUE]"));
        assert!(markdown.contains("`src/main.rs:42`"));
        assert!(markdown.contains("Magic number"));
    }

    #[test]
    fn should_keep_commit_identity_for_commit_message_comments() {
        // Comments on the messages of different commits must not collapse into
        // an indistinguishable `Commit Message:N`. The synthetic commit-message
        // path carries the short id, which keeps each comment attributable to
        // its commit in the export.
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::CommitRange,
        );
        let first = PathBuf::from("Commit Message (ed50028)");
        let second = PathBuf::from("Commit Message (c17beb2)");
        session.add_file(first.clone(), FileStatus::Added, 0);
        session.add_file(second.clone(), FileStatus::Added, 0);
        if let Some(review) = session.get_file_mut(&first) {
            review.add_line_comment(
                1,
                Comment::new(
                    "We do not need this commit".to_string(),
                    CommentType::from_id("note"),
                    Some(LineSide::New),
                ),
            );
        }
        if let Some(review) = session.get_file_mut(&second) {
            review.add_line_comment(
                6,
                Comment::new(
                    "This is wrong".to_string(),
                    CommentType::from_id("note"),
                    Some(LineSide::New),
                ),
            );
        }

        let diff_source =
            DiffSource::CommitRange(vec!["ed50028".to_string(), "c17beb2".to_string()]);
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        assert!(
            markdown.contains("`Commit Message (ed50028):1`"),
            "expected first commit's message comment to be attributable in:\n{markdown}"
        );
        assert!(
            markdown.contains("`Commit Message (c17beb2):6`"),
            "expected second commit's message comment to be attributable in:\n{markdown}"
        );
    }

    #[test]
    fn should_use_configured_label_and_definition_in_export() {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.add_file_comment(Comment::new(
                "Needs clarification".to_string(),
                CommentType::from_id("note"),
                None,
            ));
        }

        let custom_types = vec![CommentTypeDefinition {
            id: "note".to_string(),
            label: "question".to_string(),
            definition: Some("ask for clarification".to_string()),
            color: None,
        }];

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &custom_types,
            &ExportConfig::default(),
        );

        assert!(markdown.contains("Comment types: QUESTION (ask for clarification)"));
        assert!(markdown.contains("**[QUESTION]**"));
    }

    #[test]
    fn should_number_comments_sequentially() {
        // given
        let session = create_test_session();
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        // Should have 2 numbered comments
        assert!(markdown.contains("1. **[SUGGESTION]**"));
        assert!(markdown.contains("2. **[ISSUE]**"));
    }

    #[test]
    fn should_indent_multiline_comments_under_single_digit_list_marker() {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.add_file_comment(Comment::new(
                "foo\nbar".to_string(),
                CommentType::from_id("suggestion"),
                None,
            ));
        }

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &ExportConfig::default(),
        );

        let expected = "\
1. **[SUGGESTION]** `src/main.rs` - foo
   bar";

        assert!(
            markdown.contains(expected),
            "expected multiline comment continuation to align under list text:\n{markdown}"
        );
    }

    #[test]
    fn should_indent_multiline_comments_under_double_digit_list_marker() {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            for i in 0..9 {
                review.add_file_comment(Comment::new(
                    format!("comment {i}"),
                    CommentType::from_id("note"),
                    None,
                ));
            }
            review.add_file_comment(Comment::new(
                "foo\nbar".to_string(),
                CommentType::from_id("suggestion"),
                None,
            ));
        }

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &ExportConfig::default(),
        );

        let expected = "\
10. **[SUGGESTION]** `src/main.rs` - foo
    bar";

        assert!(
            markdown.contains(expected),
            "expected double-digit continuation to align under list text:\n{markdown}"
        );
    }

    #[test]
    fn should_include_review_comments_in_export() {
        let mut session = create_test_session();
        session.review_comments.push(Comment::new(
            "Please split this into smaller commits".to_string(),
            CommentType::from_id("note"),
            None,
        ));

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &ExportConfig::default(),
        );

        assert!(markdown
            .contains("`Review Comment (scope: working tree changes)` - Please split this into smaller commits"));
    }

    #[test]
    fn should_include_commit_range_scope_for_review_comments() {
        let mut session = create_test_session();
        session.review_comments.push(Comment::new(
            "High-level concern across commits".to_string(),
            CommentType::from_id("issue"),
            None,
        ));

        let markdown = generate_markdown(
            &session,
            &DiffSource::CommitRange(vec!["abc1234567890".to_string()]),
            &comment_types(),
            &ExportConfig::default(),
        );

        assert!(markdown.contains(
            "`Review Comment (scope: selected commit range)` - High-level concern across commits"
        ));
    }

    #[test]
    fn should_include_commit_sha_in_export_for_commit_scoped_comments() {
        let mut session = create_test_session();
        // Add a line comment scoped to commit abc1234567890
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.add_line_comment(
                10,
                Comment::new(
                    "Wrong variable name".to_string(),
                    CommentType::from_id("issue"),
                    Some(LineSide::New),
                )
                .with_commit_id("abc1234567890"),
            );
        }

        let markdown = generate_markdown(
            &session,
            &DiffSource::CommitRange(vec!["abc1234567890".to_string()]),
            &comment_types(),
            &ExportConfig::default(),
        );

        assert!(
            markdown.contains("`src/main.rs:10` (commit abc1234) - Wrong variable name"),
            "commit-scoped comment must include the short SHA in the export; got:\n{markdown}"
        );
    }

    #[test]
    fn should_omit_commit_suffix_for_unscoped_comments() {
        let session = create_test_session();

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &ExportConfig::default(),
        );

        // Existing comments have commit_id = None — no (commit ...) suffix
        assert!(
            !markdown.contains("(commit "),
            "unscoped comments must not have a commit suffix; got:\n{markdown}"
        );
    }

    #[test]
    fn should_fail_export_when_no_comments() {
        // given
        let session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        let diff_source = DiffSource::WorkingTree;

        // when
        let result = export_to_clipboard(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TuicrError::NoComments));
    }

    #[test]
    fn should_generate_export_content_with_comments() {
        // given
        let session = create_test_session();
        let diff_source = DiffSource::WorkingTree;

        // when
        let result = generate_export_content(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(result.is_ok());
        let content = result.unwrap();
        assert!(content.contains("I reviewed your code"));
        assert!(content.contains("[SUGGESTION]"));
        assert!(content.contains("[ISSUE]"));
    }

    #[test]
    fn should_fail_generate_export_content_when_no_comments() {
        // given
        let session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        let diff_source = DiffSource::WorkingTree;

        // when
        let result = generate_export_content(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TuicrError::NoComments));
    }

    #[test]
    fn should_include_commit_range_in_markdown() {
        // given
        let session = create_test_session();
        let diff_source = DiffSource::CommitRange(vec![
            "abc1234567890".to_string(),
            "def4567890123".to_string(),
        ]);

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("Reviewing commits: abc1234, def4567"));
    }

    #[test]
    fn should_include_single_commit_in_markdown() {
        // given
        let session = create_test_session();
        let diff_source = DiffSource::CommitRange(vec!["abc1234567890".to_string()]);

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("Reviewing commit: abc1234"));
    }

    #[test]
    fn should_write_osc52_escape_sequence() {
        // given
        let text = "Hello, World!";
        let mut buffer: Vec<u8> = Vec::new();

        // when
        write_osc52(&mut buffer, text).unwrap();

        // then
        let output = String::from_utf8(buffer).unwrap();
        // OSC 52 format: ESC ] 52 ; c ; <base64> BEL
        assert!(output.starts_with("\x1b]52;c;"));
        assert!(output.ends_with("\x07"));
        // Verify the base64 content
        let base64_content = &output[7..output.len() - 1];
        assert_eq!(BASE64.encode(text), base64_content);
    }

    #[test]
    fn should_encode_empty_string_in_osc52() {
        // given
        let text = "";
        let mut buffer: Vec<u8> = Vec::new();

        // when
        write_osc52(&mut buffer, text).unwrap();

        // then
        let output = String::from_utf8(buffer).unwrap();
        assert_eq!(output, "\x1b]52;c;\x07");
    }

    #[test]
    fn should_encode_unicode_in_osc52() {
        // given
        let text = "こんにちは 🦀";
        let mut buffer: Vec<u8> = Vec::new();

        // when
        write_osc52(&mut buffer, text).unwrap();

        // then
        let output = String::from_utf8(buffer).unwrap();
        let base64_content = &output[7..output.len() - 1];
        // Decode and verify it matches original
        let decoded = String::from_utf8(BASE64.decode(base64_content).unwrap()).unwrap();
        assert_eq!(decoded, text);
    }

    #[test]
    fn should_encode_markdown_content_in_osc52() {
        // given - simulate what would be copied during export
        let session = create_test_session();
        let diff_source = DiffSource::WorkingTree;
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );
        let mut buffer: Vec<u8> = Vec::new();

        // when
        write_osc52(&mut buffer, &markdown).unwrap();

        // then
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.starts_with("\x1b]52;c;"));
        assert!(output.ends_with("\x07"));
        // Verify we can decode the base64 back to the original markdown
        let base64_content = &output[7..output.len() - 1];
        let decoded = String::from_utf8(BASE64.decode(base64_content).unwrap()).unwrap();
        assert_eq!(decoded, markdown);
    }

    #[test]
    fn should_export_single_line_range_as_single_line() {
        // given - a comment with a single-line range should display as L42, not L42-L42
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);

        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            let range = LineRange::single(42);
            review.add_line_comment(
                42,
                Comment::new_with_range(
                    "Single line comment".to_string(),
                    CommentType::from_id("note"),
                    Some(LineSide::New),
                    range,
                ),
            );
        }
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("`src/main.rs:42`"));
        assert!(!markdown.contains("`src/main.rs:42-42`"));
    }

    #[test]
    fn should_export_line_range_with_start_and_end() {
        // given - a comment spanning multiple lines
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);

        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            let range = LineRange::new(10, 15);
            review.add_line_comment(
                15, // keyed by end line
                Comment::new_with_range(
                    "Multi-line comment".to_string(),
                    CommentType::from_id("issue"),
                    Some(LineSide::New),
                    range,
                ),
            );
        }
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("`src/main.rs:10-15`"));
        assert!(markdown.contains("Multi-line comment"));
    }

    #[test]
    fn should_export_old_side_line_range_with_tilde() {
        // given - a range comment on deleted lines (old side)
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);

        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            let range = LineRange::new(20, 25);
            review.add_line_comment(
                25, // keyed by end line
                Comment::new_with_range(
                    "Deleted lines comment".to_string(),
                    CommentType::from_id("suggestion"),
                    Some(LineSide::Old),
                    range,
                ),
            );
        }
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("`src/main.rs:~20-~25`"));
    }

    #[test]
    fn should_export_single_old_side_line_with_tilde() {
        // given - a single line comment on a deleted line
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);

        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            let range = LineRange::single(30);
            review.add_line_comment(
                30,
                Comment::new_with_range(
                    "Single deleted line".to_string(),
                    CommentType::from_id("note"),
                    Some(LineSide::Old),
                    range,
                ),
            );
        }
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("`src/main.rs:~30`"));
        assert!(!markdown.contains("`src/main.rs:~30-~30`"));
    }

    #[test]
    fn should_handle_comment_without_line_range_field() {
        // given - backward compatibility: comment without line_range uses line number
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);

        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            // Use Comment::new which sets line_range to None
            review.add_line_comment(
                50,
                Comment::new(
                    "Old style comment".to_string(),
                    CommentType::from_id("note"),
                    Some(LineSide::New),
                ),
            );
        }
        let diff_source = DiffSource::WorkingTree;

        // when
        let markdown = generate_markdown(
            &session,
            &diff_source,
            &comment_types(),
            &ExportConfig::default(),
        );

        // then
        assert!(markdown.contains("`src/main.rs:50`"));
    }

    #[test]
    fn should_omit_legend_when_legend_is_disabled() {
        let session = create_test_session();
        let diff_source = DiffSource::WorkingTree;

        let markdown = generate_markdown(&session, &diff_source, &comment_types(), &legend_off());

        assert!(!markdown.contains("Comment types:"));
        assert!(markdown.contains("[SUGGESTION]"));
        assert!(markdown.contains("[ISSUE]"));
    }

    #[test]
    fn should_only_list_used_comment_types_in_legend() {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.add_file_comment(Comment::new(
                "Great work!".to_string(),
                CommentType::from_id("praise"),
                None,
            ));
        }

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &comment_types(),
            &ExportConfig::default(),
        );

        assert!(markdown.contains("Comment types: PRAISE (positive feedback)"));
        assert!(!markdown.contains("NOTE"));
        assert!(!markdown.contains("SUGGESTION"));
        assert!(!markdown.contains("ISSUE"));
    }

    #[test]
    fn should_export_none_typed_comments_without_marker_or_legend() {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.add_line_comment(
                42,
                Comment::new(
                    "plain observation".to_string(),
                    CommentType::None,
                    Some(LineSide::New),
                ),
            );
        }

        // Pass the resolved default set (just `None`) to mirror an unconfigured
        // review.
        let none_only = vec![CommentTypeDefinition {
            id: CommentType::NONE_ID.to_string(),
            label: CommentType::NONE_ID.to_string(),
            definition: None,
            color: None,
        }];
        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &none_only,
            &ExportConfig::default(),
        );

        // No `[TYPE]` marker on the comment and no legend line at all.
        assert!(
            !markdown.contains("Comment types:"),
            "unexpected legend:\n{markdown}"
        );
        assert!(
            !markdown.contains("**["),
            "unexpected type marker:\n{markdown}"
        );
        assert!(
            !markdown.contains("[NONE]"),
            "None must not render:\n{markdown}"
        );
        assert!(markdown.contains("`src/main.rs:42`"));
        assert!(markdown.contains("plain observation"));
    }

    #[test]
    fn should_only_list_used_custom_types_in_legend() {
        let mut session = ReviewSession::new(
            PathBuf::from("/tmp/test-repo"),
            "abc1234def".to_string(),
            Some("main".to_string()),
            SessionDiffSource::WorkingTree,
        );
        session.add_file(PathBuf::from("src/main.rs"), FileStatus::Modified, 0);
        if let Some(review) = session.get_file_mut(&PathBuf::from("src/main.rs")) {
            review.add_file_comment(Comment::new(
                "Needs clarification".to_string(),
                CommentType::from_id("note"),
                None,
            ));
        }

        let custom_types = vec![
            CommentTypeDefinition {
                id: "note".to_string(),
                label: "question".to_string(),
                definition: Some("ask for clarification".to_string()),
                color: None,
            },
            CommentTypeDefinition {
                id: "issue".to_string(),
                label: "issue".to_string(),
                definition: Some("problems to fix".to_string()),
                color: None,
            },
        ];

        let markdown = generate_markdown(
            &session,
            &DiffSource::WorkingTree,
            &custom_types,
            &ExportConfig::default(),
        );

        assert!(markdown.contains("Comment types: QUESTION (ask for clarification)"));
        assert!(!markdown.contains("ISSUE"));
    }
}
