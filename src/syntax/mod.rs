//! Syntax highlighting for diff lines.
//!
//! Highlighting never runs at load time: backends produce `DiffLine`s with no
//! spans and the app asks for them lazily, for the hunks the viewport
//! touches, in bounded slices of work per frame. Each hunk records its
//! progress in [`HunkHighlight`] so it can be advanced a bit at a time.
//!
//! tree-sitter parses whole texts rather than carrying resumable per-line
//! state, so a slice here is a *window* of display lines: the window's old
//! and new sides are reconstructed, parsed, and highlighted in one pass. The
//! window is large enough that ordinary hunks are done whole — which is what
//! keeps multi-line constructs (a block comment, a template literal) intact —
//! and small enough that jumping into a huge hunk cannot stall a frame.

mod cmark;
mod languages;
mod names;
mod palette;

use std::ops::Range;
use std::path::Path;

use ratatui::style::{Color, Style};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use crate::model::diff_types::{DiffFile, DiffHunk, LineOrigin};
use crate::vcs::tabify;

pub use palette::SyntaxPalette;

/// A single line of highlighted spans (style + text pairs).
pub(crate) type HighlightedSpans = Vec<(Style, String)>;

/// Per-line highlight results for a file: `Some` if the line was highlighted, `None` on failure.
pub(crate) type HighlightedLines = Vec<Option<HighlightedSpans>>;

/// Display lines parsed together. Hunks shorter than this — nearly all of
/// them — are highlighted in a single parse.
const HUNK_WINDOW_LINES: usize = 1_024;

/// Whether `path` belongs to a "container" syntax that embeds other languages
/// and so needs the full file (not just a hunk slice) in scope before its
/// nested grammars activate.
///
/// A grammar reaches an embedded language through an injection anchored on an
/// enclosing node: HTML injects JavaScript into a `<script>` element, so a
/// hunk that does not include the opening tag parses as loose text and every
/// line falls back to the default foreground. Same shape for Svelte / Astro /
/// MDX (fenced code blocks) / PHP / ERB-family templates.
///
/// Container extensions kept narrow. `html` and `md` are intentionally
/// omitted: the vast majority of changes in those files are outside any
/// embedded-language block, so paying the full-file cost on every diff would
/// be a net regression. Add only when an extension is overwhelmingly used as
/// a container (i.e. most hunks live inside a nested grammar).
pub(crate) fn needs_full_file_highlight(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "vue"     // HTML shell with JS/TS and CSS blocks
            | "svelte" // same shape
            | "astro"  // frontmatter + HTML + JS
            | "mdx"    // markdown with JSX inside
            | "php"    // outer HTML, switches at <?php
            | "erb"    // outer HTML, switches at <% %>
            | "eex"    // same shape as erb
            | "heex" // Phoenix component templates
        )
    )
}

/// Highlighting progress for one hunk: how many leading display lines carry
/// their final spans.
///
/// A hunk whose `done` count is nonzero resumes at the next window; nothing
/// else is carried between slices, so a clone can always continue where the
/// original left off.
#[derive(Debug, Clone, Default)]
pub struct HunkHighlight {
    /// Number of leading display lines with final spans.
    done: usize,
    complete: bool,
}

impl HunkHighlight {
    /// Whether display line `line_idx` already carries its final spans.
    pub fn covers(&self, line_idx: usize) -> bool {
        self.complete || line_idx < self.done
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    fn mark_complete(&mut self) {
        self.done = usize::MAX;
        self.complete = true;
    }
}

/// Which side of a diff a window is being highlighted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

impl Side {
    /// Whether a line of this origin is part of the file on this side.
    fn includes(self, origin: LineOrigin) -> bool {
        matches!(
            (self, origin),
            (_, LineOrigin::Context)
                | (Side::Old, LineOrigin::Deletion)
                | (Side::New, LineOrigin::Addition)
        )
    }
}

/// Highlights diff lines and file contents against a theme's palette.
pub struct SyntaxHighlighter {
    /// Styles for each recognised capture name, indexed by the `Highlight`
    /// values `tree_sitter_highlight` reports.
    styles: Vec<Style>,
    /// Style for source a grammar did not capture.
    default_style: Style,
    /// Background color for added lines
    pub add_bg: Color,
    /// Background color for deleted lines
    pub del_bg: Color,
    /// Markdown construct colours, resolved from the palette once.
    markdown_palette: cmark::MarkdownPalette,
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new(
            SyntaxPalette::dark(),
            Color::Rgb(0, 35, 12),
            Color::Rgb(45, 0, 0),
        )
    }
}

