//! Syntax highlighting for diff lines.
//!
//! Highlighting is the dominant cost of opening a diff (roughly 40µs per line
//! against 1µs to parse it), so it never runs at load time. Backends produce
//! `DiffLine`s with no spans; the app then asks for spans lazily, for the
//! hunks the viewport touches, in bounded slices of work per frame. Each
//! hunk records its progress in [`HunkHighlight`] and the app keeps the
//! parser state to continue from in [`HunkStates`], so a hunk can be advanced
//! a few lines at a time.

mod cmark;

use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::path::Path;
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

use crate::model::diff_types::{DiffFile, DiffHunk, LineOrigin};
use crate::vcs::tabify;

/// A single line of highlighted spans (style + text pairs).
pub(crate) type HighlightedSpans = Vec<(Style, String)>;

/// Per-line highlight results for a file: `Some` if the line was highlighted, `None` on failure.
pub(crate) type HighlightedLines = Vec<Option<HighlightedSpans>>;

/// Whether `path` belongs to a "container" syntax that embeds other languages
/// and so needs the full file (not just a hunk slice) in scope before its
/// nested grammars activate.
///
/// Sublime-syntax grammars start in a `main` context entered at the top of the
/// file. For Vue, syntect stays in `text.html.vue`'s outer scope until it
/// sees `<template>`, `<script>`, or `<style>`. Same shape for Svelte / Astro
/// / MDX (fenced code blocks) / PHP / ERB-family templates: anything inside a
/// nested block in a hunk that doesn't include the opening tag will fall back
/// to the theme's default foreground.
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
            "vue"     // text.html.vue: HTML + JS/TS + CSS blocks
            | "svelte" // source.svelte: HTML + JS/TS + CSS blocks
            | "astro"  // source.astro: frontmatter + HTML + JS
            | "mdx"    // text.html.markdown with JSX inside
            | "php"    // text.html.php: outer HTML, switches at <?php
            | "erb"    // text.html.erb: outer HTML, switches at <% %>
            | "eex"    // text.html.elixir: same shape as erb
            | "heex" // text.html.heex: Phoenix component templates
        )
    )
}

/// Highlighting progress for one hunk: how many leading display lines carry
/// their final spans, and which [`HunkStates`] entry holds the parser state
/// needed to continue.
///
/// Display lines are fed to syntect in order, so the state after line `n` is
/// exactly what line `n + 1` needs. Context lines advance both sides;
/// deletions advance the old side and additions the new, matching how a
/// unified hunk interleaves the two files.
///
/// The parser state itself is not stored here: it is not `Send`, and hunks
/// cross threads on the diff-watch channel. A hunk whose entry is missing or
/// out of step with `done` (a clone, or a table that was pruned) starts over
/// from its first line, so a stale entry can only cost work, never spans.
#[derive(Debug, Clone, Default)]
pub struct HunkHighlight {
    /// Number of leading display lines with final spans.
    done: usize,
    complete: bool,
    /// Id of this hunk's in-progress parser state in the app's [`HunkStates`].
    state: Option<u64>,
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
        self.state = None;
    }
}

/// Parser states for hunks that are partway through highlighting. Owned by
/// the app on the main thread; entries are removed when a hunk completes.
#[derive(Debug, Default)]
pub struct HunkStates {
    entries: HashMap<u64, HunkStateEntry>,
    next_id: u64,
}

#[derive(Debug)]
struct HunkStateEntry {
    /// The hunk's `done` count when this state was stored. Continuing from it
    /// is only valid for a hunk that still reports the same count.
    done: usize,
    old: LineState,
    new: LineState,
}

/// Above this many in-progress hunks the table is cleared. Entries belong to
/// hunks the user scrolled partway through, so the table is usually tiny;
/// hunks from a replaced diff would otherwise linger forever.
const MAX_HUNK_STATES: usize = 256;

impl HunkStates {
    fn take(&mut self, id: u64, done: usize) -> Option<(LineState, LineState)> {
        let entry = self.entries.remove(&id)?;
        (entry.done == done).then_some((entry.old, entry.new))
    }

