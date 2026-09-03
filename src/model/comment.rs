use chrono::{DateTime, Utc};

/// Which side of the diff a line comment belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineSide {
    /// Comment on a deleted line (keyed by old_lineno)
    Old,
    /// Comment on an added or context line (keyed by new_lineno)
    #[default]
    New,
}

/// A range of lines for a comment (inclusive)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    /// Create a new line range
    pub fn new(start: u32, end: u32) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    /// Create a single-line range
    pub fn single(line: u32) -> Self {
        Self {
            start: line,
            end: line,
        }
    }

    /// Check if this is a single-line range
    pub fn is_single(&self) -> bool {
        self.start == self.end
    }

    /// Check if this range contains a given line
    pub fn contains(&self, line: u32) -> bool {
        line >= self.start && line <= self.end
    }
}

/// Lifecycle state of a local comment relative to the remote forge.
///
/// `LocalDraft` is editable in tuicr. `PushedDraft` and `Submitted` are locked:
/// they have been written to GitHub and edits/deletions in tuicr would diverge
/// from the remote. PR 5 introduces the field and the lock check; PR 6 wires
/// the transitions on successful submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CommentLifecycleState {
    #[default]
    LocalDraft,
    PushedDraft,
    Submitted,
}

impl CommentLifecycleState {
    /// True for any state that has already been written to the remote forge.
    pub fn is_locked(self) -> bool {
        !matches!(self, CommentLifecycleState::LocalDraft)
    }
}

/// Classification of a review comment.
///
/// `None` is the out-of-the-box default: a comment with no type. It carries no
/// `[TYPE]` prefix on submit or markdown export and shows no badge in the UI.
/// Any other type is user-defined through the `comment_types` config (see #211)
/// and represented as `Custom`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum CommentType {
    /// No type — the default. Emits no prefix or badge anywhere.
    #[default]
    None,
    /// A user-configured type, identified by its id.
    Custom(String),
}

impl CommentType {
    /// Reserved id for the typeless [`CommentType::None`] variant. A config
    /// entry (or CLI `--type`) of `"none"` — or an empty value — resolves here.
    pub const NONE_ID: &'static str = "none";

    pub fn from_id(id: &str) -> Self {
        let trimmed = id.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(Self::NONE_ID) {
            Self::None
        } else {
            Self::Custom(trimmed.to_string())
        }
    }

    pub fn id(&self) -> &str {
        match self {
            CommentType::None => Self::NONE_ID,
            CommentType::Custom(id) => id.as_str(),
        }
    }

    pub fn as_str(&self) -> String {
        self.id().to_ascii_uppercase()
    }

