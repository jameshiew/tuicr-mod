//! The capture names we recognise, and the palette role each one paints.
//!
//! `tree_sitter_highlight` is told a fixed list of names once per grammar and
//! then reports matches as an index into that list, so the list is global and
//! the per-theme part is only the parallel table of styles built by
//! [`styles`].
//!
//! Name matching is *unordered subset* matching: a recognised name matches a
//! capture when every dot-separated part of the name appears somewhere in the
//! capture's parts, and the recognised name with the most parts wins. So
//! `string` claims `@string.special.symbol` while a two-part `string.escape`
//! outranks it on `@string.escape`, and ties fall to whichever entry comes
//! first below. Captures nothing here matches — `@none`, `@spell`, a
//! grammar's private `@_name` helpers — are left unstyled.
//!
//! The list is drawn from the capture names the bundled grammars actually
//! use, including the older nvim-style spellings (`@conditional`,
//! `@parameter`, `@field`) that several of them still emit.

use ratatui::style::{Modifier, Style};

use super::palette::SyntaxPalette;

/// A palette role, plus the handful of prose roles that markup grammars need
/// and that are derived from the palette rather than named by a theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Text,
    Comment,
    Keyword,
    String,
    Number,
    Constant,
    Function,
    Type,
    Variable,
    Property,
    Operator,
    Punctuation,
    Attribute,
    Tag,
    Namespace,
    Escape,
    Error,
    MarkupHeading,
    MarkupBold,
    MarkupItalic,
    MarkupLink,
    MarkupQuote,
    MarkupList,
    MarkupRaw,
}

/// Recognised capture names, paired with the role each paints.
///
/// Order matters only for ties (see the module docs), so more specific names
/// need no particular position — but equally specific ones do.
const RECOGNIZED: &[(&str, Role)] = &[
    ("annotation", Role::Attribute),
    ("attribute", Role::Attribute),
    ("boolean", Role::Constant),
    ("character", Role::String),
    ("character.special", Role::Escape),
    ("comment", Role::Comment),
    ("conditional", Role::Keyword),
    ("constant", Role::Constant),
    ("constructor", Role::Type),
    ("define", Role::Keyword),
    ("delimiter", Role::Punctuation),
    ("error", Role::Error),
    ("escape", Role::Escape),
    ("exception", Role::Keyword),
    ("field", Role::Property),
    ("float", Role::Number),
    ("function", Role::Function),
    ("include", Role::Keyword),
    ("import", Role::Keyword),
    ("keyword", Role::Keyword),
    ("label", Role::Constant),
    ("macro", Role::Function),
    ("method", Role::Function),
    ("module", Role::Namespace),
    ("namespace", Role::Namespace),
    ("number", Role::Number),
    ("operator", Role::Operator),
    ("parameter", Role::Variable),
    ("preproc", Role::Keyword),
    ("property", Role::Property),
    ("punctuation", Role::Punctuation),
    ("repeat", Role::Keyword),
    ("storageclass", Role::Keyword),
    ("string", Role::String),
    ("string.escape", Role::Escape),
    ("symbol", Role::Constant),
    ("tag", Role::Tag),
    ("tag.attribute", Role::Attribute),
    ("tag.delimiter", Role::Punctuation),
    ("type", Role::Type),
    ("type.parameter", Role::Type),
    ("variable", Role::Variable),
    ("variable.member", Role::Property),
    // Prose. Grammars split between the `markup.*` and `text.*` spellings;
    // markdown, the one that matters most here, uses `text.*`.
    ("markup.heading", Role::MarkupHeading),
    ("markup.strong", Role::MarkupBold),
    ("markup.italic", Role::MarkupItalic),
    ("markup.link", Role::MarkupLink),
    ("markup.quote", Role::MarkupQuote),
    ("markup.list", Role::MarkupList),
    ("markup.raw", Role::MarkupRaw),
    ("text", Role::Text),
    ("text.title", Role::MarkupHeading),
    ("text.strong", Role::MarkupBold),
    ("text.emphasis", Role::MarkupItalic),
    ("text.literal", Role::MarkupRaw),
    ("text.reference", Role::MarkupLink),
    ("text.uri", Role::MarkupLink),
];

/// The recognised names, in the order [`styles`] indexes them.
pub(crate) fn names() -> Vec<&'static str> {
    RECOGNIZED.iter().map(|(name, _)| *name).collect()
}

/// The style for each recognised name under `palette`, parallel to
/// [`names`] so a `Highlight(i)` indexes straight into it.
pub(crate) fn styles(palette: &SyntaxPalette) -> Vec<Style> {
    RECOGNIZED
        .iter()
        .map(|(_, role)| style_for(*role, palette))
        .collect()
}

/// The style a bare (uncaptured) run of source gets.
pub(crate) fn default_style(palette: &SyntaxPalette) -> Style {
    Style::default().fg(palette.text)
}

fn style_for(role: Role, palette: &SyntaxPalette) -> Style {
    let fg = |color| Style::default().fg(color);
    match role {
        Role::Text => fg(palette.text),
        Role::Comment => fg(palette.comment),
        Role::Keyword => fg(palette.keyword),
        Role::String => fg(palette.string),
        Role::Number => fg(palette.number),
        Role::Constant => fg(palette.constant),
        Role::Function => fg(palette.function),
        Role::Type => fg(palette.type_name),
        Role::Variable => fg(palette.variable),
        Role::Property => fg(palette.property),
        Role::Operator => fg(palette.operator),
        Role::Punctuation => fg(palette.punctuation),
        Role::Attribute => fg(palette.attribute),
        Role::Tag => fg(palette.tag),
        Role::Namespace => fg(palette.namespace),
        Role::Escape => fg(palette.escape),
        Role::Error => fg(palette.error),
        // Prose reuses the code roles rather than asking themes for seven
        // more colours; the emphasis that carries the meaning is the
        // modifier, not the hue.
        Role::MarkupHeading => fg(palette.keyword).add_modifier(Modifier::BOLD),
        Role::MarkupBold => fg(palette.text).add_modifier(Modifier::BOLD),
        Role::MarkupItalic => fg(palette.text).add_modifier(Modifier::ITALIC),
        Role::MarkupLink => fg(palette.function).add_modifier(Modifier::UNDERLINED),
        Role::MarkupQuote => fg(palette.comment),
        Role::MarkupList => fg(palette.operator),
        Role::MarkupRaw => fg(palette.string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn palette() -> SyntaxPalette {
        SyntaxPalette {
            keyword: Color::Rgb(1, 0, 0),
            string: Color::Rgb(2, 0, 0),
            comment: Color::Rgb(3, 0, 0),
            ..SyntaxPalette::plain(Color::Rgb(9, 9, 9), Color::Rgb(3, 0, 0))
        }
    }

    #[test]
    fn names_and_styles_should_stay_parallel() {
        assert_eq!(names().len(), styles(&palette()).len());
    }

    #[test]
    fn recognized_names_should_be_unique() {
        let mut seen: Vec<&str> = names();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate recognized capture name");
    }

    /// The table is only useful if the roles resolve to the palette entry the
    /// theme set, so spot-check the three every grammar emits.
    #[test]
    fn styles_should_come_from_the_palette() {
        let palette = palette();
        let styles = styles(&palette);
        let style_of = |name: &str| {
            let idx = names().iter().position(|n| *n == name).expect("name");
            styles[idx]
        };
        assert_eq!(style_of("keyword").fg, Some(palette.keyword));
        assert_eq!(style_of("string").fg, Some(palette.string));
        assert_eq!(style_of("comment").fg, Some(palette.comment));
    }
}
