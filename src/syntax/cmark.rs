//! Markdown highlighting for review comment bodies, via a CommonMark parser.
//!
//! Comment bodies could go through the Markdown *grammar* — the same path as
//! code highlighting — but they do not, and the reason predates tree-sitter:
//! syntect's `Markdown.sublime-syntax` drove oniguruma into exponential
//! backtracking on lines carrying several inline code spans (0.17 ms at two
//! spans, 82 ms at eight, then flat — a regex engine exhausting its retry
//! budget). `pulldown-cmark` is a single-pass CommonMark parser with no
//! regex and no backtracking, flat at ~0.01 ms across every span count.
//!
//! The tree-sitter Markdown grammar would not backtrack, but it parses in two
//! passes (block, then an inline grammar injected into it) and its captures
//! describe document structure rather than the handful of constructs a review
//! comment uses. `pulldown-cmark` stays: it is faster, and the mapping from
//! its events to styles is direct.
//!
//! Fenced code blocks are still handed to tree-sitter, where the language is
//! known and a real grammar is worth the work.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use std::ops::Range;
use tree_sitter_highlight::HighlightConfiguration;

use super::palette::SyntaxPalette;
use super::{HighlightedLines, SyntaxHighlighter, languages};

/// Styles for markdown constructs, resolved once from the theme's palette so
/// prose and the code around it agree on their colours.
///
/// Resolved at construction rather than per call: it is small, but strictly
/// wasted work on a path that runs for every comment on every frame.
pub(super) struct MarkdownPalette {
    base: Style,
    heading: Style,
    bold: Style,
    italic: Style,
    code: Style,
    link: Style,
    quote: Style,
    list: Style,
    strike: Style,
}

impl MarkdownPalette {
    pub(super) fn resolve(palette: &SyntaxPalette) -> Self {
        let fg = |color| Style::default().fg(color);
        Self {
            base: fg(palette.text),
            heading: fg(palette.keyword).add_modifier(Modifier::BOLD),
            bold: fg(palette.text).add_modifier(Modifier::BOLD),
            italic: fg(palette.text).add_modifier(Modifier::ITALIC),
            code: fg(palette.string),
            link: fg(palette.function).add_modifier(Modifier::UNDERLINED),
            quote: fg(palette.comment),
            list: fg(palette.operator),
            strike: fg(palette.comment).add_modifier(Modifier::CROSSED_OUT),
        }
    }
}

/// A fenced block being walked: the grammar to highlight it with, and the
/// span its text events have covered so far.
struct OpenCodeBlock {
    config: &'static HighlightConfiguration,
    span: Option<Range<usize>>,
}