    /// True for the typeless default. Callers use this to suppress the
    /// `[TYPE]` prefix / badge for untyped comments.
    pub fn is_none(&self) -> bool {
        matches!(self, CommentType::None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineContext {
    pub new_line: Option<u32>,
    pub old_line: Option<u32>,
    pub content: String,
}

/// Default author used when a comment is created or deserialized without
/// an explicit author. Distinguishes human-authored comments from agent /
/// remote comments which set their own author string.
pub const DEFAULT_AUTHOR: &str = "user";

fn default_author() -> String {
    DEFAULT_AUTHOR.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub content: String,
    pub comment_type: CommentType,
    pub created_at: DateTime<Utc>,
    pub line_context: Option<LineContext>,
    /// Which side of the diff this comment belongs to (for line comments)
    /// None for file-level comments.
    pub side: Option<LineSide>,
    /// Line range for multi-line comments (for line comments). None for
    /// file-level comments or single-line comments.
    pub line_range: Option<LineRange>,
    /// Who wrote this comment. Free-form; agents pass `--username "Claude …"`,
    /// humans default to `"user"`.
    pub author: String,
    /// Where this comment sits in its remote forge lifecycle.
    pub lifecycle_state: CommentLifecycleState,
    /// Remote review ID this comment belongs to once submitted/pushed.
    /// `None` while still `LocalDraft`.
    pub remote_review_id: Option<String>,
    /// Remote review-comment ID once GitHub assigns one. Only meaningful for
    /// inline comments; review-level / summary comments don't get one.
    pub remote_comment_id: Option<String>,
    /// The commit SHA this comment was made against, when it was created
    /// while the inline commit selector showed exactly one commit. `None`
    /// for review-level comments and for comments made against the full
    /// cumulative diff (all commits selected). Filtering by the active
    /// commit selection hides comments whose `commit_id` does not
    /// intersect the selection; `None` comments are always shown.
    pub commit_id: Option<String>,
}

impl Comment {
    pub fn new(content: String, comment_type: CommentType, side: Option<LineSide>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            comment_type,
            created_at: Utc::now(),
            line_context: None,
            side,
            line_range: None,
            author: default_author(),
            lifecycle_state: CommentLifecycleState::default(),
            remote_review_id: None,
            remote_comment_id: None,
            commit_id: None,
        }
    }

    /// Create a new comment with a line range
    pub fn new_with_range(
        content: String,
        comment_type: CommentType,
        side: Option<LineSide>,
        line_range: LineRange,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            comment_type,
            created_at: Utc::now(),
            line_context: None,
            side,
            line_range: Some(line_range),
            author: default_author(),
            lifecycle_state: CommentLifecycleState::default(),
            remote_review_id: None,
            remote_comment_id: None,
            commit_id: None,
        }
    }

    /// Builder: set the author and return self. Useful at the boundary where
    /// we know the username (TUI startup config, CLI `--username` flag) so
    /// the existing `Comment::new` call sites can stay untouched.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    /// Builder: set `commit_id` and return self. Called by
    /// `App::save_comment` when the inline commit selector shows exactly
    /// one commit, so the comment is scoped to that commit and hidden when
    /// a different commit (or subset) is selected.
    pub fn with_commit_id(mut self, commit_id: impl Into<String>) -> Self {
        self.commit_id = Some(commit_id.into());
        self
    }

    /// True if this comment has been pushed/submitted to the forge and is
    /// therefore locked from local edits/deletions.
    pub fn is_locked(&self) -> bool {
        self.lifecycle_state.is_locked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod line_range_tests {
        use super::*;

        #[test]
        fn new_creates_range_with_correct_bounds() {
            let range = LineRange::new(10, 20);
            assert_eq!(range.start, 10);
            assert_eq!(range.end, 20);
        }

        #[test]
        fn new_normalizes_reversed_bounds() {
            // When start > end, new() should normalize them
            let range = LineRange::new(20, 10);
            assert_eq!(range.start, 10);
            assert_eq!(range.end, 20);
        }

        #[test]
        fn single_creates_single_line_range() {
            let range = LineRange::single(42);
            assert_eq!(range.start, 42);
            assert_eq!(range.end, 42);
        }

        #[test]
        fn is_single_returns_true_for_single_line() {
            let range = LineRange::single(10);
            assert!(range.is_single());
        }

        #[test]
        fn is_single_returns_false_for_multi_line() {
            let range = LineRange::new(10, 15);
            assert!(!range.is_single());
        }

        #[test]
        fn contains_returns_true_for_start_line() {
            let range = LineRange::new(10, 20);
            assert!(range.contains(10));
        }

        #[test]
        fn contains_returns_true_for_end_line() {
            let range = LineRange::new(10, 20);
            assert!(range.contains(20));
        }

        #[test]
        fn contains_returns_true_for_middle_line() {
            let range = LineRange::new(10, 20);
            assert!(range.contains(15));
        }

        #[test]
        fn contains_returns_false_for_line_before_range() {
            let range = LineRange::new(10, 20);
            assert!(!range.contains(5));
        }

        #[test]
        fn contains_returns_false_for_line_after_range() {
            let range = LineRange::new(10, 20);
            assert!(!range.contains(25));
        }

        #[test]
        fn single_line_range_contains_only_that_line() {
            let range = LineRange::single(42);
            assert!(!range.contains(41));
            assert!(range.contains(42));
            assert!(!range.contains(43));
        }
    }

    mod comment_tests {
        use super::*;

        #[test]
        fn comment_type_defaults_to_none() {
            assert_eq!(CommentType::default(), CommentType::None);
            assert!(CommentType::default().is_none());
        }

        #[test]
        fn comment_type_from_id_maps_none_and_empty_to_none() {
            assert!(CommentType::from_id("none").is_none());
            assert!(CommentType::from_id("NONE").is_none());
            assert!(CommentType::from_id("").is_none());
            assert!(CommentType::from_id("   ").is_none());
            assert!(!CommentType::from_id("issue").is_none());
        }

        #[test]
        fn new_creates_comment_without_line_range() {
            let comment = Comment::new(
                "Test comment".to_string(),
                CommentType::from_id("note"),
                Some(LineSide::New),
            );
            assert!(comment.line_range.is_none());
            assert_eq!(comment.content, "Test comment");
            assert_eq!(comment.comment_type, CommentType::from_id("note"));
            assert_eq!(comment.side, Some(LineSide::New));
        }

        #[test]
        fn new_with_range_creates_comment_with_line_range() {
            let range = LineRange::new(10, 15);
            let comment = Comment::new_with_range(
                "Range comment".to_string(),
                CommentType::from_id("issue"),
                Some(LineSide::Old),
                range,
            );
            assert!(comment.line_range.is_some());
            let stored_range = comment.line_range.unwrap();
            assert_eq!(stored_range.start, 10);
            assert_eq!(stored_range.end, 15);
            assert_eq!(comment.side, Some(LineSide::Old));
        }

        #[test]
        fn should_default_lifecycle_state_to_local_draft_for_new_comment() {
            // given/when
            let comment = Comment::new("hi".to_string(), CommentType::from_id("note"), None);
            // then
            assert_eq!(comment.lifecycle_state, CommentLifecycleState::LocalDraft);
            assert!(!comment.is_locked());
            assert!(comment.remote_review_id.is_none());
            assert!(comment.remote_comment_id.is_none());
        }

        #[test]
        fn should_report_pushed_and_submitted_comments_as_locked() {
            // given
            let mut pushed = Comment::new("p".to_string(), CommentType::from_id("note"), None);
            pushed.lifecycle_state = CommentLifecycleState::PushedDraft;
            let mut submitted = Comment::new("s".to_string(), CommentType::from_id("note"), None);
            submitted.lifecycle_state = CommentLifecycleState::Submitted;
            // then
            assert!(pushed.is_locked());
            assert!(submitted.is_locked());
        }
    }
}
