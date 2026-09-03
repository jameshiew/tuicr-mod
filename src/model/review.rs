use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::comment::{Comment, CommentType};
use super::diff_types::{DiffFile, FileStatus};
use crate::error::{Result, TuicrError};
use crate::model::{LineRange, LineSide};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearScope {
    CommentsOnly,
    CommentsAndReviewed,
}

#[derive(Debug, Clone)]
pub struct FileReview {
    pub path: PathBuf,
    pub reviewed: bool,
    pub status: FileStatus,
    pub file_comments: Vec<Comment>,
    pub line_comments: HashMap<u32, Vec<Comment>>,
    pub reviewed_hunks: BTreeSet<String>,
    pub content_hash: Option<u64>,
}

impl FileReview {
    pub fn new(path: PathBuf, status: FileStatus, content_hash: u64) -> Self {
        Self {
            path,
            reviewed: false,
            status,
            file_comments: Vec::new(),
            line_comments: HashMap::new(),
            reviewed_hunks: BTreeSet::new(),
            content_hash: Some(content_hash),
        }
    }

    pub fn comment_count(&self) -> usize {
        self.file_comments.len() + self.line_comments.values().map(|v| v.len()).sum::<usize>()
    }

    pub fn add_file_comment(&mut self, comment: Comment) {
        self.file_comments.push(comment);
    }

    pub fn add_line_comment(&mut self, line: u32, comment: Comment) {
        self.line_comments.entry(line).or_default().push(comment);
    }