impl SyntaxHighlighter {
    /// Create a highlighter for `palette` and the given diff backgrounds.
    pub fn new(palette: SyntaxPalette, add_bg: Color, del_bg: Color) -> Self {
        Self {
            styles: names::styles(&palette),
            default_style: names::default_style(&palette),
            add_bg,
            del_bg,
            markdown_palette: cmark::MarkdownPalette::resolve(&palette),
        }
    }

    /// Advance the highlighting of `file`'s hunk `hunk_idx` until display
    /// line `upto` carries its spans, processing at most `budget` lines.
    /// Returns the number of lines processed.
    ///
    /// Container grammars (see [`needs_full_file_highlight`]) ignore
    /// `hunk_idx`, `upto` and `budget`: their whole file is highlighted in
    /// one go from the text the backend attached, and every hunk of the file
    /// is completed together.
    pub(crate) fn advance_file(
        &self,
        file: &mut DiffFile,
        hunk_idx: usize,
        upto: usize,
        budget: usize,
    ) -> usize {
        if file.is_commit_message {
            mark_all_complete(file);
            return 0;
        }
        let Some(path) = file.new_path.as_deref().or(file.old_path.as_deref()) else {
            mark_all_complete(file);
            return 0;
        };
        if needs_full_file_highlight(path) {
            return self.highlight_whole_file(file);
        }
        let path = path.to_path_buf();
        match file.hunks.get_mut(hunk_idx) {
            Some(hunk) => self.advance_hunk(&path, hunk, upto, budget),
            None => 0,
        }
    }

    /// Highlight every line of every hunk in `files`. Costs the whole diff at
    /// once, so it is for tests and benchmarks only.
    #[cfg(test)]
    pub(crate) fn highlight_files_fully(&self, files: &mut [DiffFile]) {
        for file in files {
            for hunk_idx in 0..file.hunks.len() {
                self.advance_file(file, hunk_idx, usize::MAX, usize::MAX);
            }
            mark_all_complete(file);
        }
    }

    fn advance_hunk(&self, path: &Path, hunk: &mut DiffHunk, upto: usize, budget: usize) -> usize {
        if hunk.highlight.covers(upto) || budget == 0 {
            return 0;
        }
        let total = hunk.lines.len();
        if total == 0 {
            hunk.highlight.mark_complete();
            return 0;
        }
        let Some(config) = self.config_for_hunk(path, hunk) else {
            hunk.highlight.mark_complete();
            return 0;
        };

        let end = upto.min(total - 1);
        let mut processed = 0;
        while hunk.highlight.done <= end && processed < budget {
            let start = hunk.highlight.done;
            let stop = (start + HUNK_WINDOW_LINES).min(total);
            self.highlight_window(config, hunk, start..stop);
            hunk.highlight.done = stop;
            processed += stop - start;
        }

        if hunk.highlight.done >= total {
            hunk.highlight.mark_complete();
        }
        processed
    }

    /// Highlight display lines `window` of `hunk` by reconstructing each side
    /// of the diff over that range and parsing it as one text.
    fn highlight_window(
        &self,
        config: &'static HighlightConfiguration,
        hunk: &mut DiffHunk,
        window: Range<usize>,
    ) {
        for side in [Side::Old, Side::New] {
            let mut text = String::new();
            let mut line_indices = Vec::new();
            for idx in window.clone() {
                let line = &hunk.lines[idx];
                if !side.includes(line.origin) {
                    continue;
                }
                line_indices.push(idx);
                text.push_str(&line.content);
                text.push('\n');
            }
            if line_indices.is_empty() {
                continue;
            }
            let Some(highlighted) = self.highlight_lines_of(config, &text) else {
                continue;
            };
            for (nth, idx) in line_indices.into_iter().enumerate() {
                let line = &mut hunk.lines[idx];
                // A context line is in both sides; the new side wins, so that
                // it reads as the file does after the change.
                if side == Side::Old && line.origin == LineOrigin::Context {
                    continue;
                }
                let Some(spans) = highlighted.get(nth) else {
                    break;
                };
                line.highlighted_spans =
                    Some(self.apply_diff_background(spans.clone(), line.origin));
            }
        }
    }

