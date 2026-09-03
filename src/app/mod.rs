use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use ratatui::style::Color;

use crate::comment_vim::CommentVimEditor;
use crate::config::{CommentTypeConfig, ExportConfig};
use crate::editor::{EditorLaunch, EditorTarget};
use crate::error::{Result, TuicrError};
use crate::model::{
    AddCommentRequest, ClearScope, Comment, CommentTarget, CommentType, DiffFile, DiffHunk,
    DiffLine, FileStatus, LineOrigin, LineRange, LineSide, ReviewSession, SessionDiffSource,
    add_comment_to_session,
};
use crate::syntax::{HunkHighlight, HunkStates};
use crate::theme::Theme;
use crate::vcs::git::calculate_gap;
use crate::vcs::traits::VcsType;
use crate::vcs::{
    ChangeKind, CommitInfo, DiffWhitespaceMode, FileBackend, GitBackendPreference,
    ResolvedRevisionRange, RevisionDiffTarget, VcsBackend, VcsChangeStatus, VcsInfo, detect_vcs,
};

const VISIBLE_COMMIT_COUNT: usize = 10;
const COMMIT_PAGE_SIZE: usize = 10;
pub const DEFAULT_DIFF_WATCH_INTERVAL_MS: u64 = 1000;
pub const STAGED_SELECTION_ID: &str = "__tuicr_staged__";
pub const UNSTAGED_SELECTION_ID: &str = "__tuicr_unstaged__";
pub const GAP_EXPAND_BATCH: usize = 20;

fn char_slice(s: &str, lo_char: usize, hi_char: Option<usize>) -> &str {
    let mut indices = s.char_indices();
    let lo_byte = indices
        .by_ref()
        .nth(lo_char)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    let hi_byte = match hi_char {
        None => s.len(),
        Some(hi) if hi <= lo_char => return "",
        Some(hi) => indices
            .nth(hi - lo_char - 1)
            .map(|(b, _)| b)
            .unwrap_or(s.len()),
    };
    &s[lo_byte..hi_byte]
}

fn gap_annotation_line_count(
    is_top_of_file: bool,
    is_end_of_file: bool,
    remaining: usize,
) -> usize {
    if remaining == 0 {
        0
    } else if is_top_of_file {
        // ↑ expander, plus a HiddenLines line when remaining > batch
        if remaining > GAP_EXPAND_BATCH { 2 } else { 1 }
    } else if is_end_of_file {
        // ↓ expander, plus a HiddenLines line when remaining > batch
        if remaining > GAP_EXPAND_BATCH { 2 } else { 1 }
    } else {
        // Between hunks: ↓ + HiddenLines + ↑ when >= batch, else single ↕
        if remaining >= GAP_EXPAND_BATCH { 3 } else { 1 }
    }
}

fn profile_diff_result(result: &Result<Vec<DiffFile>>) -> String {
    match result {
        Ok(files) => format!("files={}", files.len()),
        Err(e) => format!("error={e}"),
    }
}

fn profile_commit_result(result: &Result<Vec<CommitInfo>>) -> String {
    match result {
        Ok(commits) => format!("commits={}", commits.len()),
        Err(e) => format!("error={e}"),
    }
}

fn profile_unit_result(result: &Result<()>) -> String {
    match result {
        Ok(()) => "result=ok".to_string(),
        Err(e) => format!("error={e}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTreeItem {
    Directory {
        path: String,
        depth: usize,
        expanded: bool,
    },
    File {
        file_idx: usize,
        depth: usize,
    },
}

/// Identifies a gap between hunks in a file (for context expansion)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GapId {
    pub file_idx: usize,
    /// Index of the hunk that this gap precedes (0 = gap before first hunk)
    pub hunk_idx: usize,
}

/// Direction of gap expansion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpandDirection {
    /// ↓ Expand downward from upper boundary
    Down,
    /// ↑ Expand upward from lower boundary
    Up,
    /// ↕ Expand all remaining lines in both directions (merged expander)
    Both,
}

/// Minimum line-number column width (covers files up to 9 999 lines).
const MIN_LINENO_WIDTH: usize = 4;

/// Number of characters needed to display `n` in decimal, minimum `MIN_LINENO_WIDTH`.
pub fn lineno_width(max_lineno: u32) -> usize {
    if max_lineno == 0 {
        return MIN_LINENO_WIDTH;
    }
    let mut digits = 0;
    let mut n = max_lineno;
    while n > 0 {
        digits += 1;
        n /= 10;
    }
    digits.max(MIN_LINENO_WIDTH)
}

/// Unified diff gutter: indicator(1) + lineno(w) + space(1) + prefix(1) + space(1).
pub fn unified_gutter(w: usize) -> u16 {
    (w + 4) as u16
}

/// Side-by-side leading width before Old content: indicator(1) + lineno(w) + space(1) + prefix(1).
pub fn sbs_left_gutter(w: usize) -> u16 {
    (w + 3) as u16
}