/// Markdown-highlight `content` (a whole `\n`-separated body).
///
/// Returns exactly one entry per line of `content`, in order, so callers can
/// index the result by line. Every entry is `Some`: there is no per-line
/// failure mode to report.
pub(super) fn highlight(hl: &SyntaxHighlighter, content: &str) -> HighlightedLines {
    let palette = &hl.markdown_palette;

    // One style slot per byte of the source. Events arrive in document order,
    // each `Start` precedes the content it encloses, and inner ranges are
    // subsets of outer ones — so painting in event order yields inner-wins
    // nesting (`**bold with `code`**`) without maintaining a style stack.
    let mut styles: Vec<Style> = vec![palette.base; content.len()];
    let mut paint = |range: Range<usize>, style: Style| {
        let end = range.end.min(content.len());
        if let Some(slots) = styles.get_mut(range.start..end) {
            slots.fill(style);
        }
    };

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut code_block: Option<OpenCodeBlock> = None;

    for (event, range) in Parser::new_ext(content, options).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => paint(range, palette.heading),
            Event::Start(Tag::Strong) => paint(range, palette.bold),
            Event::Start(Tag::Emphasis) => paint(range, palette.italic),
            Event::Start(Tag::Strikethrough) => paint(range, palette.strike),
            Event::Start(Tag::BlockQuote(_)) => paint(range, palette.quote),
            Event::Start(Tag::Link { .. }) => paint(range, palette.link),
            // The whole item is styled as a list, not just the bullet; nested
            // inline events repaint their own ranges.
            Event::Start(Tag::Item) => paint(range, palette.list),
            Event::TaskListMarker(_) => paint(range, palette.list),
            Event::Code(_) => paint(range, palette.code),
            Event::Start(Tag::CodeBlock(kind)) => {
                // The ``` fence itself stays at the base colour. The block's
                // contents are collected from `Event::Text` and highlighted
                // in one parse when the block closes, because tree-sitter
                // needs the whole block, not a line of it.
                let token = match &kind {
                    CodeBlockKind::Fenced(lang) => lang.split_whitespace().next().unwrap_or(""),
                    CodeBlockKind::Indented => "",
                };
                code_block = languages::config_for_token(token)
                    .map(|config| OpenCodeBlock { config, span: None });
            }
            Event::Text(_) => {
                // Only fenced-block text needs painting; prose inherits the
                // style its enclosing tag already applied.
                if let Some(block) = code_block.as_mut() {
                    block.span = Some(match &block.span {
                        Some(span) => span.start..range.end,
                        None => range,
                    });
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                let Some(OpenCodeBlock {
                    config,
                    span: Some(span),
                }) = code_block.take()
                else {
                    continue;
                };
                let Some(source) = content.get(span.clone()) else {
                    continue;
                };
                let Some(runs) = hl.highlight_ranges(config, source) else {
                    continue;
                };
                for (run, style) in runs {
                    paint(span.start + run.start..span.start + run.end, style);
                }
            }
            _ => {}
        }
    }

    // Slice the per-byte styles back into per-line runs, coalescing adjacent
    // characters that share a style.
    let mut out: HighlightedLines = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        let mut runs: Vec<(Style, String)> = Vec::new();
        for (i, ch) in line.char_indices() {
            let style = styles.get(offset + i).copied().unwrap_or(palette.base);
            match runs.last_mut() {
                Some((prev, text)) if *prev == style => text.push(ch),
                _ => runs.push((style, ch.to_string())),
            }
        }
        out.push(Some(runs));
        // +1 for the '\n' that `split` consumed.
        offset += line.len() + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bodies exercising the shapes review comments actually take, plus the
    /// degenerate ones. Reused by the structural invariant tests below.
    const BODIES: &[&str] = &[
        "",
        "plain prose",
        "line one\nline two\nline three",
        "trailing newline\n",
        "\n\nleading blanks",
        "# Heading\n\ntext under it",
        "- item one\n- item two with `code`",
        "1. first\n2. second",
        "> quoted line\n> second quoted",
        "**bold** *italic* ~~struck~~ `code` [link](https://example.com)",
        "before\n```rust\nfn main() { let x = 1; }\n```\nafter",
        "unterminated\n```rust\nfn main() {}",
        "```\nno language\n```",
        "| a | b |\n| --- | --- |\n| 1 | 2 |",
        "- [x] done\n- [ ] not done",
        "日本語のテキストです\nemoji 🎉 and `código` mixed",
        "`a` `b` `c` `d` `e` `f` `g` `h` `i` `j` `k` `l`",
        "**bold with `code` inside** and trailing",
    ];

    fn highlighter() -> SyntaxHighlighter {
        SyntaxHighlighter::default()
    }

    fn line_text(line: &Option<Vec<(Style, String)>>) -> String {
        line.as_ref()
            .map(|runs| runs.iter().map(|(_, t)| t.as_str()).collect())
            .unwrap_or_default()
    }

    /// Callers index the result by line number and wrap the run text directly,
    /// so the highlight must reproduce the body exactly, line for line.
    #[test]
    fn should_round_trip_every_body_exactly() {
        let hl = highlighter();
        for body in BODIES {
            let out = highlight(&hl, body);
            let expected: Vec<&str> = body.split('\n').collect();
            assert_eq!(out.len(), expected.len(), "line count for {body:?}");
            for (idx, want) in expected.iter().enumerate() {
                assert_eq!(&line_text(&out[idx]), want, "line {idx} of {body:?}");
            }
        }
    }

    /// Every line is highlighted; there is no per-line failure mode to report.
    #[test]
    fn should_highlight_every_line() {
        let hl = highlighter();
        for body in BODIES {
            assert!(
                highlight(&hl, body).iter().all(Option::is_some),
                "unhighlighted line in {body:?}"
            );
        }
    }

    #[test]
    fn should_style_inline_code_distinctly_from_prose() {
        let hl = highlighter();
        let out = highlight(&hl, "plain `code` plain");
        let runs = out[0].as_ref().expect("line highlighted");

        let code = runs
            .iter()
            .find(|(_, t)| t.contains("code"))
            .expect("inline code is its own run");
        let prose = runs
            .iter()
            .find(|(_, t)| t.starts_with("plain"))
            .expect("prose is its own run");

        assert_ne!(
            code.0.fg, prose.0.fg,
            "inline code should not match surrounding prose"
        );
    }

    /// The regression this module exists for: a line packed with inline code
    /// spans used to drive the Markdown grammar into exponential backtracking.
    /// Every span must still resolve to the code colour.
    #[test]
    fn should_style_many_inline_code_spans_on_one_line() {
        let hl = highlighter();
        let body = "Rename `a` `b` `c` `d` `e` `f` `g` `h` `i` `j` `k` `l` please.";
        let runs = highlight(&hl, body)[0]
            .as_ref()
            .expect("line highlighted")
            .clone();

        let prose = runs
            .iter()
            .find(|(_, t)| t.starts_with("Rename"))
            .map(|(s, _)| s.fg);
        let coloured = runs
            .iter()
            .filter(|(s, t)| t.starts_with('`') && Some(s.fg) != prose)
            .count();
        assert_eq!(coloured, 12, "every code span should be coloured: {runs:?}");
    }

    #[test]
    fn should_style_bold_italic_heading_quote_and_list_distinctly() {
        let hl = highlighter();
        let base = highlight(&hl, "plain")[0].as_ref().unwrap()[0].0;

        for body in [
            "**bold**",
            "*italic*",
            "# Heading",
            "> quoted",
            "- item",
            "[text](https://example.com)",
        ] {
            let runs = highlight(&hl, body)[0].as_ref().unwrap().clone();
            assert!(
                runs.iter().any(|(s, _)| *s != base),
                "{body:?} should differ from plain prose"
            );
        }
    }

    /// Fenced blocks delegate to a real grammar, which is the point of the
    /// split: the language is known, so tree-sitter can do its job.
    #[test]
    fn should_delegate_fenced_code_blocks_to_tree_sitter() {
        let hl = highlighter();
        let out = highlight(&hl, "text\n```rust\nfn main() { let x = 1; }\n```\ntext");
        let code = out[2].as_ref().expect("code line highlighted");

        let keyword = code
            .iter()
            .find(|(_, t)| t.contains("fn"))
            .expect("`fn` is its own run");
        let plain = code
            .iter()
            .find(|(_, t)| t.contains('('))
            .expect("punctuation run");
        assert_ne!(
            keyword.0.fg, plain.0.fg,
            "the rust keyword should be coloured"
        );
    }

    /// Multi-line constructs need whole-block state: a fenced block's second
    /// line is only code because the fence opened on an earlier line.
    #[test]
    fn should_carry_state_across_lines_in_fenced_blocks() {
        let hl = highlighter();
        let body = "```rust\nlet a = 1;\nlet b = 2;\n```";
        let out = highlight(&hl, body);
        let plain = highlight(&hl, "plain")[0].as_ref().unwrap()[0].0.fg;

        for idx in [1usize, 2] {
            let runs = out[idx].as_ref().expect("code line highlighted");
            assert!(
                runs.iter().any(|(s, _)| s.fg != plain),
                "line {idx} should be tokenised as rust: {runs:?}"
            );
        }
    }

    /// Byte-offset painting must not split multibyte characters.
    #[test]
    fn should_preserve_multibyte_content() {
        let hl = highlighter();
        let body = "日本語 `コード` です\n🎉 emoji `x` 🎉";
        let out = highlight(&hl, body);

        assert_eq!(line_text(&out[0]), "日本語 `コード` です");
        assert_eq!(line_text(&out[1]), "🎉 emoji `x` 🎉");
    }

    #[test]
    fn should_handle_empty_body() {
        let hl = highlighter();
        let out = highlight(&hl, "");
        assert_eq!(out.len(), 1);
        assert_eq!(line_text(&out[0]), "");
    }
}