    fn config_for_hunk(
        &self,
        path: &Path,
        hunk: &DiffHunk,
    ) -> Option<&'static HighlightConfiguration> {
        languages::config_for_path(path).or_else(|| {
            hunk.lines
                .first()
                .and_then(|line| languages::config_for_shebang(&line.content))
        })
    }

    /// Highlight a container-grammar file from the whole-file text attached
    /// by the backend, then drop that text. Lines whose side was not
    /// available keep no spans. Returns the number of file lines highlighted.
    fn highlight_whole_file(&self, file: &mut DiffFile) -> usize {
        let text = file.whole_file_text.take();
        let path = file.new_path.as_deref().or(file.old_path.as_deref());
        let mut highlighted = 0;
        if let (Some(path), Some(text)) = (path, text) {
            let old = text
                .old
                .as_deref()
                .and_then(|content| self.highlight_content(path, content));
            let new = text
                .new
                .as_deref()
                .and_then(|content| self.highlight_content(path, content));
            highlighted = old.as_ref().map_or(0, Vec::len) + new.as_ref().map_or(0, Vec::len);
            self.apply_full_file_spans(file, old.as_deref(), new.as_deref());
        }
        mark_all_complete(file);
        highlighted
    }

    fn highlight_content(&self, path: &Path, content: &str) -> Option<HighlightedLines> {
        let lines: Vec<String> = content.lines().map(tabify).collect();
        self.highlight_file_lines(path, &lines)
    }

    fn apply_full_file_spans(
        &self,
        file: &mut DiffFile,
        old_highlight: Option<&[Option<HighlightedSpans>]>,
        new_highlight: Option<&[Option<HighlightedSpans>]>,
    ) {
        for hunk in &mut file.hunks {
            for line in &mut hunk.lines {
                let old_idx = line.old_lineno.map(|n| n.saturating_sub(1) as usize);
                let new_idx = line.new_lineno.map(|n| n.saturating_sub(1) as usize);
                let spans = self.highlighted_line_for_diff_with_background(
                    old_highlight,
                    new_highlight,
                    old_idx,
                    new_idx,
                    line.origin,
                );
                if spans.is_some() {
                    line.highlighted_spans = spans;
                }
            }
        }
    }

    /// Highlight all lines in a file's content.
    ///
    /// Returns `None` when no grammar matches the file (by path or shebang).
    /// Otherwise returns one `Some(spans)` entry per input line, in order.
    pub fn highlight_file_lines(
        &self,
        file_path: &Path,
        lines: &[String],
    ) -> Option<HighlightedLines> {
        let config = languages::config_for_path(file_path).or_else(|| {
            lines
                .first()
                .and_then(|line| languages::config_for_shebang(line))
        })?;
        let text = lines.join("\n");
        let highlighted = self.highlight_lines_of(config, &text)?;
        Some(
            (0..lines.len())
                .map(|idx| highlighted.get(idx).cloned())
                .collect(),
        )
    }

    /// Highlight a review comment body (`\n`-separated) as Markdown, returning
    /// one entry per line. Colors come from the theme's syntax palette,
    /// matching code highlighting.
    ///
    /// Parsed with `pulldown-cmark` rather than the Markdown grammar; see the
    /// `cmark` module for why. Fenced code blocks are still handed to
    /// tree-sitter for their contents.
    pub(crate) fn highlight_markdown_body(&self, content: &str) -> HighlightedLines {
        cmark::highlight(self, content)
    }

    /// Style runs over `text`, as byte ranges into it.
    ///
    /// Ranges are in order and never overlap, but they need not be
    /// contiguous: callers fill any gap with [`Self::default_style`].
    fn highlight_ranges(
        &self,
        config: &'static HighlightConfiguration,
        text: &str,
    ) -> Option<Vec<(Range<usize>, Style)>> {
        let mut highlighter = Highlighter::new();
        let events = highlighter
            .highlight(config, text.as_bytes(), None, None, |name| {
                languages::injection_config(name)
            })
            .ok()?;

        let mut runs = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for event in events {
            match event.ok()? {
                HighlightEvent::HighlightStart(highlight) => stack.push(highlight.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    if start >= end {
                        continue;
                    }
                    let style = stack
                        .last()
                        .and_then(|idx| self.styles.get(*idx).copied())
                        .unwrap_or(self.default_style);
                    runs.push((start..end, style));
                }
            }
        }
        Some(runs)
    }

    /// Highlight `text` into one span list per `\n`-separated line.
    fn highlight_lines_of(
        &self,
        config: &'static HighlightConfiguration,
        text: &str,
    ) -> Option<Vec<HighlightedSpans>> {
        let runs = self.highlight_ranges(config, text)?;

        let mut lines: Vec<HighlightedSpans> = Vec::new();
        let mut current: HighlightedSpans = Vec::new();
        let mut at = 0usize;
        let mut emit = |slice: &str, style: Style| {
            let mut rest = slice;
            while let Some(idx) = rest.find('\n') {
                push_span(&mut current, style, &rest[..idx]);
                lines.push(std::mem::take(&mut current));
                rest = &rest[idx + 1..];
            }
            push_span(&mut current, style, rest);
        };

        for (range, style) in runs {
            if range.start < at {
                continue;
            }
            // Anything the runs skipped — a gap, or a range that fell on a
            // non-character boundary — still has to reach the output, or the
            // lines would come back short and misaligned.
            if let Some(gap) = text.get(at..range.start) {
                emit(gap, self.default_style);
            }
            match text.get(range.clone()) {
                Some(slice) => emit(slice, style),
                None => continue,
            }
            at = range.end;
        }
        if let Some(tail) = text.get(at..) {
            emit(tail, self.default_style);
        }
        lines.push(current);
        Some(lines)
    }

    fn highlighted_line_at(
        highlighted_lines: Option<&[Option<HighlightedSpans>]>,
        line_idx: Option<usize>,
    ) -> Option<HighlightedSpans> {
        line_idx
            .and_then(|idx| highlighted_lines.and_then(|all| all.get(idx)))
            .and_then(|line_highlight| line_highlight.as_ref().cloned())
    }

    pub(crate) fn highlighted_line_for_diff_with_background(
        &self,
        old_highlighted_lines: Option<&[Option<HighlightedSpans>]>,
        new_highlighted_lines: Option<&[Option<HighlightedSpans>]>,
        old_line_idx: Option<usize>,
        new_line_idx: Option<usize>,
        origin: LineOrigin,
    ) -> Option<HighlightedSpans> {
        let spans = match origin {
            LineOrigin::Addition => Self::highlighted_line_at(new_highlighted_lines, new_line_idx),
            LineOrigin::Deletion => Self::highlighted_line_at(old_highlighted_lines, old_line_idx),
            LineOrigin::Context => Self::highlighted_line_at(new_highlighted_lines, new_line_idx),
        }?;

        Some(self.apply_diff_background(spans, origin))
    }

    /// Apply diff background colors to highlighted spans based on line origin
    pub fn apply_diff_background(
        &self,
        spans: Vec<(Style, String)>,
        origin: LineOrigin,
    ) -> Vec<(Style, String)> {
        let bg_color = match origin {
            LineOrigin::Addition => self.add_bg,
            LineOrigin::Deletion => self.del_bg,
            LineOrigin::Context => return spans, // No background for context
        };

        spans
            .into_iter()
            .map(|(style, text)| (style.bg(bg_color), text))
            .collect()
    }
}