    fn store(&mut self, done: usize, old: LineState, new: LineState) -> u64 {
        if self.entries.len() >= MAX_HUNK_STATES {
            self.entries.clear();
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(id, HunkStateEntry { done, old, new });
        id
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// syntect parser and style state for one side of a hunk.
#[derive(Debug, Clone)]
struct LineState {
    parse: ParseState,
    highlight: HighlightState,
}

impl LineState {
    fn new(syntax: &SyntaxReference, highlighter: &Highlighter<'_>) -> Self {
        Self {
            parse: ParseState::new(syntax),
            highlight: HighlightState::new(highlighter, ScopeStack::new()),
        }
    }

    fn step(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        highlighter: &Highlighter<'_>,
    ) -> Option<HighlightedSpans> {
        let text = format!("{line}\n");
        let ops = self.parse.parse_line(&text, syntax_set).ok()?;
        let ranges: Vec<(syntect::highlighting::Style, &str)> =
            HighlightIterator::new(&mut self.highlight, &ops, &text, highlighter).collect();
        Some(SyntaxHighlighter::ranges_to_spans(ranges))
    }
}

/// Helper to highlight lines of code from a diff
pub struct SyntaxHighlighter {
    pub syntax_set: syntect::parsing::SyntaxSet,
    pub theme: syntect::highlighting::Theme,
    /// Background color for added lines
    pub add_bg: Color,
    /// Background color for deleted lines
    pub del_bg: Color,
    /// Markdown construct colours, resolved from `theme` once at construction.
    markdown_palette: cmark::MarkdownPalette,
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new(
            EmbeddedThemeName::Base16EightiesDark,
            Color::Rgb(0, 35, 12),
            Color::Rgb(45, 0, 0),
        )
    }
}

impl SyntaxHighlighter {
    /// Create a new syntax highlighter with the given theme and diff background colors
    pub fn new(theme_name: EmbeddedThemeName, add_bg: Color, del_bg: Color) -> Self {
        let theme_set = two_face::theme::extra();
        let theme = theme_set[theme_name].clone();

        Self::with_theme(theme, add_bg, del_bg)
    }

    /// Create a new syntax highlighter with a preloaded syntect theme.
    pub fn with_theme(theme: syntect::highlighting::Theme, add_bg: Color, del_bg: Color) -> Self {
        let syntax_set = two_face::syntax::extra_newlines();
        let markdown_palette = cmark::MarkdownPalette::resolve(&theme);
        Self {
            syntax_set,
            theme,
            add_bg,
            del_bg,
            markdown_palette,
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
        states: &mut HunkStates,
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
            Some(hunk) => self.advance_hunk(states, &path, hunk, upto, budget),
            None => 0,
        }
    }

    /// Highlight every line of every hunk in `files`. Costs the whole diff at
    /// once, so it is for tests and benchmarks only.
    #[cfg(test)]
    pub(crate) fn highlight_files_fully(&self, files: &mut [DiffFile]) {
        let mut states = HunkStates::default();
        for file in files {
            for hunk_idx in 0..file.hunks.len() {
                self.advance_file(&mut states, file, hunk_idx, usize::MAX, usize::MAX);
            }
            mark_all_complete(file);
        }
    }

    fn advance_hunk(
        &self,
        states: &mut HunkStates,
        path: &Path,
        hunk: &mut DiffHunk,
        upto: usize,
        budget: usize,
    ) -> usize {
        if hunk.highlight.covers(upto) || budget == 0 {
            return 0;
        }
        let total = hunk.lines.len();
        if total == 0 {
            hunk.highlight.mark_complete();
            return 0;
        }

        let highlighter = Highlighter::new(&self.theme);
        let resumed = hunk
            .highlight
            .state
            .take()
            .and_then(|id| states.take(id, hunk.highlight.done));
        let (mut old, mut new) = match resumed {
            Some(pair) => pair,
            None => {
                let Some(syntax) = self.syntax_for_hunk(path, hunk) else {
                    hunk.highlight.mark_complete();
                    return 0;
                };
                hunk.highlight.done = 0;
                (
                    LineState::new(syntax, &highlighter),
                    LineState::new(syntax, &highlighter),
                )
            }
        };

        let mut next = hunk.highlight.done;
        let end = upto.min(total - 1);
        let mut processed = 0;
        while next <= end && processed < budget {
            let line = &mut hunk.lines[next];
            let spans = match line.origin {
                LineOrigin::Context => {
                    old.step(&line.content, &self.syntax_set, &highlighter);
                    new.step(&line.content, &self.syntax_set, &highlighter)
                }
                LineOrigin::Addition => new.step(&line.content, &self.syntax_set, &highlighter),
                LineOrigin::Deletion => old.step(&line.content, &self.syntax_set, &highlighter),
            };
            line.highlighted_spans = spans.map(|s| self.apply_diff_background(s, line.origin));
            next += 1;
            processed += 1;
        }

        if next >= total {
            hunk.highlight.mark_complete();
        } else {
            hunk.highlight.done = next;
            hunk.highlight.state = Some(states.store(next, old, new));
        }
        processed
    }

    fn syntax_for_hunk(&self, path: &Path, hunk: &DiffHunk) -> Option<&SyntaxReference> {
        self.get_syntax(path).or_else(|| {
            hunk.lines
                .first()
                .and_then(|line| self.syntax_set.find_syntax_by_first_line(&line.content))
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
    /// Returns `None` when no syntax can be resolved for the file (by path or shebang).
    /// Otherwise returns one entry per input line:
    /// - `Some(spans)` if that line was highlighted successfully (including empty spans)
    /// - `None` if highlighting failed for that specific line
    pub fn highlight_file_lines(
        &self,
        file_path: &Path,
        lines: &[String],
    ) -> Option<HighlightedLines> {
        // Get syntax definition
        let syntax = self.get_syntax(file_path).or_else(|| {
            lines
                .first()
                .and_then(|line| self.syntax_set.find_syntax_by_first_line(line))
        })?;

        Some(self.highlight_lines_with(syntax, lines))
    }

    /// Highlight a review comment body (`\n`-separated) as Markdown, returning
    /// one entry per line. Colors come from the active syntect theme, matching
    /// code highlighting.
    ///
    /// Parsed with `pulldown-cmark` rather than syntect's Markdown grammar; see
    /// the `cmark` module for why. Fenced code blocks are still handed to
    /// syntect for their contents.
    pub(crate) fn highlight_markdown_body(&self, content: &str) -> HighlightedLines {
        cmark::highlight(self, content)
    }

    /// Run syntect line-by-line against a resolved syntax, converting to
    /// ratatui spans. Shared by file and markdown highlighting.
    fn highlight_lines_with(
        &self,
        syntax: &syntect::parsing::SyntaxReference,
        lines: &[String],
    ) -> HighlightedLines {
        use syntect::easy::HighlightLines;

        let mut highlighter = HighlightLines::new(syntax, &self.theme);

        Self::collect_line_highlights(lines, |line| {
            // Highlight failures are scoped to the single line; other lines still keep highlighting.
            highlighter
                .highlight_line(&format!("{}\n", line), &self.syntax_set)
                .ok()
                .map(Self::ranges_to_spans)
        })
    }

    /// Convert syntect's styled ranges for one line into owned ratatui spans.
    ///
    /// Strips the trailing `\n` that syntect includes from the input. Leaving
    /// it causes ratatui to allocate an extra buffer cell, misaligning
    /// side-by-side diff columns on short (padded) lines.
    fn ranges_to_spans(ranges: Vec<(syntect::highlighting::Style, &str)>) -> HighlightedSpans {
        let mut spans: HighlightedSpans = ranges
            .into_iter()
            .map(|(style, text)| (Self::syntect_to_ratatui_style(style), text.to_string()))
            .collect();
        if let Some(last) = spans.last_mut()
            && last.1.ends_with('\n')
        {
            last.1.truncate(last.1.len() - 1);
            if last.1.is_empty() {
                spans.pop();
            }
        }
        spans
    }

    fn collect_line_highlights<F>(lines: &[String], mut highlight_line: F) -> HighlightedLines
    where
        F: FnMut(&str) -> Option<HighlightedSpans>,
    {
        let mut result = Vec::with_capacity(lines.len());
        for line in lines {
            result.push(highlight_line(line));
        }
        result
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

    fn syntect_to_ratatui_style(style: syntect::highlighting::Style) -> Style {
        let fg_color = Self::syntect_color_to_ratatui(style.foreground);
        let mut ratatui_style = Style::default().fg(fg_color);

        if style
            .font_style
            .contains(syntect::highlighting::FontStyle::BOLD)
        {
            ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
        }
        if style
            .font_style
            .contains(syntect::highlighting::FontStyle::ITALIC)
        {
            ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
        }
        if style
            .font_style
            .contains(syntect::highlighting::FontStyle::UNDERLINE)
        {
            ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
        }

        ratatui_style
    }

    /// Translate syntect colors into ratatui colors.
    ///
    /// Some bat-compatible Base16 `.tmTheme` files encode ANSI palette slots as
    /// placeholder colors of the form `#0N000000`. syntect preserves those
    /// bytes literally, so we translate them here at the render boundary.
    fn syntect_color_to_ratatui(color: syntect::highlighting::Color) -> Color {
        if color.g == 0 && color.b == 0 && color.a == 0 {
            return match color.r {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                15 => Color::White,
                _ => Color::Rgb(color.r, color.g, color.b),
            };
        }

        Color::Rgb(color.r, color.g, color.b)
    }

    /// Map extensions not in two-face's syntax set to a known equivalent.
    fn fallback_extension(ext: &str) -> Option<&'static str> {
        match ext {
            "jsx" | "mjs" | "cjs" => Some("js"),
            "hbs" | "handlebars" | "mustache" | "ejs" | "pug" | "jade" | "njk" => Some("html"),
            "mdx" => Some("md"),
            "jsonc" | "json5" | "prisma" => Some("json"),
            "heex" => Some("rb"),
            _ => None,
        }
    }

    /// Map extension-less filenames to a known syntax extension.
    fn fallback_filename(name: &str) -> Option<&'static str> {
        match name {
            "Containerfile" => Some("sh"),
            "Justfile" | "justfile" => Some("sh"),
            _ => None,
        }
    }

    /// Resolve syntax from a file path using this lookup order:
    /// extension -> lowercase extension (when different) -> fallback extension ->
    /// filename token -> filename name -> fallback filename.
    fn get_syntax(&self, file_path: &Path) -> Option<&syntect::parsing::SyntaxReference> {
        // Try by extension first
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            if let Some(syntax) = self.syntax_set.find_syntax_by_extension(ext) {
                return Some(syntax);
            }

            let normalized = ext.to_ascii_lowercase();
            if normalized != ext
                && let Some(syntax) = self.syntax_set.find_syntax_by_extension(&normalized)
            {
                return Some(syntax);
            }

            // Try fallback mapping for extensions not in syntect's defaults
            if let Some(fallback) = Self::fallback_extension(&normalized)
                && let Some(syntax) = self.syntax_set.find_syntax_by_extension(fallback)
            {
                return Some(syntax);
            }
        }

        // Try token/name matches for extension-less files (e.g. Makefile, BUILD).
        if let Some(filename) = file_path.file_name().and_then(|f| f.to_str()) {
            if let Some(syntax) = self.syntax_set.find_syntax_by_token(filename) {
                return Some(syntax);
            }

            if let Some(syntax) = self.syntax_set.find_syntax_by_name(filename) {
                return Some(syntax);
            }

            if let Some(fallback) = Self::fallback_filename(filename)
                && let Some(syntax) = self.syntax_set.find_syntax_by_extension(fallback)
            {
                return Some(syntax);
            }
        }

        None
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

    #[test]
    fn should_find_syntax_for_uppercase_extension() {
        let highlighter = SyntaxHighlighter::default();
        let syntax = highlighter.get_syntax(Path::new("SRC/MAIN.RS"));
        assert!(syntax.is_some());
    }

    #[test]
    fn should_find_syntax_for_build_filename_token() {
        let highlighter = SyntaxHighlighter::default();
        let syntax = highlighter.get_syntax(Path::new("BUILD"));
        assert!(syntax.is_some());
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

    #[test]
    fn should_keep_file_highlighting_when_one_line_fails() {
        let lines = vec!["first".to_string(), "bad".to_string(), "third".to_string()];
        let highlighted = SyntaxHighlighter::collect_line_highlights(&lines, |line| {
            if line == "bad" {
                None
            } else {
                Some(vec![(Style::default(), line.to_string())])
            }
        });

        assert_eq!(highlighted.len(), lines.len());
        assert!(highlighted[0].is_some());
        assert!(highlighted[1].is_none());
        assert!(highlighted[2].is_some());
    }

    #[test]
    fn should_find_syntax_for_typescript() {
        let highlighter = SyntaxHighlighter::default();
        for ext in &["ts", "tsx", "mts", "cts", "jsx", "mjs", "cjs"] {
            let path = format!("file.{ext}");
            assert!(
                highlighter.get_syntax(Path::new(&path)).is_some(),
                "should find syntax for .{ext}"
            );
        }
    }

    #[test]
    fn should_find_syntax_for_fallback_extensions() {
        let highlighter = SyntaxHighlighter::default();
        let extensions = [
            "jsx", "mjs", "cjs", "hbs", "mustache", "ejs", "pug", "njk", "mdx", "jsonc", "json5",
            "prisma", "heex",
        ];
        for ext in &extensions {
            let path = format!("file.{ext}");
            assert!(
                highlighter.get_syntax(Path::new(&path)).is_some(),
                "should find syntax for .{ext}"
            );
        }
    }

    #[test]
    fn should_find_syntax_for_fallback_filenames() {
        let highlighter = SyntaxHighlighter::default();
        for name in &["Containerfile", "Justfile", "justfile"] {
            assert!(
                highlighter.get_syntax(Path::new(name)).is_some(),
                "should find syntax for {name}"
            );
        }
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
            // At least one span should have a non-default foreground color
            let has_fg = spans.iter().any(|(style, _)| style.fg.is_some());
            assert!(has_fg, "line {i} should have foreground color: {spans:?}");
        }
    }

    #[test]
    fn should_translate_base16_placeholder_colors_to_ansi_palette() {
        let style = SyntaxHighlighter::syntect_to_ratatui_style(syntect::highlighting::Style {
            foreground: syntect::highlighting::Color {
                r: 7,
                g: 0,
                b: 0,
                a: 0,
            },
            background: syntect::highlighting::Color::BLACK,
            font_style: syntect::highlighting::FontStyle::empty(),
        });
        assert_eq!(style.fg, Some(Color::Gray));

        let bright = SyntaxHighlighter::syntect_to_ratatui_style(syntect::highlighting::Style {
            foreground: syntect::highlighting::Color {
                r: 12,
                g: 0,
                b: 0,
                a: 0,
            },
            background: syntect::highlighting::Color::BLACK,
            font_style: syntect::highlighting::FontStyle::empty(),
        });
        assert_eq!(bright.fg, Some(Color::LightBlue));
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
    fn should_preserve_empty_line_highlight_results() {
        let lines = vec!["value".to_string(), "".to_string()];
        let highlighted = SyntaxHighlighter::collect_line_highlights(&lines, |line| {
            if line.is_empty() {
                Some(Vec::new())
            } else {
                Some(vec![(Style::default(), line.to_string())])
            }
        });

        assert!(matches!(highlighted[1], Some(ref spans) if spans.is_empty()));
    }

    #[test]
    fn should_not_use_weak_fallback_mappings() {
        for ext in &["toml", "hcl", "tf", "tfvars", "nix", "swift", "zig", "v"] {
            assert_eq!(SyntaxHighlighter::fallback_extension(ext), None);
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
    fn should_not_include_trailing_newline_in_highlighted_spans() {
        // given - syntect requires a trailing \n for highlight_line, but the
        // resulting spans must not include it. A leaked \n occupies an extra
        // buffer cell in ratatui, misaligning side-by-side diff columns on
        // short (padded) lines while truncated lines stay correct.
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];

        // when
        let highlighted = highlighter
            .highlight_file_lines(Path::new("test.rs"), &lines)
            .unwrap();

        // then
        for (i, line) in highlighted.iter().enumerate() {
            let spans = line.as_ref().unwrap();
            let full_text: String = spans.iter().map(|(_, t)| t.as_str()).collect();
            assert!(
                !full_text.contains('\n'),
                "line {i} spans should not contain newline, got: {full_text:?}"
            );
        }
    }

    #[test]
    fn advance_hunk_should_stop_at_the_requested_line_and_resume() {
        let highlighter = SyntaxHighlighter::default();
        let mut states = HunkStates::default();
        let mut f = file("a.rs", vec![hunk(rust_lines(9))]);

        let processed = highlighter.advance_file(&mut states, &mut f, 0, 3, usize::MAX);
        assert_eq!(processed, 4);
        let h = &f.hunks[0];
        assert!(h.highlight.covers(3));
        assert!(!h.highlight.covers(4));
        assert!(!h.highlight.is_complete());
        assert!(h.lines[..4].iter().all(|l| l.highlighted_spans.is_some()));
        assert!(h.lines[4..].iter().all(|l| l.highlighted_spans.is_none()));

        let processed = highlighter.advance_file(&mut states, &mut f, 0, usize::MAX, usize::MAX);
        assert_eq!(processed, 5);
        let h = &f.hunks[0];
        assert!(h.highlight.is_complete());
        assert!(h.lines.iter().all(|l| l.highlighted_spans.is_some()));
    }

    #[test]
    fn advance_hunk_should_honor_the_line_budget() {
        let highlighter = SyntaxHighlighter::default();
        let mut states = HunkStates::default();
        let mut f = file("a.rs", vec![hunk(rust_lines(9))]);

        assert_eq!(highlighter.advance_file(&mut states, &mut f, 0, 8, 2), 2);
        assert!(f.hunks[0].highlight.covers(1));
        assert!(!f.hunks[0].highlight.covers(2));
        assert_eq!(highlighter.advance_file(&mut states, &mut f, 0, 8, 100), 7);
        assert!(f.hunks[0].highlight.is_complete());
        assert_eq!(highlighter.advance_file(&mut states, &mut f, 0, 8, 100), 0);
    }

    #[test]
    fn state_table_should_only_hold_hunks_in_progress() {
        let highlighter = SyntaxHighlighter::default();
        let mut states = HunkStates::default();
        let mut f = file("a.rs", vec![hunk(rust_lines(6))]);

        highlighter.advance_file(&mut states, &mut f, 0, 2, usize::MAX);
        assert_eq!(states.len(), 1);
        highlighter.advance_file(&mut states, &mut f, 0, usize::MAX, usize::MAX);
        assert_eq!(states.len(), 0, "a completed hunk keeps no parser state");
    }

    #[test]
    fn clone_with_stale_state_should_restart_and_still_match() {
        // A clone shares the original's state id. Once the original advances,
        // the entry is gone (or out of step), so the clone must start over
        // rather than continue from a state that belongs to different lines.
        let highlighter = SyntaxHighlighter::default();
        let mut states = HunkStates::default();
        let lines = vec![
            diff_line(LineOrigin::Context, "/* start", Some(1), Some(1)),
            diff_line(LineOrigin::Context, "   still comment", Some(2), Some(2)),
            diff_line(LineOrigin::Context, "   end */", Some(3), Some(3)),
            diff_line(LineOrigin::Context, "fn f() {}", Some(4), Some(4)),
        ];
        let mut original = file("a.rs", vec![hunk(lines.clone())]);
        highlighter.advance_file(&mut states, &mut original, 0, 1, usize::MAX);
        let mut clone = original.clone();
        highlighter.advance_file(&mut states, &mut original, 0, usize::MAX, usize::MAX);

        let processed =
            highlighter.advance_file(&mut states, &mut clone, 0, usize::MAX, usize::MAX);
        assert_eq!(processed, 4, "the clone restarts from its first line");

        let mut reference = file("a.rs", vec![hunk(lines)]);
        highlighter.advance_file(&mut states, &mut reference, 0, usize::MAX, usize::MAX);
        for (a, b) in clone.hunks[0].lines.iter().zip(&reference.hunks[0].lines) {
            assert_eq!(a.highlighted_spans, b.highlighted_spans, "{:?}", a.content);
        }
        assert_eq!(states.len(), 0);
    }

    #[test]
    fn incremental_highlighting_should_match_one_shot_highlighting() {
        // A hunk advanced in slices must produce the same spans as one advanced
        // in a single call: the parser state carried between slices is the
        // whole point.
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            diff_line(LineOrigin::Context, "/* start", Some(1), Some(1)),
            diff_line(LineOrigin::Deletion, "   still comment", Some(2), None),
            diff_line(LineOrigin::Addition, "   also comment", None, Some(2)),
            diff_line(LineOrigin::Context, "   end */", Some(3), Some(3)),
            diff_line(LineOrigin::Context, "fn f() {}", Some(4), Some(4)),
        ];
        let mut states = HunkStates::default();
        let mut sliced = file("a.rs", vec![hunk(lines.clone())]);
        let mut whole = file("a.rs", vec![hunk(lines)]);

        for upto in 0..5 {
            highlighter.advance_file(&mut states, &mut sliced, 0, upto, 1);
        }
        highlighter.advance_file(&mut states, &mut whole, 0, usize::MAX, usize::MAX);

        for (a, b) in sliced.hunks[0].lines.iter().zip(&whole.hunks[0].lines) {
            assert_eq!(a.highlighted_spans, b.highlighted_spans, "{:?}", a.content);
        }
        // The block comment spans lines, so the closing line must have been
        // recognised as a comment rather than code.
        let closing = whole.hunks[0].lines[3].highlighted_spans.as_ref().unwrap();
        let code = whole.hunks[0].lines[4].highlighted_spans.as_ref().unwrap();
        assert_ne!(closing[0].0.fg, code[0].0.fg);
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
        let mut states = HunkStates::default();
        let mut f = file(
            "notes.unknownext",
            vec![hunk(vec![diff_line(
                LineOrigin::Context,
                "hello",
                Some(1),
                Some(1),
            )])],
        );
        assert_eq!(
            highlighter.advance_file(&mut states, &mut f, 0, 0, usize::MAX),
            0
        );
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
        let mut states = HunkStates::default();
        let mut f = vue_file("const msg = ref('hi')", "const msg = ref('hello')", 7);
        f.whole_file_text = Some(WholeFileText {
            old: Some(VUE_OLD.to_string()),
            new: Some(VUE_NEW.to_string()),
        });

        let highlighted = highlighter.advance_file(&mut states, &mut f, 0, 0, 1);
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
        let mut states = HunkStates::default();
        let mut f = vue_file("const msg = ref('hi')", "const msg = ref('hello')", 7);
        assert_eq!(
            highlighter.advance_file(&mut states, &mut f, 0, 0, usize::MAX),
            0
        );
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
        let mut states = HunkStates::default();
        let mut f = vue_file("const msg = ref('hi')", "const msg = ref('hello')", 7);
        f.whole_file_text = Some(WholeFileText {
            old: None,
            new: Some(VUE_NEW.to_string()),
        });
        highlighter.advance_file(&mut states, &mut f, 0, 0, usize::MAX);
        assert!(f.hunks[0].lines[0].highlighted_spans.is_none());
        assert!(f.hunks[0].lines[1].highlighted_spans.is_some());
    }
}