    pub fn toggle_hunk_reviewed(&mut self, key: String) -> bool {
        if self.reviewed_hunks.contains(&key) {
            self.reviewed_hunks.remove(&key);
            false
        } else {
            self.reviewed_hunks.insert(key);
            true
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionDiffSource {
    #[default]
    WorkingTree,
    Staged,
    Unstaged,
    StagedAndUnstaged,
    CommitRange,
    WorkingTreeAndCommits,
    StagedUnstagedAndCommits,
    /// Whole-repo annotation surface. Every tracked file is shown in
    /// context-only rendering, sourced from `git ls-files`. The
    /// `base_commit` for these sessions starts with `"pristine:"`.
    Pristine,
}

#[derive(Debug, Clone)]
pub struct ReviewSession {
    pub repo_path: PathBuf,
    pub branch_name: Option<String>,
    pub base_commit: String,
    pub diff_source: SessionDiffSource,
    pub commit_range: Option<Vec<String>>,
    pub review_comments: Vec<Comment>,
    pub files: HashMap<PathBuf, FileReview>,
    pub session_notes: Option<String>,
}

impl ReviewSession {
    pub fn new(
        repo_path: PathBuf,
        base_commit: String,
        branch_name: Option<String>,
        diff_source: SessionDiffSource,
    ) -> Self {
        Self {
            repo_path,
            branch_name,
            base_commit,
            diff_source,
            commit_range: None,
            review_comments: Vec::new(),
            files: HashMap::new(),
            session_notes: None,
        }
    }

    pub fn reviewed_count(&self) -> usize {
        self.files.values().filter(|f| f.reviewed).count()
    }

    pub fn has_reviewed_state(&self) -> bool {
        self.files
            .values()
            .any(|file| file.reviewed || !file.reviewed_hunks.is_empty())
    }

    /// Registers a file in the session. Returns true if the file was previously
    /// reviewed but its content changed, causing reviewed status to be reset.
    pub fn add_file(&mut self, path: PathBuf, status: FileStatus, content_hash: u64) -> bool {
        if let Some(review) = self.files.get_mut(&path) {
            let old_hash = review.content_hash;
            review.content_hash = Some(content_hash);
            if review.reviewed && old_hash != Some(content_hash) {
                review.reviewed = false;
                return true;
            }
            return false;
        }
        self.files
            .insert(path.clone(), FileReview::new(path, status, content_hash));
        false
    }

    pub fn add_diff_file(&mut self, file: &DiffFile) -> bool {
        let path = file.display_path().clone();
        let invalidated = self.add_file(path.clone(), file.status, file.content_hash);
        if let Some(review) = self.files.get_mut(&path) {
            let valid_hunks: BTreeSet<_> = file.hunk_review_keys().into_iter().collect();
            review
                .reviewed_hunks
                .retain(|key| valid_hunks.contains(key));
        }
        invalidated
    }

    /// Register a transient filtered diff without dropping hunk keys that
    /// belong to the broader persisted review scope.
    pub fn add_diff_file_preserving_hunks(&mut self, file: &DiffFile) -> bool {
        self.add_file(file.display_path().clone(), file.status, file.content_hash)
    }

    pub fn get_file_mut(&mut self, path: &PathBuf) -> Option<&mut FileReview> {
        self.files.get_mut(path)
    }

    pub fn has_comments(&self) -> bool {
        !self.review_comments.is_empty() || self.files.values().any(|f| f.comment_count() > 0)
    }

    pub fn clear_comments(&mut self, scope: ClearScope) -> (usize, usize) {
        let mut cleared = self.review_comments.len();
        let mut unreviewed = 0;
        self.review_comments.clear();
        for file in self.files.values_mut() {
            cleared += file.comment_count();
            file.file_comments.clear();
            file.line_comments.clear();
            if scope == ClearScope::CommentsAndReviewed {
                if file.reviewed || !file.reviewed_hunks.is_empty() {
                    unreviewed += 1;
                }
                file.reviewed = false;
                file.reviewed_hunks.clear();
            }
        }
        (cleared, unreviewed)
    }

    pub fn is_file_reviewed(&self, path: &PathBuf) -> bool {
        self.files.get(path).map(|r| r.reviewed).unwrap_or(false)
    }

    pub fn is_hunk_reviewed(&self, path: &PathBuf, key: &str) -> bool {
        self.files
            .get(path)
            .is_some_and(|review| review.reviewed_hunks.contains(key))
    }
}

/// Request to add a local draft comment to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddCommentRequest {
    pub target: CommentTarget,
    pub content: String,
    pub comment_type: CommentType,
    /// Author to stamp on the resulting comment. Caller is responsible for
    /// picking a sensible default (`Comment::DEFAULT_AUTHOR`) when none is
    /// supplied.
    pub author: String,
    /// Commit SHA to stamp on the comment when it was created while the
    /// inline commit selector showed exactly one commit. `None` for
    /// review-level comments and full-range selections.
    pub commit_id: Option<String>,
}

/// Where a new local draft comment should be attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentTarget {
    Review,
    File {
        path: PathBuf,
    },
    Line {
        path: PathBuf,
        line: u32,
        side: LineSide,
    },
    LineRange {
        path: PathBuf,
        range: LineRange,
        side: LineSide,
    },
}

/// Add a local draft comment to an in-memory session.
pub fn add_comment_to_session(
    session: &mut ReviewSession,
    request: AddCommentRequest,
) -> Result<Comment> {
    let content = request.content.trim().to_string();
    if content.is_empty() {
        return Err(TuicrError::InvalidInput(
            "comment cannot be empty".to_string(),
        ));
    }

    let author = request.author;
    let commit_id = request.commit_id;
    let comment = match request.target {
        CommentTarget::Review => {
            let comment = Comment::new(content, request.comment_type, None).with_author(author);
            session.review_comments.push(comment.clone());
            comment
        }
        CommentTarget::File { path } => {
            let review = file_review_mut(session, &path)?;
            let mut comment = Comment::new(content, request.comment_type, None).with_author(author);
            if let Some(sha) = &commit_id {
                comment = comment.with_commit_id(sha.clone());
            }
            review.add_file_comment(comment.clone());
            comment
        }
        CommentTarget::Line { path, line, side } => {
            let review = file_review_mut(session, &path)?;
            let mut comment =
                Comment::new(content, request.comment_type, Some(side)).with_author(author);
            if let Some(sha) = &commit_id {
                comment = comment.with_commit_id(sha.clone());
            }
            review.add_line_comment(line, comment.clone());
            comment
        }
        CommentTarget::LineRange { path, range, side } => {
            let review = file_review_mut(session, &path)?;
            let mut comment =
                Comment::new_with_range(content, request.comment_type, Some(side), range)
                    .with_author(author);
            if let Some(sha) = &commit_id {
                comment = comment.with_commit_id(sha.clone());
            }
            review.add_line_comment(range.end, comment.clone());
            comment
        }
    };

    Ok(comment)
}

fn file_review_mut<'a>(session: &'a mut ReviewSession, path: &Path) -> Result<&'a mut FileReview> {
    session.get_file_mut(&path.to_path_buf()).ok_or_else(|| {
        TuicrError::InvalidInput(format!("session does not contain file {}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::comment::{Comment, CommentType};
    use crate::model::{DiffHunk, DiffLine, LineOrigin};

    // Arbitrary hash value for tests that don't care about the specific hash.
    const SOME_HASH: u64 = 0xdeadbeef;

    fn test_session() -> ReviewSession {
        ReviewSession::new(
            PathBuf::from("/repo"),
            "abc123".to_string(),
            None,
            SessionDiffSource::WorkingTree,
        )
    }

    fn test_hunk(new_start: u32, content: &str) -> DiffHunk {
        DiffHunk {
            header: format!("@@ -{new_start},1 +{new_start},1 @@"),
            lines: vec![DiffLine {
                origin: LineOrigin::Context,
                content: content.to_string(),
                old_lineno: Some(new_start),
                new_lineno: Some(new_start),
                highlighted_spans: None,
            }],
            old_start: new_start,
            old_count: 1,
            new_start,
            new_count: 1,
        }
    }

    fn test_diff_file(path: &str, hunks: Vec<DiffHunk>) -> DiffFile {
        let content_hash = DiffFile::compute_content_hash(&hunks);
        DiffFile {
            old_path: None,
            new_path: Some(PathBuf::from(path)),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash,
        }
    }

    #[test]
    fn should_return_zero_when_clearing_empty_session() {
        let mut session = test_session();
        let (cleared, unreviewed) = session.clear_comments(ClearScope::CommentsAndReviewed);
        assert_eq!(cleared, 0);
        assert_eq!(unreviewed, 0);
    }

    #[test]
    fn should_clear_review_level_comments() {
        let mut session = test_session();
        session.review_comments.push(Comment::new(
            "note".to_string(),
            CommentType::from_id("note"),
            None,
        ));
        session.review_comments.push(Comment::new(
            "issue".to_string(),
            CommentType::from_id("issue"),
            None,
        ));

        let (cleared, unreviewed) = session.clear_comments(ClearScope::CommentsAndReviewed);
        assert_eq!(cleared, 2);
        assert_eq!(unreviewed, 0);
        assert!(session.review_comments.is_empty());
    }

    #[test]
    fn should_clear_file_and_line_comments() {
        let mut session = test_session();
        let path = PathBuf::from("src/main.rs");
        session.add_file(path.clone(), FileStatus::Modified, SOME_HASH);
        let file = session.get_file_mut(&path).unwrap();
        file.add_file_comment(Comment::new(
            "comment".to_string(),
            CommentType::from_id("note"),
            None,
        ));
        file.add_line_comment(
            10,
            Comment::new("line".to_string(), CommentType::from_id("note"), None),
        );

        let (cleared, _) = session.clear_comments(ClearScope::CommentsAndReviewed);
        assert_eq!(cleared, 2);

        let file = session.files.get(&path).unwrap();
        assert!(file.file_comments.is_empty());
        assert!(file.line_comments.is_empty());
    }

    #[test]
    fn should_reset_reviewed_status_on_all_files() {
        let mut session = test_session();
        let path_a = PathBuf::from("a.rs");
        let path_b = PathBuf::from("b.rs");
        session.add_file(path_a.clone(), FileStatus::Modified, SOME_HASH);
        session.add_file(path_b.clone(), FileStatus::Added, SOME_HASH);

        session.get_file_mut(&path_a).unwrap().reviewed = true;
        session.get_file_mut(&path_b).unwrap().reviewed = true;

        let (cleared, unreviewed) = session.clear_comments(ClearScope::CommentsAndReviewed);
        assert_eq!(cleared, 0);
        assert_eq!(unreviewed, 2);
        assert!(!session.is_file_reviewed(&path_a));
        assert!(!session.is_file_reviewed(&path_b));
    }

    #[test]
    fn should_only_count_reviewed_files_as_unreviewed() {
        let mut session = test_session();
        let reviewed = PathBuf::from("reviewed.rs");
        let pending = PathBuf::from("pending.rs");
        session.add_file(reviewed.clone(), FileStatus::Modified, SOME_HASH);
        session.add_file(pending.clone(), FileStatus::Modified, SOME_HASH);

        session.get_file_mut(&reviewed).unwrap().reviewed = true;

        let (_, unreviewed) = session.clear_comments(ClearScope::CommentsAndReviewed);
        assert_eq!(unreviewed, 1);
    }

    #[test]
    fn should_clear_both_comments_and_reviewed_status() {
        let mut session = test_session();
        let path = PathBuf::from("src/lib.rs");
        session.add_file(path.clone(), FileStatus::Modified, SOME_HASH);
        let file = session.get_file_mut(&path).unwrap();
        file.reviewed = true;
        file.add_file_comment(Comment::new(
            "comment".to_string(),
            CommentType::from_id("note"),
            None,
        ));

        session.review_comments.push(Comment::new(
            "review".to_string(),
            CommentType::from_id("note"),
            None,
        ));

        let (cleared, unreviewed) = session.clear_comments(ClearScope::CommentsAndReviewed);
        assert_eq!(cleared, 2);
        assert_eq!(unreviewed, 1);
        assert!(!session.is_file_reviewed(&path));
    }

    #[test]
    fn should_clear_hunk_reviewed_status() {
        let mut session = test_session();
        let file = test_diff_file("src/main.rs", vec![test_hunk(10, "same")]);
        let path = file.display_path().clone();
        let key = file.hunk_review_key(0).unwrap();

        session.add_diff_file(&file);
        session
            .get_file_mut(&path)
            .unwrap()
            .toggle_hunk_reviewed(key.clone());

        let (cleared, unreviewed) = session.clear_comments(ClearScope::CommentsAndReviewed);

        assert_eq!(cleared, 0);
        assert_eq!(unreviewed, 1);
        assert!(!session.is_hunk_reviewed(&path, &key));
    }

    #[test]
    fn should_preserve_reviewed_status_when_requested() {
        let mut session = test_session();
        let path = PathBuf::from("src/lib.rs");
        session.add_file(path.clone(), FileStatus::Modified, SOME_HASH);
        let file = session.get_file_mut(&path).unwrap();
        file.reviewed = true;
        file.add_file_comment(Comment::new(
            "comment".to_string(),
            CommentType::from_id("note"),
            None,
        ));

        session.review_comments.push(Comment::new(
            "review".to_string(),
            CommentType::from_id("note"),
            None,
        ));

        let (cleared, unreviewed) = session.clear_comments(ClearScope::CommentsOnly);
        assert_eq!(cleared, 2);
        assert_eq!(unreviewed, 0);
        assert!(session.is_file_reviewed(&path));
    }

    #[test]
    fn should_store_content_hash_on_new_file() {
        let mut session = test_session();
        let path = PathBuf::from("new.rs");
        session.add_file(path.clone(), FileStatus::Added, 42);

        let file = session.files.get(&path).unwrap();
        assert_eq!(file.content_hash, Some(42));
        assert!(!file.reviewed);
    }

    #[test]
    fn should_keep_reviewed_when_hash_unchanged() {
        let mut session = test_session();
        let path = PathBuf::from("stable.rs");
        session.add_file(path.clone(), FileStatus::Modified, 100);
        session.get_file_mut(&path).unwrap().reviewed = true;

        let invalidated = session.add_file(path.clone(), FileStatus::Modified, 100);
        assert!(!invalidated);
        assert!(session.is_file_reviewed(&path));
    }

    #[test]
    fn should_reset_reviewed_when_hash_changes() {
        let mut session = test_session();
        let path = PathBuf::from("changed.rs");
        session.add_file(path.clone(), FileStatus::Modified, 100);
        session.get_file_mut(&path).unwrap().reviewed = true;

        let invalidated = session.add_file(path.clone(), FileStatus::Modified, 200);
        assert!(invalidated);
        assert!(!session.is_file_reviewed(&path));
    }

    #[test]
    fn should_not_report_invalidated_for_unreviewed_file_with_changed_hash() {
        let mut session = test_session();
        let path = PathBuf::from("pending.rs");
        session.add_file(path.clone(), FileStatus::Modified, 100);

        let invalidated = session.add_file(path.clone(), FileStatus::Modified, 200);
        assert!(!invalidated);
        assert!(!session.is_file_reviewed(&path));
    }

    #[test]
    fn should_update_hash_even_when_not_reviewed() {
        let mut session = test_session();
        let path = PathBuf::from("evolving.rs");
        session.add_file(path.clone(), FileStatus::Modified, 100);
        session.add_file(path.clone(), FileStatus::Modified, 200);

        let file = session.files.get(&path).unwrap();
        assert_eq!(file.content_hash, Some(200));
    }

    #[test]
    fn should_preserve_reviewed_hunk_when_only_line_numbers_shift() {
        let mut session = test_session();
        let original = test_diff_file("src/main.rs", vec![test_hunk(10, "same")]);
        let path = original.display_path().clone();
        let key = original.hunk_review_key(0).unwrap();

        session.add_diff_file(&original);
        session
            .get_file_mut(&path)
            .unwrap()
            .toggle_hunk_reviewed(key.clone());

        let shifted = test_diff_file("src/main.rs", vec![test_hunk(30, "same")]);
        let shifted_key = shifted.hunk_review_key(0).unwrap();
        session.add_diff_file(&shifted);

        assert_eq!(key, shifted_key);
        assert!(session.is_hunk_reviewed(&path, &shifted_key));
    }

    #[test]
    fn should_use_line_aware_keys_for_repeated_identical_hunks() {
        let mut session = test_session();
        let original = test_diff_file(
            "src/main.rs",
            vec![test_hunk(10, "same"), test_hunk(20, "same")],
        );
        let path = original.display_path().clone();
        let first_key = original.hunk_review_key(0).unwrap();
        let second_key = original.hunk_review_key(1).unwrap();

        session.add_diff_file(&original);
        session
            .get_file_mut(&path)
            .unwrap()
            .toggle_hunk_reviewed(first_key.clone());

        let shifted = test_diff_file(
            "src/main.rs",
            vec![test_hunk(30, "same"), test_hunk(40, "same")],
        );
        session.add_diff_file(&shifted);

        assert_ne!(first_key, second_key);
        assert_ne!(first_key, shifted.hunk_review_key(0).unwrap());
        assert_ne!(second_key, shifted.hunk_review_key(1).unwrap());
        assert!(!session.is_hunk_reviewed(&path, &first_key));
        assert!(!session.is_hunk_reviewed(&path, &second_key));
    }

    #[test]
    fn should_not_move_reviewed_status_between_identical_hunks() {
        let mut session = test_session();
        let original = test_diff_file(
            "src/main.rs",
            vec![
                test_hunk(10, "same"),
                test_hunk(20, "same"),
                test_hunk(30, "same"),
            ],
        );
        let path = original.display_path().clone();
        let first_key = original.hunk_review_key(0).unwrap();
        let second_key = original.hunk_review_key(1).unwrap();
        let third_key = original.hunk_review_key(2).unwrap();

        session.add_diff_file(&original);
        let review = session.get_file_mut(&path).unwrap();
        review.toggle_hunk_reviewed(first_key.clone());
        review.toggle_hunk_reviewed(second_key.clone());

        let updated = test_diff_file(
            "src/main.rs",
            vec![
                test_hunk(10, "same"),
                test_hunk(20, "changed"),
                test_hunk(30, "same"),
            ],
        );
        let updated_first_key = updated.hunk_review_key(0).unwrap();
        let updated_third_key = updated.hunk_review_key(2).unwrap();
        session.add_diff_file(&updated);

        assert_eq!(first_key, updated_first_key);
        assert_eq!(third_key, updated_third_key);
        assert!(session.is_hunk_reviewed(&path, &updated_first_key));
        assert!(!session.is_hunk_reviewed(&path, &updated.hunk_review_key(1).unwrap()));
        assert!(!session.is_hunk_reviewed(&path, &updated_third_key));
    }

    #[test]
    fn should_prune_reviewed_hunks_that_no_longer_exist() {
        let mut session = test_session();
        let original = test_diff_file(
            "src/main.rs",
            vec![test_hunk(10, "kept"), test_hunk(20, "removed")],
        );
        let path = original.display_path().clone();
        let kept_key = original.hunk_review_key(0).unwrap();
        let removed_key = original.hunk_review_key(1).unwrap();

        session.add_diff_file(&original);
        let review = session.get_file_mut(&path).unwrap();
        review.toggle_hunk_reviewed(kept_key.clone());
        review.toggle_hunk_reviewed(removed_key.clone());

        let updated = test_diff_file(
            "src/main.rs",
            vec![test_hunk(10, "kept"), test_hunk(30, "new")],
        );
        session.add_diff_file(&updated);

        assert!(session.is_hunk_reviewed(&path, &kept_key));
        assert!(!session.is_hunk_reviewed(&path, &removed_key));
    }

    #[test]
    fn should_preserve_reviewed_hunks_for_transient_diff_views() {
        let mut session = test_session();
        let full = test_diff_file(
            "src/main.rs",
            vec![test_hunk(10, "first"), test_hunk(20, "second")],
        );
        let path = full.display_path().clone();
        let first_key = full.hunk_review_key(0).unwrap();
        let second_key = full.hunk_review_key(1).unwrap();

        session.add_diff_file(&full);
        let review = session.get_file_mut(&path).unwrap();
        review.toggle_hunk_reviewed(first_key.clone());
        review.toggle_hunk_reviewed(second_key.clone());

        let subset = test_diff_file("src/main.rs", vec![test_hunk(10, "first")]);
        session.add_diff_file_preserving_hunks(&subset);

        assert!(session.is_hunk_reviewed(&path, &first_key));
        assert!(session.is_hunk_reviewed(&path, &second_key));
    }

    #[test]
    fn should_reset_reviewed_when_legacy_session_has_no_hash() {
        let mut session = test_session();
        let path = PathBuf::from("legacy.rs");

        // Simulate a legacy session entry without content_hash.
        session.files.insert(
            path.clone(),
            FileReview {
                path: path.clone(),
                reviewed: true,
                status: FileStatus::Modified,
                file_comments: Vec::new(),
                line_comments: HashMap::new(),
                reviewed_hunks: BTreeSet::new(),
                content_hash: None,
            },
        );

        let invalidated = session.add_file(path.clone(), FileStatus::Modified, 999);
        assert!(invalidated);
        assert!(!session.is_file_reviewed(&path));
        assert_eq!(session.files.get(&path).unwrap().content_hash, Some(999));
    }
}