/// Side-by-side fixed overhead (both gutters + " │ " divider).
/// Left: indicator(1) + lineno(w) + space(1) + prefix(1)
/// Right: lineno(w) + space(1) + prefix(1)
/// Divider: 3
pub fn sbs_overhead(w: usize) -> u16 {
    (2 * w + 8) as u16
}

/// X-coords of one diff content pane. SBS has Old and New; Unified has one.
#[derive(Debug, Clone, Copy)]
pub struct PaneGeom {
    pub content_x_start: u16,
    pub content_x_end: u16,
    pub content_width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelPoint {
    pub annotation_idx: usize,
    pub char_offset: usize,
    pub side: LineSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualSelection {
    pub anchor: SelPoint,
    pub head: SelPoint,
}

impl VisualSelection {
    pub fn collapsed(point: SelPoint) -> Self {
        Self {
            anchor: point,
            head: point,
        }
    }

    pub fn ordered(&self) -> (SelPoint, SelPoint) {
        if (self.anchor.annotation_idx, self.anchor.char_offset)
            <= (self.head.annotation_idx, self.head.char_offset)
        {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Char range `[lo, hi)` of `total_chars` covered by this selection on the
    /// annotation `ann_idx`. Returns `(0, total_chars)` for annotations
    /// strictly between start and end.
    pub fn char_range(&self, ann_idx: usize, total_chars: usize) -> (usize, usize) {
        let (start, end) = self.ordered();
        let lo = if ann_idx == start.annotation_idx {
            start.char_offset.min(total_chars)
        } else {
            0
        };
        let hi = if ann_idx == end.annotation_idx {
            end.char_offset.min(total_chars)
        } else {
            total_chars
        };
        (lo, hi)
    }
}

/// Result of checking what the cursor is on in a gap region
pub enum GapCursorHit {
    /// Cursor is on a directional expander
    Expander(GapId, ExpandDirection),
    /// Cursor is on the "N lines hidden" info line
    HiddenLines(GapId),
    /// Cursor is on already-expanded context
    ExpandedContent(GapId),
}

/// Describes what a rendered line represents - built once and used for O(1) cursor queries
#[derive(Debug, Clone)]
pub enum AnnotatedLine {
    /// Review comments section header line
    ReviewCommentsHeader,
    /// A review-level comment line (part of a multi-line comment box)
    ReviewComment { comment_idx: usize },
    /// File header line
    FileHeader { file_idx: usize },
    /// "Marked reviewed" banner shown in single-file view when the focused
    /// file is reviewed. Both renderers emit this row, so it needs an
    /// annotation slot to keep `line_annotations` index-parallel with them.
    ReviewedBanner { file_idx: usize },
    /// A file-level comment line (part of a multi-line comment box)
    FileComment { file_idx: usize, comment_idx: usize },
    /// Expander line showing hidden context with direction arrow
    Expander {
        gap_id: GapId,
        direction: ExpandDirection,
    },
    /// Informational line showing count of hidden lines between expanders
    HiddenLines { gap_id: GapId, count: usize },
    /// Expanded context line (muted text)
    ExpandedContext { gap_id: GapId, line_idx: usize },
    /// Hunk header (@@...@@)
    HunkHeader { file_idx: usize, hunk_idx: usize },
    /// Actual diff line with line numbers
    DiffLine {
        file_idx: usize,
        hunk_idx: usize,
        line_idx: usize,
        old_lineno: Option<u32>,
        new_lineno: Option<u32>,
    },
    /// Side-by-side paired diff line
    SideBySideLine {
        file_idx: usize,
        hunk_idx: usize,
        del_line_idx: Option<usize>,
        add_line_idx: Option<usize>,
        old_lineno: Option<u32>,
        new_lineno: Option<u32>,
    },
    /// A line comment (part of a multi-line comment box)
    LineComment {
        file_idx: usize,
        line: u32,
        side: LineSide,
        comment_idx: usize,
    },
    /// Binary or empty file indicator
    BinaryOrEmpty { file_idx: usize },
    /// Spacing between files
    Spacing,
}

/// Result of searching for a source line number in annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindSourceLineResult {
    /// Exact match found at the given annotation index.
    Exact(usize),
    /// No exact match; nearest line found at the given annotation index.
    Nearest(usize),
    /// No matching lines found in the current file at all.
    NotFound,
}

/// Best-guess side for an annotation: New for everything except a Side-by-Side
/// line that only has an Old number (a deletion). Mouse cells outside content
/// annotations get New as a harmless default; range-comment line resolution
/// later filters non-diff annotations anyway.
pub fn annotation_side_default(annotation: &AnnotatedLine) -> LineSide {
    match annotation {
        AnnotatedLine::SideBySideLine {
            new_lineno: None,
            old_lineno: Some(_),
            ..
        } => LineSide::Old,
        AnnotatedLine::DiffLine {
            new_lineno: None,
            old_lineno: Some(_),
            ..
        } => LineSide::Old,
        _ => LineSide::New,
    }
}

pub fn annotation_file_idx(annotation: &AnnotatedLine) -> Option<usize> {
    match annotation {
        AnnotatedLine::FileHeader { file_idx }
        | AnnotatedLine::ReviewedBanner { file_idx }
        | AnnotatedLine::FileComment { file_idx, .. }
        | AnnotatedLine::HunkHeader { file_idx, .. }
        | AnnotatedLine::DiffLine { file_idx, .. }
        | AnnotatedLine::SideBySideLine { file_idx, .. }
        | AnnotatedLine::LineComment { file_idx, .. }
        | AnnotatedLine::BinaryOrEmpty { file_idx } => Some(*file_idx),
        AnnotatedLine::ReviewCommentsHeader
        | AnnotatedLine::ReviewComment { .. }
        | AnnotatedLine::Expander { .. }
        | AnnotatedLine::HiddenLines { .. }
        | AnnotatedLine::ExpandedContext { .. }
        | AnnotatedLine::Spacing => None,
    }
}

/// Search `line_annotations` for the annotation whose line number on the given
/// `side` best matches `target_lineno` within the file identified by
/// `current_file`. `side` selects whether to compare against `new_lineno`
/// (post-change) or `old_lineno` (pre-change).
///
/// Test-only entry point that exercises the core matching algorithm against
/// `DiffLine` / `SideBySideLine` annotations. Production code goes through
/// `App::find_source_line_in_diff`, which also resolves `ExpandedContext`
/// lines through `get_expanded_line`.
#[cfg(test)]
pub fn find_source_line(
    annotations: &[AnnotatedLine],
    current_file: usize,
    target_lineno: u32,
    side: LineSide,
) -> FindSourceLineResult {
    let mut best: Option<(usize, u32)> = None; // (index, distance)

    for (idx, annotation) in annotations.iter().enumerate() {
        let (file_idx, old_lineno, new_lineno) = match annotation {
            AnnotatedLine::DiffLine {
                file_idx,
                old_lineno,
                new_lineno,
                ..
            } => (*file_idx, *old_lineno, *new_lineno),
            AnnotatedLine::SideBySideLine {
                file_idx,
                old_lineno,
                new_lineno,
                ..
            } => (*file_idx, *old_lineno, *new_lineno),
            _ => continue,
        };
        if file_idx != current_file {
            continue;
        }
        let candidate = match side {
            LineSide::New => new_lineno,
            LineSide::Old => old_lineno,
        };
        if let Some(ln) = candidate {
            let dist = ln.abs_diff(target_lineno);
            if dist == 0 {
                return FindSourceLineResult::Exact(idx);
            }
            if best.is_none() || dist < best.unwrap().1 {
                best = Some((idx, dist));
            }
        }
    }

    match best {
        Some((idx, _)) => FindSourceLineResult::Nearest(idx),
        None => FindSourceLineResult::NotFound,
    }
}

/// True for rendered lines the cursor should never rest on — spacing between
/// files and file header rows.
fn is_decoration(annotation: &AnnotatedLine) -> bool {
    matches!(
        annotation,
        AnnotatedLine::Spacing
            | AnnotatedLine::FileHeader { .. }
            | AnnotatedLine::ReviewedBanner { .. }
    )
}

/// Walk `start` forward (capped at `max_line`) to the nearest non-decoration
/// annotation so scroll and jump motions land on actionable content.
fn skip_decoration_forward(annotations: &[AnnotatedLine], start: usize, max_line: usize) -> usize {
    let mut line = start;
    while line < max_line && annotations.get(line).is_some_and(is_decoration) {
        line += 1;
    }
    line
}

/// Walk `start` backward to the nearest non-decoration annotation.
fn skip_decoration_backward(annotations: &[AnnotatedLine], start: usize) -> usize {
    let mut line = start;
    while line > 0 && annotations.get(line).is_some_and(is_decoration) {
        line -= 1;
    }
    line
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Comment,
    Command,
    Search,
    Help,
    /// Scrollable full-screen view for the complete current error message.
    MessageDetails,
    /// View of the active review's pending local-draft comments.
    Summary,
    Confirm,
    CommitSelect,
    VisualSelect,
}

/// CommandCompletionState keeps one Tab-completion run anchored to the text
/// the user typed before cycling began.
///
/// Without this state, repeated Tab presses would re-scan from the currently
/// displayed candidate and narrow the cycle to a different match set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCompletionState {
    /// Prefix used to build `matches`.
    pub(crate) prefix: String,
    /// Matching command strings in the order they should cycle.
    pub(crate) matches: Vec<&'static str>,
    /// Index of the command currently displayed in the command buffer.
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSource {
    WorkingTree,
    Staged,
    Unstaged,
    StagedAndUnstaged,
    CommitRange(Vec<String>),
    StagedUnstagedAndCommits(Vec<String>),
}

impl DiffSource {
    /// Returns true when the active review target includes live worktree changes.
    ///
    /// This marks diff sources where reloading after an external editor exits
    /// can surface newly written worktree edits. Pure staged, commit-range,
    /// and pull-request reviews intentionally return false because editing the
    /// local file does not update the selected review target.
    pub fn includes_worktree_changes(&self) -> bool {
        matches!(
            self,
            Self::WorkingTree
                | Self::Unstaged
                | Self::StagedAndUnstaged
                | Self::StagedUnstagedAndCommits(_)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    CopyAndQuit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPanel {
    FileList,
    Comments,
    Diff,
    CommitSelector,
}

/// Active tab in the review target selector.
///
/// The selector internally still goes through `InputMode::CommitSelect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetTab {
    Local,
}

/// Identity snapshot for an in-flight diff-watch reload, captured when the
/// fetch is spawned. Compared against the live `App` state when the result
/// lands so a fetch that outlives a diff-source switch, a commit-selection
/// change, or a mode change (see `apply_diff_files`'s invariant comment) is
/// discarded instead of applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffWatchReloadRequest {
    pub diff_source: DiffSource,
    pub commit_selection_range: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffWatchTarget {
    Review,
    LocalSelector,
}

/// Result delivered from the diff-watch background thread. `Ok(None)` means
/// the diff is unchanged. `Ok(Some(_))` carries a fetched diff ready for
/// `apply_diff_files`, including an empty diff when all changes disappear.
#[derive(Debug)]
pub enum DiffWatchReloadEvent {
    Done {
        request: DiffWatchReloadRequest,
        result: std::result::Result<Option<Vec<DiffFile>>, String>,
        /// Commits re-read during the same tick, so a commit written while
        /// tuicr is open reaches the inline pane. `None` means the worker did
        /// not ask: either a narrowing was active when it spawned, or the
        /// backend does not list commits.
        commits: Option<Vec<CommitInfo>>,
        /// Whether each of staged and unstaged holds anything, read during the
        /// same tick. Decides which of the two synthetic rows the pane should
        /// carry. `None` means the backend could not say, and the rows are
        /// left exactly as they are.
        change_status: Option<VcsChangeStatus>,
    },
}

/// An in-flight diff-watch reload: the channel the worker will answer on,
/// paired with the snapshot of what the user was looking at when it was
/// spawned. Held together so neither can exist without the other.
#[derive(Debug)]
pub struct DiffWatchReload {
    pub request: DiffWatchReloadRequest,
    pub target: DiffWatchTarget,
    pub commit_limit: usize,
    pub rx: std::sync::mpsc::Receiver<DiffWatchReloadEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffViewMode {
    Unified,
    #[default]
    SideBySide,
}

/// Display order for the inline commit selector. The stored `review_commits`
/// list is always newest-first; this only flips presentation (render + input
/// mapping), never the underlying data model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommitOrder {
    /// Newest commit at the top (the historical default).
    #[default]
    Descending,
    /// Oldest commit at the top.
    Ascending,
}

/// Which commits are selected when a multi-commit review first opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommitSelectionStart {
    /// Select the whole range (the historical default).
    #[default]
    All,
    /// Select only the oldest commit, for a walk-forward per-commit review.
    Oldest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageType {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub content: String,
    pub message_type: MessageType,
    /// When this message should be auto-cleared. `None` means sticky.
    pub expires_at: Option<Instant>,
}

const MESSAGE_TTL_INFO: Duration = Duration::from_secs(3);
const MESSAGE_TTL_WARNING: Duration = Duration::from_secs(5);

/// Pending "press again to confirm" state for the vim comment box. A first
/// plain `Enter`/`Esc` in Normal mode arms `Save`/`Cancel` and shows a header
/// hint; a second consecutive press performs it. Any other key resets to `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommentVimPending {
    #[default]
    None,
    Save,
    Cancel,
}

pub struct App {
    pub theme: Theme,
    pub vcs: Box<dyn VcsBackend>,
    pub vcs_info: VcsInfo,
    pub session: ReviewSession,
    /// A `0` interval in config disables automatic local refreshes.
    pub diff_watch_interval: Option<Duration>,
    pub next_diff_watch_at: Instant,
    /// Last diff-watch error text, so a sustained failure warns once instead of
    /// once per tick. Cleared on the next successful fetch.
    last_diff_watch_error: Option<String>,
    /// In-flight diff-watch reload spawned by a tick. Guards against a second
    /// tick spawning while one is already running, and carries the identity
    /// snapshot checked on receipt. The channel and the snapshot live in one
    /// value on purpose: as two independent `Option`s they could disagree,
    /// and a snapshot left behind without its channel blocks every future
    /// tick from ever spawning again.
    pub diff_watch_reload: Option<DiffWatchReload>,
    /// Everything `detect_vcs` needs to open a backend the same way the
    /// startup one was opened. The diff-watch worker opens its own, because
    /// `git2::Repository` is `Send` but not `Sync` and so cannot share
    /// `App::vcs`, and `AppStartupOptions` is dropped by the end of
    /// `App::new`. Kept as one value so a future setting is added in one
    /// place rather than at every construction path.
    vcs_open_options: VcsOpenOptions,
    pub diff_files: Vec<DiffFile>,
    pub diff_source: DiffSource,
    pub pending_editor_target: Option<EditorTarget>,
    /// Windowed editors that have not exited yet; polled by
    /// `poll_editor_launches`.
    pub(crate) editor_launches: Vec<EditorLaunch>,

    pub input_mode: InputMode,
    pub focused_panel: FocusedPanel,
    pub diff_view_mode: DiffViewMode,
    pub relative_line_numbers: bool,
    /// Which side the cursor targets in side-by-side view (old/left vs
    /// new/right). Drives the `▶` caret placement and the side a new line
    /// comment attaches to. Ignored in unified view. Defaults to `New`.
    pub cursor_side: LineSide,

    pub file_list_state: FileListState,
    pub comment_navigator_state: CommentNavigatorState,
    pub diff_state: DiffState,
    pub help_state: HelpState,
    pub summary_state: SummaryState,
    /// File-tree include/exclude filters and `/` search.
    pub file_filter: FileTreeFilter,
    pub command_buffer: String,
    pub(crate) command_completion: Option<CommandCompletionState>,
    pub(crate) command_return_mode: InputMode,
    pub search_buffer: String,
    pub last_search_pattern: Option<String>,
    pub(crate) search_needle_lower: Option<String>,
    pub(crate) search_matches: Vec<usize>,
    pub(crate) search_matches_stale: bool,
    pub(crate) search_highlight_visible: bool,
    pub search_highlight_enabled: bool,
    pub(crate) search_return_mode: InputMode,
    pub(crate) overlay_return_mode: InputMode,
    pub comment_buffer: String,
    pub comment_cursor: usize,
    /// Config `comment_vim`: vim modal editing in the comment box.
    pub comment_vim_enabled: bool,
    /// Spaces inserted by Tab while typing in the vim comment box (config
    /// `comment_tab_width`, default 4).
    pub comment_tab_width: usize,
    /// Active vim overlay (only while in comment mode with vim on); synced into
    /// `comment_buffer`/`comment_cursor`, which stay canonical for rendering.
    pub comment_vim_editor: Option<CommentVimEditor>,
    /// In-progress `:` command-line in vim Normal mode (without the leading
    /// `:`); `Some("")` right after `:` is pressed. `:w` saves, `:q` cancels.
    pub comment_vim_command: Option<String>,
    /// Pending double-press confirm in vim Normal mode: a first plain
    /// `Enter`/`Esc` arms Save/Cancel (with a header hint), the second performs
    /// it (`:w`/`:q`). Any other key resets it.
    pub comment_vim_pending: CommentVimPending,
    pub comment_type: CommentType,
    pub comment_types: Vec<CommentTypeDefinition>,
    pub comment_is_review_level: bool,
    pub comment_is_file_level: bool,
    pub comment_line: Option<(u32, LineSide)>,
    pub editing_comment_id: Option<String>,

    pub visual_selection: Option<VisualSelection>,
    /// True once the active mouse drag has actually moved off the press cell.
    /// Lets Up distinguish click from drag-back-to-anchor.
    pub mouse_drag_active: bool,
    /// Line range for range comments (used when creating comments from visual selection)
    pub comment_line_range: Option<(LineRange, LineSide)>,

    // Commit selection state
    pub commit_list: Vec<CommitInfo>,
    pub commit_list_cursor: usize,
    pub commit_list_scroll_offset: usize,
    pub commit_list_viewport_height: usize,
    /// Selected commit range as (start_idx, end_idx) inclusive, where start <= end.
    /// Indices refer to positions in commit_list.
    pub commit_selection_range: Option<(usize, usize)>,
    /// State describing how many commits are currently shown and how pagination behaves.
    pub visible_commit_count: usize,
    pub commit_page_size: usize,
    pub has_more_commit: bool,

    // Review target selector tab state.
    pub target_tab: TargetTab,
    /// Local viewer identity. Stamped on new comments authored in the TUI,
    /// and compared against existing comment authors so the comment pane can
    /// distinguish "your" comments from others. Resolved from the config
    /// `username` field; defaults to `Comment::DEFAULT_AUTHOR`.
    pub username: String,

    pub should_quit: bool,
    pub dirty: bool,
    pub quit_warned: bool,
    pub message: Option<Message>,
    pub pending_confirm: Option<ConfirmAction>,
    pub supports_keyboard_enhancement: bool,
    pub show_file_list: bool,
    /// `true` when the session was opened via `--all-files`. Drives the
    /// `PRISTINE · N files` chip in the status bar and prevents that chip
    /// from showing in the regular `--file <dir>` directory mode.
    pub is_pristine_mode: bool,
    /// `true` when single-file view is active. Renders only the currently
    /// focused file in the diff panel instead of the continuous-scroll
    /// concatenation. Toggled via `:focus` or `<leader>f`.
    pub is_single_file_view: bool,
    /// A reviewed file whose body is temporarily expanded after opening a
    /// comment from the summary view. The persisted reviewed marker is left
    /// untouched; this is only a presentation override for continuous view.
    pub revealed_reviewed_file: Option<PathBuf>,
    /// A reviewed hunk whose body is temporarily expanded after opening a
    /// comment from the summary view. The persisted reviewed marker is left
    /// untouched; this is only a presentation override.
    pub revealed_reviewed_hunk: Option<(PathBuf, String)>,
    /// Set when `j` (or down arrow) tries to overflow past the last line
    /// of the current file in single-file view. The first overflow press
    /// arms the flag and parks the cursor on max; a deliberate second
    /// press then walks to the next file. Reset by any cursor move that
    /// isn't a continuing overflow attempt.
    pub primed_walk_next: bool,
    /// Symmetric inverse of [`primed_walk_next`]: armed by an underflow
    /// `k` press at the first line of the current file in single-file
    /// view; consumed by a second underflow press to walk to the
    /// previous file.
    pub primed_walk_prev: bool,
    /// Set when the Down arrow / `j` key is released after
    /// `primed_walk_next` was armed. The walk consumes only when both
    /// flags are true so held-key auto-repeat (Press, Repeat, Repeat...)
    /// never satisfies the gate. Only meaningful on terminals that
    /// support kitty `REPORT_EVENT_TYPES`; on others Release events are
    /// never emitted and the gate is bypassed via
    /// `supports_keyboard_enhancement`.
    pub down_released_since_arm: bool,
    /// Symmetric inverse of [`down_released_since_arm`] for the prev-file
    /// walk gate.
    pub up_released_since_arm: bool,
    pub cursor_line_highlight: bool,
    pub leader_key: char,
    pub scroll_offset: usize,
    pub file_list_area: Option<ratatui::layout::Rect>,
    pub comment_navigator_area: Option<ratatui::layout::Rect>,
    pub diff_area: Option<ratatui::layout::Rect>,
    /// Inner content rect of the file list panel; populated during render.
    pub file_list_inner_area: Option<ratatui::layout::Rect>,
    /// Inner content rect of the comment navigator panel; populated during render.
    pub comment_navigator_inner_area: Option<ratatui::layout::Rect>,
    /// Inner content rect of the diff panel; populated during render.
    pub diff_inner_area: Option<ratatui::layout::Rect>,
    /// Inner content rect of the commit list panel (full-screen picker or inline selector);
    /// populated during render.
    pub commit_list_inner_area: Option<ratatui::layout::Rect>,
    /// Visual-row -> annotation-index map for the diff viewport. Wrapped
    /// logical lines repeat their annotation index across multiple rows.
    pub diff_row_to_annotation: Vec<usize>,
    pub expanded_dirs: HashSet<String>,
    /// Stores lines expanded downward from the upper boundary of each gap
    pub expanded_top: HashMap<GapId, Vec<DiffLine>>,
    /// Stores lines expanded upward from the lower boundary of each gap (in ascending line order)
    pub expanded_bottom: HashMap<GapId, Vec<DiffLine>>,
    /// Cached file line counts (keyed by file_idx) to avoid repeated disk reads
    pub file_line_count_cache: HashMap<usize, u32>,
    /// Parser states of hunks partway through lazy syntax highlighting.
    pub(crate) highlight_states: HunkStates,
    /// Set by the diff view when it ran out of highlighting budget before
    /// every visible row had its spans; the event loop redraws to continue.
    pub highlight_pending: bool,
    /// Cached annotations describing what each rendered line represents
    pub line_annotations: Vec<AnnotatedLine>,
    /// Output to stdout instead of clipboard when exporting
    pub output_to_stdout: bool,
    /// Pending output to print to stdout after TUI exits
    pub pending_stdout_output: Option<String>,
    /// Calculated screen position for comment input cursor (col, row) for IME positioning.
    /// Set during render when in Comment mode, None otherwise.
    pub comment_cursor_screen_pos: Option<(u16, u16)>,
    /// During render, the comment input box may introduce lines that have no corresponding
    /// entry in `line_annotations`. This field stores `(box_start, box_len, annotations_replaced)`
    /// where `box_start` is the absolute rendered line index where the input box begins,
    /// `box_len` is the number of rendered lines the input box occupies, and
    /// `annotations_replaced` is how many annotation entries exist for the comment being
    /// edited (0 for a new comment). Used by `is_line_highlighted` to adjust annotation lookups.
    pub comment_input_annotation_offset: Option<(usize, usize, usize)>,
    /// Accumulated digit count for {N}G jump-to-line
    pub pending_count: Option<usize>,

    // Inline commit selector state (shown at top of diff view for multi-commit reviews)
    /// CommitInfo for commits in the current review (display order: newest first)
    pub review_commits: Vec<CommitInfo>,
    /// Whether the inline commit selector panel is visible
    pub show_commit_selector: bool,
    /// Display order for the inline commit selector (presentation only).
    pub commit_order: CommitOrder,
    /// Which commits are selected when a multi-commit review first opens.
    pub commit_selection_start: CommitSelectionStart,
    /// Cached individual/subrange diffs keyed by (start_idx, end_idx) into review_commits
    pub commit_diff_cache: HashMap<(usize, usize), Vec<DiffFile>>,
    /// The combined "all selected" diff, cached for quick restoration
    pub range_diff_files: Option<Vec<DiffFile>>,
    /// Saved inline selection range when entering full commit select mode via :commits
    pub saved_inline_selection: Option<(usize, usize)>,
    /// Path filter for scoping diff to a specific file or directory
    pub path_filter: Option<String>,
    /// Resolved `[export]` settings shaping the generated review markdown.
    pub export: ExportConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentTypeDefinition {
    pub id: String,
    pub label: String,
    pub definition: Option<String>,
    pub color: Option<Color>,
}

#[derive(Default)]
pub struct FileListState {
    pub list_state: ratatui::widgets::ListState,
    pub scroll_x: usize,
    pub viewport_width: usize,    // Set during render
    pub viewport_height: usize,   // Set during render
    pub max_content_width: usize, // Set during render
}

impl FileListState {
    pub fn selected(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    pub fn select(&mut self, index: usize) {
        self.list_state.select(Some(index));
    }

    pub fn scroll_left(&mut self, cols: usize) {
        self.scroll_x = self.scroll_x.saturating_sub(cols);
    }

    pub fn scroll_right(&mut self, cols: usize) {
        let max_scroll_x = self.max_content_width.saturating_sub(self.viewport_width);
        self.scroll_x = (self.scroll_x.saturating_add(cols)).min(max_scroll_x);
    }
}

#[derive(Default)]
pub struct CommentNavigatorState {
    pub list_state: ratatui::widgets::ListState,
    pub scroll_x: usize,
    pub viewport_width: usize,    // Set during render
    pub viewport_height: usize,   // Set during render
    pub max_content_width: usize, // Set during render
}

impl CommentNavigatorState {
    pub fn selected(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    pub fn select(&mut self, index: usize) {
        self.list_state.select(Some(index));
    }

    pub fn scroll_left(&mut self, cols: usize) {
        self.scroll_x = self.scroll_x.saturating_sub(cols);
    }

    pub fn scroll_right(&mut self, cols: usize) {
        let max_scroll_x = self.max_content_width.saturating_sub(self.viewport_width);
        self.scroll_x = (self.scroll_x.saturating_add(cols)).min(max_scroll_x);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentNavigatorKey {
    Review {
        comment_idx: usize,
    },
    File {
        file_idx: usize,
        comment_idx: usize,
    },
    Line {
        file_idx: usize,
        line: u32,
        side: LineSide,
        comment_idx: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentNavigatorKind {
    Local(CommentType),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentNavigatorItem {
    pub key: CommentNavigatorKey,
    pub kind: CommentNavigatorKind,
    pub target_annotation: usize,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub side: Option<LineSide>,
    /// Author of the underlying comment.
    pub author: Option<String>,
}

#[derive(Debug)]
pub struct DiffState {
    pub scroll_offset: usize,
    pub scroll_x: usize,
    pub cursor_line: usize,
    pub current_file_idx: usize,
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub max_content_width: usize,
    pub wrap_lines: bool,
    /// Number of logical lines that fit in the viewport (set during render).
    /// When wrapping is enabled, this accounts for lines expanding to multiple visual rows.
    pub visible_line_count: usize,
}

impl DiffState {
    /// Number of logical lines that fit in the viewport. Uses the render-computed
    /// `visible_line_count` (which accounts for line wrapping), falling back to
    /// `viewport_height` before the first render.
    pub fn effective_visible_lines(&self) -> usize {
        if self.visible_line_count > 0 {
            self.visible_line_count
        } else {
            self.viewport_height.max(1)
        }
    }

    /// Minimum number of lines kept between the cursor and the viewport edge
    /// (equivalent to vim's `scrolloff`). Must be strictly less than half the
    /// viewport to guarantee a stable free zone after centering (zz).
    pub fn effective_scroll_margin(&self, scroll_offset: usize) -> usize {
        scroll_offset.min((self.effective_visible_lines() / 2).saturating_sub(1))
    }
}

impl Default for DiffState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            scroll_x: 0,
            cursor_line: 0,
            current_file_idx: 0,
            viewport_height: 0,
            viewport_width: 0,
            max_content_width: 0,
            wrap_lines: true,
            visible_line_count: 0,
        }
    }
}

/// Which file-tree prompt is currently collecting input. All three share
/// one draft buffer because only one can be open at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTreePrompt {
    /// `i` — keep only files whose path matches (regex).
    Include,
    /// `e` — drop files whose path matches (regex).
    Exclude,
    /// `/` — jump the tree selection to a matching file (substring).
    Search,
}

impl FileTreePrompt {
    /// Prefix shown before the buffer in the prompt line, mirroring the
    /// key that opened it.
    pub fn sigil(self) -> char {
        match self {
            FileTreePrompt::Include => 'i',
            FileTreePrompt::Exclude => 'e',
            FileTreePrompt::Search => '/',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FileTreePrompt::Include => "include",
            FileTreePrompt::Exclude => "exclude",
            FileTreePrompt::Search => "search",
        }
    }
}

/// An in-progress prompt. `Some` in `FileTreeFilter::draft` makes the file
/// tree a text-input sub-state of `InputMode::Normal`, the same way
/// `pr_filter_draft` does for the target selector.
#[derive(Debug, Clone)]
pub struct FileTreeDraft {
    pub prompt: FileTreePrompt,
    pub buffer: String,
}

/// An applied regex filter plus the pattern the user typed, kept so the
/// prompt can be reopened pre-seeded and the UI can echo the source.
pub struct FilePattern {
    pub source: String,
    pub regex: regex::Regex,
}

/// Include/exclude/search state for the file tree. Filters narrow both the
/// tree and the diff pane (see `App::file_passes_filter`); search only moves
/// the tree selection.
pub struct FileTreeFilter {
    pub include: Option<FilePattern>,
    pub exclude: Option<FilePattern>,
    /// Applied `/` query. Persists after the prompt closes so `n`/`N` can
    /// keep stepping matches.
    pub search: Option<String>,
    pub draft: Option<FileTreeDraft>,
    /// False hides files marked reviewed from the tree and the diff (`H`,
    /// `:set noreviewed`, config `show_reviewed`).
    pub show_reviewed: bool,
}

impl Default for FileTreeFilter {
    /// Hand-written because `show_reviewed` defaults to *true*: a derived
    /// `bool` default would silently boot with reviewed files hidden.
    fn default() -> Self {
        Self {
            include: None,
            exclude: None,
            search: None,
            draft: None,
            show_reviewed: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct HelpState {
    pub scroll_offset: usize,
    pub horizontal_offset: usize,
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub total_lines: usize,    // Set during render
    pub max_line_width: usize, // Set during render
    pub(crate) searchable_lines: Vec<String>,
    pub(crate) last_search_pattern: Option<String>,
    pub(crate) current_match_line: Option<usize>,
}

impl HelpState {
    /// Furthest left column the popup can be panned to, so the widest help
    /// line's tail can still reach the viewport.
    pub(crate) fn max_horizontal_offset(&self) -> usize {
        self.max_line_width.saturating_sub(self.viewport_width)
    }

    pub(crate) fn scroll_right(&mut self, columns: usize) {
        self.horizontal_offset =
            (self.horizontal_offset + columns).min(self.max_horizontal_offset());
    }

    pub(crate) fn scroll_left(&mut self, columns: usize) {
        self.horizontal_offset = self.horizontal_offset.saturating_sub(columns);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryCommentTarget {
    Review {
        comment_id: String,
    },
    File {
        path: PathBuf,
        comment_id: String,
    },
    Line {
        path: PathBuf,
        line: u32,
        side: LineSide,
        comment_id: String,
    },
}

#[derive(Debug, Default)]
pub struct SummaryState {
    pub selected_comment: usize,
    pub scroll_offset: usize,
    pub viewport_height: usize,
    pub total_lines: usize, // Set during render
    /// Exclusive rendered-line ranges for each pending comment.
    pub comment_ranges: Vec<(usize, usize)>,
    /// Stable jump targets in the same order as `comment_ranges`.
    /// A target is absent when the comment is hidden by the current diff,
    /// commit selection, or file-tree filters.
    pub targets: Vec<Option<SummaryCommentTarget>>,
    pub(crate) selection_needs_scroll: bool,
}

/// Represents a comment location for deletion
enum CommentLocation {
    Review {
        index: usize,
    },
    File {
        path: std::path::PathBuf,
        index: usize,
    },
    Line {
        path: std::path::PathBuf,
        line: u32,
        side: LineSide,
        index: usize,
    },
}

/// What `detect_vcs` needs to open a backend. Bundled because these two
/// always travel together: they are chosen once at startup and then replayed
/// verbatim by the diff-watch worker when it opens its own backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VcsOpenOptions {
    git_backend_preference: GitBackendPreference,
    diff_whitespace_mode: DiffWhitespaceMode,
}

impl Default for VcsOpenOptions {
    /// What every non-Git start uses: `--file`, `--all-files`, and PR reviews
    /// never reopen a backend, so their options are never read.
    fn default() -> Self {
        Self {
            git_backend_preference: GitBackendPreference::Libgit2,
            diff_whitespace_mode: DiffWhitespaceMode::default(),
        }
    }
}

pub struct AppStartupOptions<'a> {
    pub revisions: Option<&'a str>,
    pub working_tree: bool,
    pub path_filter: Option<&'a str>,
    pub file_path: Option<&'a str>,
    /// Whole-repo annotation mode (`--all-files`). Mutually exclusive with
    /// the other selectors; the binary validates that before reaching here.
    pub all_files: bool,
    pub git_backend_preference: GitBackendPreference,
    pub diff_whitespace_mode: DiffWhitespaceMode,
    /// Which commits are selected when a multi-commit review first opens.
    pub commit_selection: CommitSelectionStart,
}

impl AppStartupOptions<'_> {
    /// The subset `detect_vcs` needs, so an `App` can reopen a backend later
    /// without holding on to the whole options struct.
    fn vcs_open_options(&self) -> VcsOpenOptions {
        VcsOpenOptions {
            git_backend_preference: self.git_backend_preference,
            diff_whitespace_mode: self.diff_whitespace_mode,
        }
    }
}

mod annotations;
mod comment_vim;
mod comments;
mod commits;
mod diff_load;
mod file_filter;
mod gaps;
mod highlight;
mod init;
mod modes;
mod navigation;
mod reviewed;
mod search;
mod session;
mod tree;
mod visual;

#[cfg(test)]
mod tests;
