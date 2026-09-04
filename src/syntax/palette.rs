//! Theme-facing colours for syntax highlighting.
//!
//! tree-sitter grammars label code with capture names (`@keyword`,
//! `@string.escape`, ...) rather than the TextMate scopes a `.tmTheme` keys
//! on, so a theme supplies colours per *role* instead of shipping a theme
//! file. [`names`](super::names) folds the several hundred capture names the
//! bundled grammars use down to the roles below.

use ratatui::style::Color;

/// One colour per syntax role a theme can control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntaxPalette {
    /// Anything a grammar does not label, and the fallback for every role.
    pub text: Color,
    pub comment: Color,
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub constant: Color,
    pub function: Color,
    pub type_name: Color,
    pub variable: Color,
    pub property: Color,
    pub operator: Color,
    pub punctuation: Color,
    pub attribute: Color,
    pub tag: Color,
    pub namespace: Color,
    pub escape: Color,
    pub error: Color,
}

impl SyntaxPalette {
    /// Every role in `text`, except comments in `comment`.
    ///
    /// Themes name the roles they colour differently and take the rest from
    /// here with struct-update syntax, so adding a role does not mean editing
    /// two dozen theme definitions.
    /// The palette the built-in dark theme uses, and the fallback for a local
    /// theme on a dark background. Base16 Eighties, which is what the
    /// TextMate-era default resolved to.
    pub const fn dark() -> Self {
        Self {
            keyword: Color::Rgb(0xcc, 0x99, 0xcc),
            string: Color::Rgb(0x99, 0xcc, 0x99),
            number: Color::Rgb(0xf9, 0x91, 0x57),
            constant: Color::Rgb(0xf9, 0x91, 0x57),
            function: Color::Rgb(0x66, 0x99, 0xcc),
            type_name: Color::Rgb(0xff, 0xcc, 0x66),
            property: Color::Rgb(0x66, 0x99, 0xcc),
            operator: Color::Rgb(0x66, 0xcc, 0xcc),
            attribute: Color::Rgb(0xff, 0xcc, 0x66),
            tag: Color::Rgb(0xf2, 0x77, 0x7a),
            namespace: Color::Rgb(0xff, 0xcc, 0x66),
            escape: Color::Rgb(0x66, 0xcc, 0xcc),
            error: Color::Rgb(0xf2, 0x77, 0x7a),
            ..Self::plain(Color::Rgb(0xd3, 0xd0, 0xc8), Color::Rgb(0x74, 0x73, 0x69))
        }
    }

    /// The palette the built-in light theme uses, and the fallback for a
    /// local theme on a light background.
    pub const fn light() -> Self {
        Self {
            keyword: Color::Rgb(0xcf, 0x22, 0x2e),
            string: Color::Rgb(0x0a, 0x30, 0x69),
            number: Color::Rgb(0x05, 0x50, 0xae),
            constant: Color::Rgb(0x05, 0x50, 0xae),
            function: Color::Rgb(0x82, 0x50, 0xdf),
            type_name: Color::Rgb(0x95, 0x38, 0x00),
            property: Color::Rgb(0x05, 0x50, 0xae),
            operator: Color::Rgb(0x05, 0x50, 0xae),
            attribute: Color::Rgb(0x05, 0x50, 0xae),
            tag: Color::Rgb(0x11, 0x63, 0x29),
            namespace: Color::Rgb(0x95, 0x38, 0x00),
            escape: Color::Rgb(0x05, 0x50, 0xae),
            error: Color::Rgb(0xcf, 0x22, 0x2e),
            ..Self::plain(Color::Rgb(0x24, 0x29, 0x2f), Color::Rgb(0x6e, 0x77, 0x81))
        }
    }

    pub const fn plain(text: Color, comment: Color) -> Self {
        Self {
            text,
            comment,
            keyword: text,
            string: text,
            number: text,
            constant: text,
            function: text,
            type_name: text,
            variable: text,
            property: text,
            operator: text,
            punctuation: text,
            attribute: text,
            tag: text,
            namespace: text,
            escape: text,
            error: text,
        }
    }
}
