//! VCS abstraction layer for supporting multiple version control systems.
//!
//! Currently supports:
//! - Git
//! - Mercurial
//! - Jujutsu
//!
//! ## Detection Order
//!
//! When auto-detecting the VCS type, Jujutsu is tried first because jj repos
//! are Git-backed and contain a `.git` directory. If jj detection fails, Git
//! is tried next, then Mercurial.

pub(crate) mod diff_parser;
pub mod file;
pub mod git;
mod hg;
mod jj;
pub mod pristine;
pub(crate) mod traits;

pub use file::FileBackend;
pub use git::{GitBackend, GitBackendPreference};
pub use hg::HgBackend;
pub use jj::JjBackend;
pub use traits::{
    ChangeKind, CommitInfo, DiffWhitespaceMode, ResolvedRevisionRange, RevisionDiffTarget,
    VcsBackend, VcsChangeStatus, VcsInfo,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Result, TuicrError};
use crate::model::diff_types::WholeFileText;
use crate::model::{DiffFile, DiffLine, LineOrigin, LineSide};
use crate::syntax::needs_full_file_highlight;

/// Boundary marker emitted between files in batched `hg cat` / `jj file show`
/// output. The long random suffix makes accidental collision with real source
/// content effectively impossible.
pub(crate) const BATCH_BOUNDARY: &str = "@@TUICR_BATCH_BOUNDARY_e97f2d44_8b1a@@";

/// Collect the unique paths of files that need full-file syntax highlighting
/// (Vue, Svelte, PHP and friends) on the given side, skipping binary, too-large,
/// or empty entries. Used by hg / jj to know which files to batch-fetch.
pub(crate) fn container_file_paths(files: &[DiffFile], side: LineSide) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|f| !f.is_binary && !f.is_too_large && !f.hunks.is_empty())
        .filter_map(|f| {
            let syntax_path = f.new_path.as_deref().or(f.old_path.as_deref())?;
            if !needs_full_file_highlight(syntax_path) {
                return None;
            }
            match side {
                LineSide::Old => f.old_path.clone(),
                LineSide::New => f.new_path.clone(),
            }
        })
        .collect()
}

/// Expand tabs to spaces in diff line content so highlighted spans line up
/// with the displayed text in side-by-side and unified rendering.
pub(crate) fn tabify(s: &str) -> String {
    s.replace('\t', "    ")
}