/// Append `text` to `spans`, extending the last span when the style matches.
fn push_span(spans: &mut HighlightedSpans, style: Style, text: &str) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some((last_style, last_text)) if *last_style == style => last_text.push_str(text),
        _ => spans.push((style, text.to_string())),
    }
}

fn mark_all_complete(file: &mut DiffFile) {
    for hunk in &mut file.hunks {
        hunk.highlight.mark_complete();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::diff_types::{DiffLine, FileStatus, WholeFileText};
    use std::path::PathBuf;

    fn diff_line(
        origin: LineOrigin,
        content: &str,
        old: Option<u32>,
        new: Option<u32>,
    ) -> DiffLine {
        DiffLine {
            origin,
            content: content.to_string(),
            old_lineno: old,
            new_lineno: new,
            highlighted_spans: None,
        }
    }

    fn hunk(lines: Vec<DiffLine>) -> DiffHunk {
        let n = lines.len() as u32;
        DiffHunk {
            header: format!("@@ -1,{n} +1,{n} @@"),
            lines,
            old_start: 1,
            old_count: n,
            new_start: 1,
            new_count: n,
            highlight: HunkHighlight::default(),
        }
    }

    fn file(path: &str, hunks: Vec<DiffHunk>) -> DiffFile {
        DiffFile {
            old_path: Some(PathBuf::from(path)),
            new_path: Some(PathBuf::from(path)),
            status: FileStatus::Modified,
            hunks,
            is_binary: false,
            is_too_large: false,
            is_commit_message: false,
            content_hash: 0,
            whole_file_text: None,
        }
    }

    fn rust_lines(n: usize) -> Vec<DiffLine> {
        (0..n)
            .map(|i| {
                let ln = i as u32 + 1;
                match i % 3 {
                    0 => diff_line(LineOrigin::Context, "fn main() {", Some(ln), Some(ln)),
                    1 => diff_line(LineOrigin::Deletion, "    let x = 1;", Some(ln), None),
                    _ => diff_line(LineOrigin::Addition, "    let y = \"s\";", None, Some(ln)),
                }
            })
            .collect()
    }

    fn distinct_fg_count(spans: &HighlightedSpans) -> usize {
        spans
            .iter()
            .filter_map(|(s, _)| s.fg)
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    fn line_text(spans: &HighlightedSpans) -> String {
        spans.iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn should_highlight_each_line_independently() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];
        let highlighted = highlighter.highlight_file_lines(Path::new("main.rs"), &lines);

        assert!(highlighted.is_some());
        let highlighted = highlighted.unwrap();
        assert_eq!(highlighted.len(), lines.len());
        assert!(highlighted.iter().all(|line| line.is_some()));
    }

    /// Callers index the result by line and render the spans directly, so the
    /// text has to come back exactly as it went in — for every grammar, not
    /// just the ones with tidy queries.
    #[test]
    fn should_round_trip_every_language_line_for_line() {
        let highlighter = SyntaxHighlighter::default();
        let samples: &[(&str, &str)] = &[
            ("main.rs", "fn main() {\n    let x = \"日本語\";\n}\n"),
            ("a.py", "def f(x):\n    return x + 1  # 🎉\n"),
            ("a.ts", "const x: number = 1\nexport {}\n"),
            ("a.tsx", "const A = () => <div a=\"b\">hi</div>\n"),
            ("a.go", "package main\n\nfunc main() {}\n"),
            ("a.cpp", "#include <vector>\nint main() { return 0; }\n"),
            ("a.rb", "def x\n  puts 'hi'\nend\n"),
            ("a.yml", "key:\n  - one\n  - two\n"),
            ("a.toml", "[table]\nkey = \"value\"\n"),
            ("a.json", "{\n  \"a\": [1, 2]\n}\n"),
            ("a.html", "<div>\n  <script>let a = 1</script>\n</div>\n"),
            ("a.scss", "$c: red;\n.a { color: $c; }\n"),
            ("a.md", "# Title\n\nSome `code` and **bold**.\n"),
            (
                "a.tf",
                "resource \"aws_s3_bucket\" \"b\" {\n  bucket = var.name\n}\n",
            ),
            ("a.kt", "fun main() {\n    val x = \"hi\"\n}\n"),
            ("a.sh", "#!/bin/sh\necho \"hi\" | wc -l\n"),
            ("a.sql", "SELECT a, b FROM t WHERE a = 1;\n"),
            ("a.java", "class A {\n  void f() {}\n}\n"),
        ];

        for (path, source) in samples {
            let lines: Vec<String> = source.lines().map(str::to_string).collect();
            let highlighted = highlighter
                .highlight_file_lines(Path::new(path), &lines)
                .unwrap_or_else(|| panic!("no grammar for {path}"));
            assert_eq!(highlighted.len(), lines.len(), "line count for {path}");
            for (idx, want) in lines.iter().enumerate() {
                let spans = highlighted[idx].as_ref().expect("line highlighted");
                assert_eq!(&line_text(spans), want, "line {idx} of {path}");
            }
        }
    }

    #[test]
    fn should_colour_more_than_one_role_per_language() {
        let highlighter = SyntaxHighlighter::default();
        let samples: &[(&str, &str)] = &[
            ("main.rs", "fn main() { let x = \"s\"; }"),
            ("a.py", "def f(x): return 'y'"),
            ("a.ts", "const x: string = 'y'"),
            ("a.go", "func main() { s := \"y\" }"),
            ("a.cpp", "int main() { return 0; }"),
            ("a.tf", "resource \"a\" \"b\" { c = 1 }"),
            ("a.kt", "fun f() { val x = 1 }"),
            ("a.yml", "key: value"),
        ];

        for (path, source) in samples {
            let lines = vec![source.to_string()];
            let spans = highlighter
                .highlight_file_lines(Path::new(path), &lines)
                .unwrap_or_else(|| panic!("no grammar for {path}"))[0]
                .clone()
                .expect("line highlighted");
            assert!(
                distinct_fg_count(&spans) >= 2,
                "{path} produced one flat colour: {spans:?}"
            );
        }
    }

    /// The HTML grammar injects JavaScript into `<script>`; without the
    /// injection callback the block would come back as undifferentiated text.
    #[test]
    fn should_highlight_injected_languages() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "<script>".to_string(),
            "  const total = 1;".to_string(),
            "</script>".to_string(),
        ];
        let highlighted = highlighter
            .highlight_file_lines(Path::new("a.html"), &lines)
            .expect("html grammar");
        let script = highlighted[1].as_ref().expect("line highlighted");
        assert!(
            distinct_fg_count(script) >= 2,
            "javascript inside <script> was not highlighted: {script:?}"
        );
    }

    #[test]
    fn should_find_syntax_for_uppercase_extension() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec!["fn main() {}".to_string()];
        assert!(
            highlighter
                .highlight_file_lines(Path::new("SRC/MAIN.RS"), &lines)
                .is_some()
        );
    }

    #[test]
    fn highlighted_spans_should_have_color() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];
        let highlighted = highlighter
            .highlight_file_lines(Path::new("test.rs"), &lines)
            .unwrap();
        for (i, line) in highlighted.iter().enumerate() {
            let spans = line
                .as_ref()
                .unwrap_or_else(|| panic!("line {i} should be Some"));
            assert!(!spans.is_empty(), "line {i} should have spans");
            let has_fg = spans.iter().any(|(style, _)| style.fg.is_some());
            assert!(has_fg, "line {i} should have foreground color: {spans:?}");
        }
    }

    #[test]
    fn should_detect_syntax_from_shebang_when_extensionless() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "#!/usr/bin/env python".to_string(),
            "print('hello')".to_string(),
        ];

        let highlighted = highlighter.highlight_file_lines(Path::new("script"), &lines);
        assert!(highlighted.is_some());
        assert_eq!(highlighted.unwrap().len(), lines.len());
    }

    #[test]
    fn should_not_include_trailing_newline_in_highlighted_spans() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];

        let highlighted = highlighter
            .highlight_file_lines(Path::new("test.rs"), &lines)
            .unwrap();

        for (i, line) in highlighted.iter().enumerate() {
            let spans = line.as_ref().unwrap();
            assert!(
                !line_text(spans).contains('\n'),
                "line {i} spans should not contain a newline"
            );
        }
    }

    #[test]
    fn highlighted_line_for_diff_with_background_should_handle_none_per_line() {
        let highlighter = SyntaxHighlighter::default();
        let old_lines = vec![None];
        let new_lines = vec![None];
        let highlighted = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Addition,
        );
        assert!(highlighted.is_none());
    }

    #[test]
    fn highlighted_line_for_diff_with_background_should_apply_background_on_success() {
        let highlighter = SyntaxHighlighter::default();
        let old_lines = vec![Some(vec![(Style::default(), "old".to_string())])];
        let new_lines = vec![Some(vec![(Style::default(), "new".to_string())])];

        let deletion = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Deletion,
        );
        let addition = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Addition,
        );
        let context = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Context,
        );

        let deletion = deletion.unwrap();
        assert_eq!(deletion.len(), 1);
        assert_eq!(deletion[0].0.bg, Some(highlighter.del_bg));
        assert_eq!(deletion[0].1, "old");

        let addition = addition.unwrap();
        assert_eq!(addition.len(), 1);
        assert_eq!(addition[0].0.bg, Some(highlighter.add_bg));
        assert_eq!(addition[0].1, "new");

        let context = context.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].0.bg, None);
        assert_eq!(context[0].1, "new");
    }

    #[test]
    fn advance_hunk_should_stop_at_the_requested_line_and_resume() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file("a.rs", vec![hunk(rust_lines(9))]);

        let processed = highlighter.advance_file(&mut f, 0, 3, 1);
        assert_eq!(processed, 9, "a window smaller than the hunk is not split");
        assert!(f.hunks[0].highlight.is_complete());
        assert!(
            f.hunks[0]
                .lines
                .iter()
                .all(|l| l.highlighted_spans.is_some())
        );
    }

    /// A hunk longer than one window is delivered a window at a time, so a
    /// frame that only needs the top of it does not pay for the whole thing.
    #[test]
    fn advance_hunk_should_process_long_hunks_a_window_at_a_time() {
        let highlighter = SyntaxHighlighter::default();
        let total = HUNK_WINDOW_LINES * 3;
        let mut f = file("a.rs", vec![hunk(rust_lines(total))]);

        let processed = highlighter.advance_file(&mut f, 0, 10, 1);
        assert_eq!(processed, HUNK_WINDOW_LINES);
        assert!(!f.hunks[0].highlight.is_complete());
        assert!(f.hunks[0].highlight.covers(HUNK_WINDOW_LINES - 1));
        assert!(!f.hunks[0].highlight.covers(HUNK_WINDOW_LINES));

        let processed = highlighter.advance_file(&mut f, 0, usize::MAX, usize::MAX);
        assert_eq!(processed, total - HUNK_WINDOW_LINES);
        assert!(f.hunks[0].highlight.is_complete());
        assert!(
            f.hunks[0]
                .lines
                .iter()
                .all(|l| l.highlighted_spans.is_some())
        );
        assert_eq!(
            highlighter.advance_file(&mut f, 0, usize::MAX, usize::MAX),
            0
        );
    }

    #[test]
    fn advance_hunk_should_honor_the_line_budget() {
        let highlighter = SyntaxHighlighter::default();
        let total = HUNK_WINDOW_LINES * 2;
        let mut f = file("a.rs", vec![hunk(rust_lines(total))]);

        assert_eq!(
            highlighter.advance_file(&mut f, 0, total - 1, 1),
            HUNK_WINDOW_LINES
        );
        assert!(!f.hunks[0].highlight.is_complete());
        assert_eq!(
            highlighter.advance_file(&mut f, 0, total - 1, HUNK_WINDOW_LINES * 4),
            HUNK_WINDOW_LINES
        );
        assert!(f.hunks[0].highlight.is_complete());
    }

    /// Both sides of a hunk are parsed, so a deletion is highlighted from the
    /// old file's text and an addition from the new one's.
    #[test]
    fn both_sides_of_a_hunk_should_be_highlighted() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file(
            "a.rs",
            vec![hunk(vec![
                diff_line(LineOrigin::Context, "fn f() {", Some(1), Some(1)),
                diff_line(LineOrigin::Deletion, "    let old = 1;", Some(2), None),
                diff_line(LineOrigin::Addition, "    let new = 2;", None, Some(2)),
                diff_line(LineOrigin::Context, "}", Some(3), Some(3)),
            ])],
        );
        highlighter.highlight_files_fully(std::slice::from_mut(&mut f));
        for line in &f.hunks[0].lines {
            let spans = line
                .highlighted_spans
                .as_ref()
                .unwrap_or_else(|| panic!("unhighlighted line {line:?}"));
            assert_eq!(line_text(spans), line.content);
        }
        let deletion = f.hunks[0].lines[1].highlighted_spans.as_ref().unwrap();
        assert!(distinct_fg_count(deletion) >= 2, "{deletion:?}");
    }

    /// The point of parsing a window as one text: a construct that opens on
    /// one line and closes on another has to be recognised on both.
    #[test]
    fn multi_line_constructs_should_span_lines_within_a_window() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file(
            "a.rs",
            vec![hunk(vec![
                diff_line(LineOrigin::Context, "/* start", Some(1), Some(1)),
                diff_line(LineOrigin::Context, "   still comment", Some(2), Some(2)),
                diff_line(LineOrigin::Context, "   end */", Some(3), Some(3)),
                diff_line(LineOrigin::Context, "fn f() {}", Some(4), Some(4)),
            ])],
        );
        highlighter.highlight_files_fully(std::slice::from_mut(&mut f));

        let closing = f.hunks[0].lines[2].highlighted_spans.as_ref().unwrap();
        let code = f.hunks[0].lines[3].highlighted_spans.as_ref().unwrap();
        assert_ne!(
            closing[0].0.fg, code[0].0.fg,
            "the closing line of a block comment should still read as a comment"
        );
    }

    #[test]
    fn diff_backgrounds_should_follow_line_origin() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file("a.rs", vec![hunk(rust_lines(3))]);
        highlighter.highlight_files_fully(std::slice::from_mut(&mut f));
        let lines = &f.hunks[0].lines;
        assert!(
            lines[0]
                .highlighted_spans
                .as_ref()
                .unwrap()
                .iter()
                .all(|(s, _)| s.bg.is_none())
        );
        assert!(
            lines[1]
                .highlighted_spans
                .as_ref()
                .unwrap()
                .iter()
                .all(|(s, _)| s.bg == Some(highlighter.del_bg))
        );
        assert!(
            lines[2]
                .highlighted_spans
                .as_ref()
                .unwrap()
                .iter()
                .all(|(s, _)| s.bg == Some(highlighter.add_bg))
        );
    }

    #[test]
    fn unknown_syntax_should_complete_without_spans() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file(
            "notes.unknownext",
            vec![hunk(vec![diff_line(
                LineOrigin::Context,
                "hello",
                Some(1),
                Some(1),
            )])],
        );
        assert_eq!(highlighter.advance_file(&mut f, 0, 0, usize::MAX), 0);
        assert!(f.hunks[0].highlight.is_complete());
        assert!(f.hunks[0].lines[0].highlighted_spans.is_none());
    }

    #[test]
    fn shebang_should_pick_the_syntax_for_extensionless_hunks() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file(
            "script",
            vec![hunk(vec![
                diff_line(LineOrigin::Addition, "#!/usr/bin/env python", None, Some(1)),
                diff_line(LineOrigin::Addition, "print('hello')", None, Some(2)),
            ])],
        );
        highlighter.highlight_files_fully(std::slice::from_mut(&mut f));
        assert!(
            f.hunks[0]
                .lines
                .iter()
                .all(|l| l.highlighted_spans.is_some())
        );
    }

    #[test]
    fn commit_message_files_should_never_be_highlighted() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = file(
            "Commit Message (abc)",
            vec![hunk(vec![diff_line(
                LineOrigin::Context,
                "#!/bin/sh fix",
                None,
                Some(1),
            )])],
        );
        f.is_commit_message = true;
        highlighter.highlight_files_fully(std::slice::from_mut(&mut f));
        assert!(f.hunks[0].highlight.is_complete());
        assert!(f.hunks[0].lines[0].highlighted_spans.is_none());
    }

    fn vue_file(deleted: &str, added: &str, target_line: u32) -> DiffFile {
        let h = DiffHunk {
            header: format!("@@ -{target_line} +{target_line} @@"),
            lines: vec![
                diff_line(LineOrigin::Deletion, deleted, Some(target_line), None),
                diff_line(LineOrigin::Addition, added, None, Some(target_line)),
            ],
            old_start: target_line,
            old_count: 1,
            new_start: target_line,
            new_count: 1,
            highlight: HunkHighlight::default(),
        };
        file("Comp.vue", vec![h])
    }

    const VUE_OLD: &str = "<template>\n  <div>{{ msg }}</div>\n</template>\n\n<script setup>\n\
                           import { ref } from 'vue'\nconst msg = ref('hi')\nconst other = 1\n</script>\n";
    const VUE_NEW: &str = "<template>\n  <div>{{ msg }}</div>\n</template>\n\n<script setup>\n\
                           import { ref } from 'vue'\nconst msg = ref('hello')\nconst other = 1\n</script>\n";

    #[test]
    fn container_files_should_highlight_from_whole_file_text() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = vue_file("const msg = ref('hi')", "const msg = ref('hello')", 7);
        f.whole_file_text = Some(WholeFileText {
            old: Some(VUE_OLD.to_string()),
            new: Some(VUE_NEW.to_string()),
        });

        let highlighted = highlighter.advance_file(&mut f, 0, 0, 1);
        assert_eq!(highlighted, 18, "both sides of the nine-line file");
        assert!(f.whole_file_text.is_none(), "text is dropped once used");
        assert!(f.hunks[0].highlight.is_complete());
        for line in &f.hunks[0].lines {
            let spans = line
                .highlighted_spans
                .as_ref()
                .unwrap_or_else(|| panic!("vue line should be highlighted: {line:?}"));
            assert!(
                distinct_fg_count(spans) >= 2,
                "vue line {line:?} should have varied fg colors"
            );
        }
        let deletion = f.hunks[0].lines[0].highlighted_spans.as_ref().unwrap();
        assert!(
            deletion
                .iter()
                .all(|(s, _)| s.bg == Some(highlighter.del_bg))
        );
    }

    #[test]
    fn container_files_without_text_should_complete_without_spans() {
        // Per-hunk highlighting of a container grammar would paint every line
        // in the theme's default foreground, which reads worse than the plain
        // diff colours, so a file whose text could not be fetched stays plain.
        let highlighter = SyntaxHighlighter::default();
        let mut f = vue_file("const msg = ref('hi')", "const msg = ref('hello')", 7);
        assert_eq!(highlighter.advance_file(&mut f, 0, 0, usize::MAX), 0);
        assert!(f.hunks[0].highlight.is_complete());
        assert!(
            f.hunks[0]
                .lines
                .iter()
                .all(|l| l.highlighted_spans.is_none())
        );
    }

    #[test]
    fn container_files_should_keep_lines_whose_side_is_missing_plain() {
        let highlighter = SyntaxHighlighter::default();
        let mut f = vue_file("const msg = ref('hi')", "const msg = ref('hello')", 7);
        f.whole_file_text = Some(WholeFileText {
            old: None,
            new: Some(VUE_NEW.to_string()),
        });
        highlighter.advance_file(&mut f, 0, 0, usize::MAX);
        assert!(f.hunks[0].lines[0].highlighted_spans.is_none());
        assert!(f.hunks[0].lines[1].highlighted_spans.is_some());
    }
}
