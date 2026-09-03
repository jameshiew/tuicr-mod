use ratatui::style::Style;
use std::{collections::HashMap, path::PathBuf};

use crate::hash::Fnv1aHasher;
use crate::model::comment::LineSide;
use crate::syntax::HunkHighlight;

/// Backend-provided file metadata paired with that file's opaque patch text.
///
/// Paths and status come from a machine-readable VCS or forge channel; the
/// patch is used only to materialize hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub status: FileStatus,
    pub patch: String,
    pub is_binary: bool,
    pub is_too_large: bool,
}

impl FilePatch {
    pub fn new(
        old_path: Option<PathBuf>,
        new_path: Option<PathBuf>,
        status: FileStatus,
        patch: impl Into<String>,
    ) -> Self {
        Self {
            old_path,
            new_path,
            status,
            patch: patch.into(),
            is_binary: false,
            is_too_large: false,
        }
    }

    pub fn display_path(&self) -> Option<&std::path::Path> {
        self.new_path.as_deref().or(self.old_path.as_deref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

impl FileStatus {
    pub fn as_char(&self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOrigin {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub origin: LineOrigin,
    pub content: String,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    /// Syntax-highlighted spans for this line, filled in lazily as the line
    /// comes into view (see `DiffHunk::highlight`). `None` until then, or
    /// when no grammar matched, in which case the default diff coloring is used.
    pub highlighted_spans: Option<Vec<(Style, String)>>,
}

#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
    /// Starting line number in the old file (from @@ header)
    #[allow(dead_code)]
    pub old_start: u32,
    /// Number of lines from the old file in this hunk
    #[allow(dead_code)]
    pub old_count: u32,
    /// Starting line number in the new file (from @@ header)
    pub new_start: u32,
    /// Number of lines from the new file in this hunk
    pub new_count: u32,
    /// How far syntax highlighting has progressed through `lines`.
    pub highlight: HunkHighlight,
}

/// Whole-file text on each side of a diff, kept only for grammars that need
/// the full file in scope (see `syntax::needs_full_file_highlight`). The
/// lazy highlighter consumes and drops it the first time the file is shown.
#[derive(Debug, Clone, Default)]
pub struct WholeFileText {
    pub old: Option<String>,
    pub new: Option<String>,
}

impl WholeFileText {
    /// Files larger than this skip whole-file highlighting and stay plain.
    /// Keeps a runaway-cost ceiling on diffs that include huge generated
    /// artefacts (lockfiles, vendored bundles, fixtures).
    pub const MAX_BYTES: usize = 1024 * 1024;

    /// Keep `content` only when it is small enough and not binary.
    pub fn accept(content: Option<String>) -> Option<String> {
        content.filter(|c| c.len() <= Self::MAX_BYTES && !c.as_bytes().contains(&0u8))
    }

    /// `None` when neither side is available, so nothing is retained.
    pub fn from_sides(old: Option<String>, new: Option<String>) -> Option<Self> {
        let old = Self::accept(old);
        let new = Self::accept(new);
        (old.is_some() || new.is_some()).then_some(Self { old, new })
    }
}

#[derive(Debug, Clone)]
pub struct DiffFile {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub status: FileStatus,
    pub hunks: Vec<DiffHunk>,
    pub is_binary: bool,
    pub is_too_large: bool,
    pub is_commit_message: bool,
    pub content_hash: u64,
    /// Present only for container-grammar files, until first highlighted.
    pub whole_file_text: Option<WholeFileText>,
}

impl DiffHunk {
    fn review_content_hash(&self) -> u64 {
        let mut hasher = Fnv1aHasher::new();
        write_hunk_content_hash(&mut hasher, &self.lines);
        hasher.finish()
    }
}

impl DiffFile {
    /// Stable key for a reviewed hunk.
    ///
    /// Unique hunk content ignores hunk header line numbers so unrelated
    /// edits above a hunk do not clear its reviewed state. Repeated identical
    /// hunks fall back to a line-aware key because a pure occurrence count can
    /// move reviewed state onto a different hunk when one duplicate changes.
    pub fn hunk_review_key(&self, hunk_idx: usize) -> Option<String> {
        let hash_counts = self.hunk_content_hash_counts();
        self.hunks
            .get(hunk_idx)
            .map(|hunk| self.hunk_review_key_with_counts(hunk, &hash_counts))
    }

    pub fn hunk_review_keys(&self) -> Vec<String> {
        let hash_counts = self.hunk_content_hash_counts();
        self.hunks
            .iter()
            .map(|hunk| self.hunk_review_key_with_counts(hunk, &hash_counts))
            .collect()
    }

    fn hunk_content_hash_counts(&self) -> HashMap<u64, usize> {
        let mut hash_counts = HashMap::new();
        for hunk in &self.hunks {
            *hash_counts.entry(hunk.review_content_hash()).or_insert(0) += 1;
        }
        hash_counts
    }

    fn hunk_review_key_with_counts(
        &self,
        hunk: &DiffHunk,
        hash_counts: &HashMap<u64, usize>,
    ) -> String {
        let hash = hunk.review_content_hash();
        if hash_counts.get(&hash).copied().unwrap_or_default() > 1 {
            format_hunk_review_span_key(hunk, hash)
        } else {
            format_hunk_review_content_key(hash)
        }
    }

    /// Computes a hash of the diff content (all hunk line contents) for change detection.
    pub fn compute_content_hash(hunks: &[DiffHunk]) -> u64 {
        let mut hasher = Fnv1aHasher::new();
        for hunk in hunks {
            write_hunk_content_hash(&mut hasher, &hunk.lines);
        }
        hasher.finish()
    }

    /// Highest line number reachable from hunk headers (old or new side).
    /// Used to size the gutter; expanded context beyond hunks is covered
    /// separately via `file_line_count_cache`.
    pub fn max_lineno(&self) -> u32 {
        self.hunks
            .iter()
            .map(|h| (h.old_start + h.old_count).max(h.new_start + h.new_count))
            .max()
            .unwrap_or(0)
    }

    pub fn display_path(&self) -> &PathBuf {
        self.new_path
            .as_ref()
            .or(self.old_path.as_ref())
            .expect("DiffFile must have at least one path")
    }

    /// First line number in display order that carries a value on `side`.
    ///
    /// On `LineSide::New`, returns the first context or addition line; on
    /// `LineSide::Old`, the first deletion line. Used by the submission
    /// mapper to anchor file-level comments per the spec (a file-level
    /// comment posts on the first valid visible line on the right side, or
    /// the first deleted line for pure-deletion files).
    ///
    /// Returns `None` for binary, too-large, or empty-hunk files, and for
    /// the requested side when the file has no lines on that side (e.g. a
    /// pure addition has no Old-side anchor).
    pub fn first_valid_line(&self, side: LineSide) -> Option<u32> {
        if self.is_binary || self.is_too_large {
            return None;
        }
        for hunk in &self.hunks {
            for line in &hunk.lines {
                let candidate = match side {
                    LineSide::New => match line.origin {
                        LineOrigin::Context | LineOrigin::Addition => line.new_lineno,
                        LineOrigin::Deletion => None,
                    },
                    LineSide::Old => match line.origin {
                        LineOrigin::Deletion => line.old_lineno,
                        _ => None,
                    },
                };
                if let Some(n) = candidate {
                    return Some(n);
                }
            }
        }
        None
    }

    /// Returns `(additions, deletions)` for this file.
    pub fn stat(&self) -> (usize, usize) {
        let mut additions = 0;
        let mut deletions = 0;
        for hunk in &self.hunks {
            for line in &hunk.lines {
                match line.origin {
                    LineOrigin::Addition => additions += 1,
                    LineOrigin::Deletion => deletions += 1,
                    LineOrigin::Context => {}
                }
            }
        }
        (additions, deletions)
    }
}

fn format_hunk_review_content_key(hash: u64) -> String {
    format!("hunk-content-v1:{hash:016x}:0")
}

fn format_hunk_review_span_key(hunk: &DiffHunk, hash: u64) -> String {
    format!(
        "hunk-span-v1:{hash:016x}:{}:{}:{}:{}",
        hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
    )
}

fn write_hunk_content_hash(hasher: &mut Fnv1aHasher, lines: &[DiffLine]) {
    for line in lines {
        hasher.write(match line.origin {
            LineOrigin::Addition => b"+",
            LineOrigin::Deletion => b"-",
            LineOrigin::Context => b" ",
        });
        hasher.write(line.content.as_bytes());
        hasher.write(b"\n");
    }
}