/// Slice `[start_line, end_line]` (1-indexed, inclusive) into `DiffLine`s.
pub(crate) fn slice_context_lines(content: &str, start_line: u32, end_line: u32) -> Vec<DiffLine> {
    if start_line > end_line || start_line == 0 {
        return Vec::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    for line_num in start_line..=end_line {
        let idx = (line_num - 1) as usize;
        if idx >= lines.len() {
            break;
        }
        result.push(DiffLine {
            origin: LineOrigin::Context,
            content: tabify(lines[idx]),
            old_lineno: Some(line_num),
            new_lineno: Some(line_num),
            highlighted_spans: None,
        });
    }
    result
}

/// Read a file from the working tree, returning `None` on any IO error.
pub(crate) fn read_workdir_file(root: &Path, rel: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// Parse the output of a batched `hg cat` / `jj file show` invocation whose
/// template prefixed each file with `\n{BATCH_BOUNDARY}\n{path}\n` before
/// emitting `{data}`. Returns a `path → data` map.
pub(crate) fn parse_batched_files(output: &str) -> HashMap<PathBuf, String> {
    let sep = format!("\n{BATCH_BOUNDARY}\n");
    output
        .split(&sep)
        .filter(|s| !s.is_empty())
        .filter_map(|block| {
            let mut iter = block.splitn(2, '\n');
            let path = iter.next()?;
            let data = iter.next().unwrap_or("");
            Some((PathBuf::from(path), data.to_string()))
        })
        .collect()
}

/// Attach whole-file text to container-grammar files (Vue, Svelte, etc) at
/// the requested revisions, for the lazy highlighter to consume. `new_rev =
/// None` reads the new side from the working tree on disk instead of calling
/// `fetch_batch`. The `fetch_batch` closure is the backend-specific
/// batched-fetch primitive (`hg cat -r REV ...` or `jj file show -r REV ...`).
pub(crate) fn attach_container_whole_file_text<F>(
    root: &Path,
    old_rev: &str,
    new_rev: Option<&str>,
    files: &mut [DiffFile],
    fetch_batch: F,
) -> Result<()>
where
    F: Fn(&Path, &str, &[PathBuf]) -> Result<HashMap<PathBuf, String>>,
{
    let old_paths = container_file_paths(files, LineSide::Old);
    let new_paths = container_file_paths(files, LineSide::New);

    if old_paths.is_empty() && new_paths.is_empty() {
        return Ok(());
    }

    let mut old_map = fetch_batch(root, old_rev, &old_paths)?;
    let mut new_map = match new_rev {
        Some(rev) => fetch_batch(root, rev, &new_paths)?,
        None => HashMap::new(),
    };

    let workdir = new_rev.is_none().then(|| root.to_path_buf());

    attach_whole_file_text(
        files,
        |p| old_map.remove(p),
        |p| match (new_map.remove(p), workdir.as_deref()) {
            (Some(content), _) => Some(content),
            (None, Some(root)) => read_workdir_file(root, p),
            (None, None) => None,
        },
    );

    Ok(())
}

/// Attach the whole old and new text to each file whose grammar needs it
/// (see `needs_full_file_highlight`), so the lazy highlighter can parse the
/// file from its first line when the file is first shown. Other files are
/// left alone and highlighted per hunk.
///
/// `fetch_old`/`fetch_new` return the entire content of the file at the old
/// and new sides respectively (or `None` if unavailable). Oversized or binary
/// text is dropped by `WholeFileText::from_sides`.
pub(crate) fn attach_whole_file_text<F, G>(
    files: &mut [DiffFile],
    mut fetch_old: F,
    mut fetch_new: G,
) where
    F: FnMut(&Path) -> Option<String>,
    G: FnMut(&Path) -> Option<String>,
{
    for file in files.iter_mut() {
        if file.is_binary || file.is_too_large || file.hunks.is_empty() {
            continue;
        }
        let Some(syntax_path) = file.new_path.as_deref().or(file.old_path.as_deref()) else {
            continue;
        };
        if !needs_full_file_highlight(syntax_path) {
            continue;
        }
        let old_content = file.old_path.as_deref().and_then(&mut fetch_old);
        let new_content = file.new_path.as_deref().and_then(&mut fetch_new);
        file.whole_file_text = WholeFileText::from_sides(old_content, new_content);
    }
}

/// Detect the VCS type and return the appropriate backend.
///
/// Detection order: Jujutsu → Git → Mercurial.
/// Jujutsu is tried first because jj repos are Git-backed.
pub fn detect_vcs(
    git_backend_preference: GitBackendPreference,
    whitespace_mode: DiffWhitespaceMode,
) -> Result<Box<dyn VcsBackend>> {
    // Try jj first since jj repos are Git-backed
    if let Ok(backend) = JjBackend::discover(whitespace_mode) {
        return Ok(Box::new(backend));
    }

    // Try git
    if let Ok(backend) = GitBackend::discover(git_backend_preference, whitespace_mode) {
        return Ok(Box::new(backend));
    }

    // Try hg
    if let Ok(backend) = HgBackend::discover(whitespace_mode) {
        return Ok(Box::new(backend));
    }

    Err(TuicrError::NotARepository)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::traits::VcsType;
    use std::path::PathBuf;

    #[test]
    fn exports_are_accessible() {
        // Verify that public types are properly exported
        let _: fn(GitBackendPreference, DiffWhitespaceMode) -> Result<Box<dyn VcsBackend>> =
            detect_vcs;

        // VcsInfo can be constructed
        let info = VcsInfo {
            root_path: PathBuf::from("/test"),
            head_commit: "abc".to_string(),
            branch_name: None,
            vcs_type: VcsType::Git,
        };
        assert_eq!(info.head_commit, "abc");

        // CommitInfo can be constructed
        let commit = CommitInfo {
            id: "abc".to_string(),
            short_id: "abc".to_string(),
            branch_name: Some("main".to_string()),
            summary: "test".to_string(),
            body: None,
            author: "author".to_string(),
            time: chrono::Utc::now(),
        };
        assert_eq!(commit.id, "abc");
    }

    #[test]
    fn detect_vcs_outside_repo_returns_error() {
        // When run outside any VCS repo, should return NotARepository
        // Note: This test may pass or fail depending on where tests are run
        // In CI or outside a repo, it should fail with NotARepository
        // Inside the tuicr repo (which is git), it will succeed
        let result = detect_vcs(GitBackendPreference::Libgit2, DiffWhitespaceMode::Normal);

        // We just verify the function runs without panic
        // The actual result depends on the environment
        match result {
            Ok(backend) => {
                // If we're in a repo, we should get valid info
                let info = backend.info();
                assert!(!info.head_commit.is_empty());
            }
            Err(TuicrError::NotARepository) => {
                // Expected when outside a repo
            }
            Err(e) => {
                panic!("Unexpected error: {e:?}");
            }
        }
    }

    #[test]
    fn slice_context_lines_expands_tabs() {
        let content = "fn main() {\n\tprintln!(\"hi\");\n}";

        let lines = slice_context_lines(content, 2, 2);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content, "    println!(\"hi\");");
    }

    fn vue_diff_file(
        idx: usize,
        deleted_line: &str,
        added_line: &str,
        target_line: u32,
    ) -> DiffFile {
        use crate::model::diff_types::{DiffHunk, DiffLine, FileStatus, LineOrigin};
        let path = PathBuf::from(format!("Comp{idx}.vue"));
        let hunk = DiffHunk {
            header: format!("@@ -{target_line} +{target_line} @@"),
            lines: vec![
                DiffLine {
                    origin: LineOrigin::Deletion,
                    content: deleted_line.to_string(),
                    old_lineno: Some(target_line),
                    new_lineno: None,
                    highlighted_spans: None,
                },
                DiffLine {
                    origin: LineOrigin::Addition,
                    content: added_line.to_string(),
                    old_lineno: None,
                    new_lineno: Some(target_line),
                    highlighted_spans: None,
                },
            ],
            old_start: target_line,
            old_count: 1,
            new_start: target_line,
            new_count: 1,
            highlight: Default::default(),
        };
        DiffFile {
            old_path: Some(path.clone()),
            new_path: Some(path),
            status: FileStatus::Modified,
            hunks: vec![hunk],
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash: 0,
            whole_file_text: None,
        }
    }

    #[test]
    fn attach_whole_file_text_should_only_touch_container_files() {
        let mut vue = vue_diff_file(0, "a", "b", 1);
        let mut rust = vue_diff_file(1, "a", "b", 1);
        rust.new_path = Some(PathBuf::from("main.rs"));
        rust.old_path = rust.new_path.clone();
        let mut files = vec![vue.clone(), rust.clone()];

        attach_whole_file_text(
            &mut files,
            |_| Some("old".to_string()),
            |_| Some("new".to_string()),
        );

        let text = files[0]
            .whole_file_text
            .as_ref()
            .expect("vue file gets text");
        assert_eq!(text.old.as_deref(), Some("old"));
        assert_eq!(text.new.as_deref(), Some("new"));
        assert!(files[1].whole_file_text.is_none());

        vue.is_binary = true;
        let mut files = vec![vue];
        attach_whole_file_text(&mut files, |_| Some("old".into()), |_| Some("new".into()));
        assert!(files[0].whole_file_text.is_none());
    }

    #[test]
    fn attach_whole_file_text_should_drop_missing_and_oversized_sides() {
        let mut files = vec![vue_diff_file(0, "a", "b", 1)];
        attach_whole_file_text(&mut files, |_| None, |_| None);
        assert!(files[0].whole_file_text.is_none());

        let huge = "x".repeat(WholeFileText::MAX_BYTES + 1);
        attach_whole_file_text(&mut files, |_| Some(huge.clone()), |_| Some("new".into()));
        let text = files[0].whole_file_text.as_ref().unwrap();
        assert!(text.old.is_none());
        assert_eq!(text.new.as_deref(), Some("new"));

        attach_whole_file_text(&mut files, |_| Some("a\0b".into()), |_| None);
        assert!(files[0].whole_file_text.is_none());
    }

    #[test]
    fn attach_container_whole_file_text_should_read_new_side_from_workdir() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("Comp0.vue"), "on disk").unwrap();
        let mut files = vec![vue_diff_file(0, "a", "b", 1)];
        let fetched = std::cell::RefCell::new(Vec::new());

        attach_container_whole_file_text(temp.path(), "base", None, &mut files, |_, rev, paths| {
            fetched.borrow_mut().push((rev.to_string(), paths.to_vec()));
            Ok(paths
                .iter()
                .map(|p| (p.clone(), format!("{rev}:{}", p.display())))
                .collect())
        })
        .unwrap();

        let text = files[0].whole_file_text.as_ref().unwrap();
        assert_eq!(text.old.as_deref(), Some("base:Comp0.vue"));
        assert_eq!(text.new.as_deref(), Some("on disk"));
        assert_eq!(
            fetched.borrow().len(),
            1,
            "no batch fetch for the workdir side"
        );
    }
}
