//! The bundled tree-sitter grammars, and how a file is matched to one.
//!
//! Grammars are compiled into the binary but their queries are not: parsing a
//! grammar's highlight query costs a millisecond or two, and a review touches
//! a handful of languages, so each configuration is built on first use and
//! then kept for the life of the process. Keeping them in a global also lets
//! the injection callback hand out `&'static` configurations, which is what
//! `tree_sitter_highlight` wants for embedded languages (JavaScript inside
//! HTML, a fenced code block inside Markdown).
//!
//! Configurations are theme-independent: a theme only supplies the styles
//! that `Highlight` indices are looked up in, so one cache serves every
//! theme and a theme switch costs nothing here.

use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use tree_sitter::Language;
use tree_sitter_highlight::HighlightConfiguration;

use super::names;

/// A bundled grammar and the files it claims.
struct LanguageDef {
    /// Canonical name. Also what injection queries and Markdown fences match
    /// against, so it follows the `injection.language` spelling grammars use.
    name: &'static str,
    /// Extra spellings an injection query or a code fence might use.
    aliases: &'static [&'static str],
    /// Lowercase file extensions, without the dot.
    extensions: &'static [&'static str],
    /// Exact file names, for files that carry no extension.
    filenames: &'static [&'static str],
    build: fn() -> Option<HighlightConfiguration>,
}

/// Per-entry configuration cache, parallel to [`LANGUAGES`].
static CONFIGS: LazyLock<Vec<OnceLock<Option<HighlightConfiguration>>>> =
    LazyLock::new(|| LANGUAGES.iter().map(|_| OnceLock::new()).collect());

fn config_at(index: usize) -> Option<&'static HighlightConfiguration> {
    CONFIGS[index]
        .get_or_init(|| (LANGUAGES[index].build)())
        .as_ref()
}

/// The configuration for `path`, matched on extension then file name.
pub(crate) fn config_for_path(path: &Path) -> Option<&'static HighlightConfiguration> {
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        let extension = extension.to_ascii_lowercase();
        if let Some(index) = LANGUAGES
            .iter()
            .position(|lang| lang.extensions.contains(&extension.as_str()))
        {
            return config_at(index);
        }
    }

    let name = path.file_name().and_then(|n| n.to_str())?;
    let index = LANGUAGES
        .iter()
        .position(|lang| lang.filenames.contains(&name))?;
    config_at(index)
}

/// The configuration a language *token* names — an `injection.language`
/// value, or the word after a Markdown code fence.
///
/// Tokens are matched against names, then aliases, then extensions, so
/// ```` ```rs ```` and `#!/usr/bin/env python3` land on the same grammars as
/// `main.rs` and `main.py` without a second alias table.
pub(crate) fn config_for_token(token: &str) -> Option<&'static HighlightConfiguration> {
    let token = token.trim().to_ascii_lowercase();
    if token.is_empty() {
        return None;
    }
    let index = LANGUAGES
        .iter()
        .position(|lang| lang.name == token)
        .or_else(|| {
            LANGUAGES
                .iter()
                .position(|lang| lang.aliases.contains(&token.as_str()))
        })
        .or_else(|| {
            LANGUAGES
                .iter()
                .position(|lang| lang.extensions.contains(&token.as_str()))
        })?;
    config_at(index)
}

/// The configuration a `#!` line asks for, if `line` is one.
///
/// Takes the last path segment of the interpreter, then drops a trailing
/// version (`python3`, `ruby2.7`), so `#!/usr/bin/env python3` resolves.
pub(crate) fn config_for_shebang(line: &str) -> Option<&'static HighlightConfiguration> {
    let rest = line.strip_prefix("#!")?;
    let mut words = rest.split_whitespace();
    let mut interpreter = words.next()?;
    if interpreter.ends_with("env") {
        // `#!/usr/bin/env -S deno run` — skip the flags `env` was given.
        interpreter = words.find(|word| !word.starts_with('-'))?;
    }
    let interpreter = interpreter.rsplit('/').next()?;
    let stem = interpreter.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    config_for_token(stem).or_else(|| config_for_token(interpreter))
}

/// Look up the injection callback's language, resolving the aliases
/// grammars use for languages we hold under another name.
pub(crate) fn injection_config(name: &str) -> Option<&'static HighlightConfiguration> {
    config_for_token(name)
}

/// Build a configuration, tolerating a base query the grammar has outgrown.
///
/// `highlights` is ordered from most general to most specific: a grammar that
/// extends another (C++ on C, TypeScript on JavaScript, SCSS on CSS) is
/// highlighted by the base query plus its own. If the pair no longer compiles
/// against this grammar the base is dropped rather than losing the language.
fn build(
    name: &'static str,
    language: Language,
    highlights: &[&str],
    injections: &str,
) -> Option<HighlightConfiguration> {
    let mut config =
        compile(name, &language, &highlights.join("\n"), injections).or_else(|| {
            let own = highlights.last()?;
            (highlights.len() > 1)
                .then(|| compile(name, &language, own, injections))
                .flatten()
        })?;
    // Locals tracking is deliberately left off: it suppresses highlights for
    // identifiers that resolve to a local definition, and a diff hunk rarely
    // contains the definition, so the same identifier would be coloured in
    // one hunk and plain in the next.
    config.configure(&names::names());
    Some(config)
}

fn compile(
    name: &'static str,
    language: &Language,
    highlights: &str,
    injections: &str,
) -> Option<HighlightConfiguration> {
    match HighlightConfiguration::new(language.clone(), name, highlights, injections, "") {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!("syntax: {name} highlight query rejected: {err}");
            None
        }
    }
}

macro_rules! languages {
    ($($name:literal {
        aliases: [$($alias:literal),* $(,)?],
        extensions: [$($ext:literal),* $(,)?],
        filenames: [$($file:literal),* $(,)?],
        build: $build:expr,
    })*) => {
        static LANGUAGES: &[LanguageDef] = &[
            $(LanguageDef {
                name: $name,
                aliases: &[$($alias),*],
                extensions: &[$($ext),*],
                filenames: &[$($file),*],
                build: || {
                    let (language, highlights, injections) = $build;
                    build($name, language.into(), &highlights, injections)
                },
            }),*
        ];
    };
}

languages! {
    "bash" {
        aliases: ["sh", "shell", "zsh", "ksh"],
        extensions: ["sh", "bash", "zsh", "ksh", "ash", "envrc", "bats"],
        filenames: [".bashrc", ".bash_profile", ".zshrc", ".profile", "PKGBUILD", ".envrc"],
        build: (
            tree_sitter_bash::LANGUAGE,
            [tree_sitter_bash::HIGHLIGHT_QUERY],
            "",
        ),
    }
    "c" {
        aliases: [],
        extensions: ["c", "h"],
        filenames: [],
        build: (
            tree_sitter_c::LANGUAGE,
            [tree_sitter_c::HIGHLIGHT_QUERY],
            "",
        ),
    }
    "c_sharp" {
        aliases: ["csharp", "cs"],
        extensions: ["cs", "csx"],
        filenames: [],
        build: (
            tree_sitter_c_sharp::LANGUAGE,
            [tree_sitter_c_sharp::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "cmake" {
        aliases: [],
        extensions: ["cmake"],
        filenames: ["CMakeLists.txt"],
        build: (
            tree_sitter_cmake::LANGUAGE,
            [tree_sitter_cmake::HIGHLIGHTS_QUERY],
            tree_sitter_cmake::INJECTIONS_QUERY,
        ),
    }
    "cpp" {
        aliases: ["c++", "cuda"],
        extensions: ["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "cu", "cuh", "ino"],
        filenames: [],
        build: (
            tree_sitter_cpp::LANGUAGE,
            [tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY],
            "",
        ),
    }
    "css" {
        aliases: [],
        extensions: ["css"],
        filenames: [],
        build: (
            tree_sitter_css::LANGUAGE,
            [tree_sitter_css::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "dart" {
        aliases: [],
        extensions: ["dart"],
        filenames: [],
        build: (
            tree_sitter_dart::LANGUAGE,
            [tree_sitter_dart::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "diff" {
        aliases: ["patch"],
        extensions: ["diff", "patch"],
        filenames: [],
        build: (
            tree_sitter_diff::LANGUAGE,
            [tree_sitter_diff::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "dockerfile" {
        aliases: ["docker", "containerfile"],
        extensions: ["dockerfile", "containerfile"],
        filenames: ["Dockerfile", "Containerfile", "dockerfile", "containerfile"],
        build: (
            tree_sitter_containerfile::LANGUAGE,
            [tree_sitter_containerfile::HIGHLIGHTS_QUERY],
            tree_sitter_containerfile::INJECTIONS_QUERY,
        ),
    }
    "elixir" {
        aliases: ["ex"],
        extensions: ["ex", "exs"],
        filenames: ["mix.lock"],
        build: (
            tree_sitter_elixir::LANGUAGE,
            [tree_sitter_elixir::HIGHLIGHTS_QUERY],
            tree_sitter_elixir::INJECTIONS_QUERY,
        ),
    }
    "elm" {
        aliases: [],
        extensions: ["elm"],
        filenames: [],
        build: (
            tree_sitter_elm::LANGUAGE,
            [tree_sitter_elm::HIGHLIGHTS_QUERY],
            tree_sitter_elm::INJECTIONS_QUERY,
        ),
    }
    "erb" {
        aliases: ["eruby", "embedded_template"],
        extensions: ["erb", "rhtml"],
        filenames: [],
        build: (
            tree_sitter_embedded_template::LANGUAGE,
            [tree_sitter_embedded_template::HIGHLIGHTS_QUERY],
            tree_sitter_embedded_template::INJECTIONS_ERB_QUERY,
        ),
    }
    "ejs" {
        aliases: [],
        extensions: ["ejs"],
        filenames: [],
        build: (
            tree_sitter_embedded_template::LANGUAGE,
            [tree_sitter_embedded_template::HIGHLIGHTS_QUERY],
            tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
        ),
    }
    "erlang" {
        aliases: ["erl"],
        extensions: ["erl", "hrl", "escript"],
        filenames: ["rebar.config"],
        build: (
            tree_sitter_erlang::LANGUAGE,
            [tree_sitter_erlang::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "fish" {
        aliases: [],
        extensions: ["fish"],
        filenames: [],
        build: (
            tree_sitter_fish::language(),
            [tree_sitter_fish::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "glsl" {
        aliases: [],
        extensions: ["glsl", "vert", "frag", "geom", "comp", "tesc", "tese", "vs", "fs"],
        filenames: [],
        build: (
            tree_sitter_glsl::LANGUAGE_GLSL,
            [tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_glsl::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "go" {
        aliases: ["golang"],
        extensions: ["go"],
        filenames: [],
        build: (
            tree_sitter_go::LANGUAGE,
            [tree_sitter_go::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "graphql" {
        aliases: ["gql"],
        extensions: ["graphql", "graphqls", "gql"],
        filenames: [],
        build: (
            tree_sitter_graphql::LANGUAGE,
            [include_str!("queries/graphql.scm")],
            "",
        ),
    }
    "groovy" {
        aliases: ["gradle"],
        extensions: ["groovy", "gradle", "gvy", "jenkinsfile"],
        filenames: ["Jenkinsfile", "jenkinsfile"],
        build: (
            tree_sitter_groovy::LANGUAGE,
            [
                tree_sitter_java::HIGHLIGHTS_QUERY,
                include_str!("queries/groovy.scm"),
            ],
            "",
        ),
    }
    "haskell" {
        aliases: ["hs"],
        extensions: ["hs", "hs-boot"],
        filenames: [],
        build: (
            tree_sitter_haskell::LANGUAGE,
            [tree_sitter_haskell::HIGHLIGHTS_QUERY],
            tree_sitter_haskell::INJECTIONS_QUERY,
        ),
    }
    "hcl" {
        aliases: ["terraform", "tf"],
        extensions: ["hcl", "tf", "tfvars", "nomad", "pkr"],
        filenames: [],
        build: (
            tree_sitter_hcl::LANGUAGE,
            [include_str!("queries/hcl.scm")],
            "",
        ),
    }
    "heex" {
        aliases: ["eex"],
        extensions: ["heex", "eex", "leex"],
        filenames: [],
        build: (
            tree_sitter_heex::LANGUAGE,
            [tree_sitter_heex::HIGHLIGHTS_QUERY],
            tree_sitter_heex::INJECTIONS_QUERY,
        ),
    }
    "html" {
        aliases: ["vue", "astro", "xhtml", "handlebars", "hbs"],
        extensions: [
            "html", "htm", "xhtml", "vue", "astro", "hbs", "handlebars", "mustache", "njk",
            "jinja", "jinja2", "twig",
        ],
        filenames: [],
        build: (
            tree_sitter_html::LANGUAGE,
            [tree_sitter_html::HIGHLIGHTS_QUERY],
            tree_sitter_html::INJECTIONS_QUERY,
        ),
    }
    "ini" {
        aliases: ["cfg", "conf", "properties", "toml_ini"],
        extensions: ["ini", "cfg", "conf", "properties", "desktop", "service"],
        filenames: [
            ".editorconfig", ".gitconfig", ".gitmodules", ".npmrc", ".flake8", ".coveragerc",
        ],
        build: (
            tree_sitter_ini::LANGUAGE,
            [tree_sitter_ini::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "java" {
        aliases: [],
        extensions: ["java"],
        filenames: [],
        build: (
            tree_sitter_java::LANGUAGE,
            [tree_sitter_java::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "javascript" {
        aliases: ["js", "node", "bun", "ecmascript"],
        extensions: ["js", "mjs", "cjs"],
        filenames: [],
        build: (
            tree_sitter_javascript::LANGUAGE,
            [tree_sitter_javascript::HIGHLIGHT_QUERY],
            tree_sitter_javascript::INJECTIONS_QUERY,
        ),
    }
    "jsx" {
        aliases: [],
        extensions: ["jsx"],
        filenames: [],
        build: (
            tree_sitter_javascript::LANGUAGE,
            [
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            ],
            tree_sitter_javascript::INJECTIONS_QUERY,
        ),
    }
    "json" {
        aliases: ["jsonc", "json5"],
        extensions: ["json", "jsonc", "json5", "jsonl", "ndjson", "avsc", "webmanifest"],
        filenames: [
            ".eslintrc", ".babelrc", ".prettierrc", "composer.lock", "flake.lock",
        ],
        build: (
            tree_sitter_json::LANGUAGE,
            [tree_sitter_json::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "jsonnet" {
        aliases: [],
        extensions: ["jsonnet", "libsonnet"],
        filenames: [],
        build: (
            tree_sitter_jsonnet::LANGUAGE,
            [tree_sitter_jsonnet::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "kotlin" {
        aliases: ["kt"],
        extensions: ["kt", "kts", "ktm"],
        filenames: [],
        build: (
            tree_sitter_kotlin_ng::LANGUAGE,
            [include_str!("queries/kotlin.scm")],
            "",
        ),
    }
    "lua" {
        aliases: [],
        extensions: ["lua", "luau", "rockspec"],
        filenames: [".luacheckrc"],
        build: (
            tree_sitter_lua::LANGUAGE,
            [tree_sitter_lua::HIGHLIGHTS_QUERY],
            tree_sitter_lua::INJECTIONS_QUERY,
        ),
    }
    "make" {
        aliases: ["makefile", "just", "justfile"],
        extensions: ["mk", "mak", "make"],
        filenames: ["Makefile", "makefile", "GNUmakefile", "Justfile", "justfile"],
        build: (
            tree_sitter_make::LANGUAGE,
            [tree_sitter_make::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "markdown" {
        aliases: ["md", "mdx"],
        extensions: ["md", "markdown", "mdx", "mkd", "mdown"],
        filenames: [],
        build: (
            tree_sitter_md::LANGUAGE,
            [tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
            tree_sitter_md::INJECTION_QUERY_BLOCK,
        ),
    }
    "markdown_inline" {
        aliases: ["markdown.inline"],
        extensions: [],
        filenames: [],
        build: (
            tree_sitter_md::INLINE_LANGUAGE,
            [tree_sitter_md::HIGHLIGHT_QUERY_INLINE],
            tree_sitter_md::INJECTION_QUERY_INLINE,
        ),
    }
    "nix" {
        aliases: [],
        extensions: ["nix"],
        filenames: [],
        build: (
            tree_sitter_nix::LANGUAGE,
            [tree_sitter_nix::HIGHLIGHTS_QUERY],
            tree_sitter_nix::INJECTIONS_QUERY,
        ),
    }
    "ocaml" {
        aliases: ["ml"],
        // `.mli` signatures get the implementation grammar: the crate's one
        // highlight query does not compile against the interface grammar, and
        // an error-tolerant parse of a signature still finds its keywords,
        // strings and comments.
        extensions: ["ml", "mli"],
        filenames: [],
        build: (
            tree_sitter_ocaml::LANGUAGE_OCAML,
            [tree_sitter_ocaml::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "php" {
        aliases: [],
        extensions: ["php", "phtml", "php3", "php4", "php5"],
        filenames: [],
        build: (
            tree_sitter_php::LANGUAGE_PHP,
            [tree_sitter_php::HIGHLIGHTS_QUERY],
            tree_sitter_php::INJECTIONS_QUERY,
        ),
    }
    "php_only" {
        aliases: [],
        extensions: [],
        filenames: [],
        build: (
            tree_sitter_php::LANGUAGE_PHP_ONLY,
            [tree_sitter_php::HIGHLIGHTS_QUERY],
            tree_sitter_php::INJECTIONS_QUERY,
        ),
    }
    "powershell" {
        aliases: ["ps1", "pwsh"],
        extensions: ["ps1", "psm1", "psd1"],
        filenames: [],
        build: (
            tree_sitter_powershell::LANGUAGE,
            [tree_sitter_powershell::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "proto" {
        aliases: ["protobuf"],
        extensions: ["proto"],
        filenames: [],
        build: (
            tree_sitter_proto::LANGUAGE,
            [include_str!("queries/proto.scm")],
            "",
        ),
    }
    "python" {
        aliases: ["py", "python3"],
        extensions: ["py", "pyi", "pyw", "bzl"],
        filenames: ["SConstruct", "SConscript", "wscript", "BUILD", "BUILD.bazel", "WORKSPACE"],
        build: (
            tree_sitter_python::LANGUAGE,
            [tree_sitter_python::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "r" {
        aliases: ["rscript"],
        extensions: ["r", "rprofile"],
        filenames: [".Rprofile", ".Rprofile.site"],
        build: (
            tree_sitter_r::LANGUAGE,
            [tree_sitter_r::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "regex" {
        aliases: [],
        extensions: [],
        filenames: [],
        build: (
            tree_sitter_regex::LANGUAGE,
            [tree_sitter_regex::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "ruby" {
        aliases: ["rb"],
        extensions: ["rb", "rake", "gemspec", "ru", "podspec", "thor"],
        filenames: [
            "Gemfile", "Rakefile", "Guardfile", "Capfile", "Vagrantfile", "Podfile", "Brewfile",
        ],
        build: (
            tree_sitter_ruby::LANGUAGE,
            [tree_sitter_ruby::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "rust" {
        aliases: ["rs"],
        extensions: ["rs"],
        filenames: [],
        build: (
            tree_sitter_rust::LANGUAGE,
            [tree_sitter_rust::HIGHLIGHTS_QUERY],
            tree_sitter_rust::INJECTIONS_QUERY,
        ),
    }
    "scala" {
        aliases: ["sbt"],
        extensions: ["scala", "sbt", "sc"],
        filenames: [],
        build: (
            tree_sitter_scala::LANGUAGE,
            [tree_sitter_scala::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "scss" {
        aliases: ["sass"],
        extensions: ["scss", "sass"],
        filenames: [],
        build: (
            tree_sitter_scss::language(),
            [tree_sitter_css::HIGHLIGHTS_QUERY, tree_sitter_scss::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "sql" {
        aliases: ["mysql", "postgresql", "psql", "sequel"],
        extensions: ["sql", "mysql", "pgsql", "ddl", "dml"],
        filenames: [],
        build: (
            tree_sitter_sequel::LANGUAGE,
            [tree_sitter_sequel::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "svelte" {
        aliases: [],
        extensions: ["svelte"],
        filenames: [],
        build: (
            tree_sitter_svelte_ng::LANGUAGE,
            [tree_sitter_html::HIGHLIGHTS_QUERY, tree_sitter_svelte_ng::HIGHLIGHTS_QUERY],
            tree_sitter_svelte_ng::INJECTIONS_QUERY,
        ),
    }
    "swift" {
        aliases: [],
        extensions: ["swift"],
        filenames: [],
        build: (
            tree_sitter_swift::LANGUAGE,
            [tree_sitter_swift::HIGHLIGHTS_QUERY],
            tree_sitter_swift::INJECTIONS_QUERY,
        ),
    }
    "toml" {
        aliases: [],
        extensions: ["toml"],
        filenames: ["Cargo.lock", "Pipfile", "poetry.lock", "uv.lock", "Gopkg.lock"],
        build: (
            tree_sitter_toml_ng::LANGUAGE,
            [tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "typescript" {
        aliases: ["ts", "deno"],
        extensions: ["ts", "mts", "cts"],
        filenames: [],
        build: (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            [
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
            tree_sitter_javascript::INJECTIONS_QUERY,
        ),
    }
    "tsx" {
        aliases: [],
        extensions: ["tsx"],
        filenames: [],
        build: (
            tree_sitter_typescript::LANGUAGE_TSX,
            [
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
            tree_sitter_javascript::INJECTIONS_QUERY,
        ),
    }
    "vim" {
        aliases: ["viml"],
        extensions: ["vim", "vimrc"],
        filenames: [".vimrc", "_vimrc", ".gvimrc"],
        build: (
            tree_sitter_vim::language(),
            [tree_sitter_vim::HIGHLIGHTS_QUERY],
            tree_sitter_vim::INJECTIONS_QUERY,
        ),
    }
    "xml" {
        aliases: ["svg", "plist"],
        extensions: [
            "xml", "svg", "xsl", "xslt", "xsd", "plist", "csproj", "vbproj", "fsproj", "vcxproj",
            "props", "targets", "resx", "wsdl", "rss", "atom", "storyboard", "xib",
        ],
        filenames: [],
        build: (
            tree_sitter_xml::LANGUAGE_XML,
            [tree_sitter_xml::XML_HIGHLIGHT_QUERY],
            "",
        ),
    }
    "yaml" {
        aliases: ["yml"],
        extensions: ["yaml", "yml"],
        filenames: [".clang-format", ".clang-tidy"],
        build: (
            tree_sitter_yaml::LANGUAGE,
            [tree_sitter_yaml::HIGHLIGHTS_QUERY],
            "",
        ),
    }
    "zig" {
        aliases: [],
        extensions: ["zig", "zon"],
        filenames: [],
        build: (
            tree_sitter_zig::LANGUAGE,
            [tree_sitter_zig::HIGHLIGHTS_QUERY],
            tree_sitter_zig::INJECTIONS_QUERY,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled grammar must produce a working configuration: a query
    /// that fails to compile is a silent loss of the whole language, and the
    /// two we wrote ourselves have nothing upstream to catch a typo.
    #[test]
    fn every_grammar_should_build_a_configuration() {
        let failed: Vec<&str> = LANGUAGES
            .iter()
            .enumerate()
            .filter(|(index, _)| config_at(*index).is_none())
            .map(|(_, lang)| lang.name)
            .collect();
        assert!(
            failed.is_empty(),
            "no highlight configuration for {failed:?}; run with RUST_LOG=warn for the query errors"
        );
    }

    /// A grammar that extends another (C++ on C, TypeScript on JavaScript) is
    /// configured with both queries, and `build` drops the base if the pair
    /// stops compiling — which costs most of the highlighting. The base
    /// carries far more patterns than the extension, so a collapsed pair
    /// shows up as a pattern count near the extension's own.
    #[test]
    fn derived_grammars_should_keep_their_base_query() {
        // Minimums sit well above what each grammar's own query contributes
        // (cpp 12, typescript 7, tsx 7, scss 20, svelte 10, glsl 69).
        for (name, minimum) in [
            ("cpp", 40),
            ("typescript", 20),
            ("tsx", 25),
            ("scss", 40),
            ("svelte", 15),
            ("glsl", 100),
        ] {
            let patterns = config_for_token(name)
                .expect("configuration")
                .query
                .pattern_count();
            assert!(
                patterns >= minimum,
                "{name} has only {patterns} patterns; its base query was dropped"
            );
        }
    }

    #[test]
    fn should_match_languages_by_extension_and_filename() {
        assert!(config_for_path(Path::new("src/main.rs")).is_some());
        assert!(config_for_path(Path::new("SRC/MAIN.RS")).is_some());
        assert!(config_for_path(Path::new("Makefile")).is_some());
        assert!(config_for_path(Path::new("Cargo.lock")).is_some());
        assert!(config_for_path(Path::new("Dockerfile")).is_some());
        assert!(config_for_path(Path::new("notes.unknownext")).is_none());
    }

    #[test]
    fn should_match_language_tokens_and_aliases() {
        for token in ["rust", "rs", "ts", "tsx", "sh", "yml", "c++", "golang"] {
            assert!(
                config_for_token(token).is_some(),
                "no configuration for token {token}"
            );
        }
        assert!(config_for_token("").is_none());
        assert!(config_for_token("not-a-language").is_none());
    }

    #[test]
    fn should_resolve_shebangs() {
        for line in [
            "#!/usr/bin/env python3",
            "#!/bin/sh",
            "#!/usr/bin/env -S deno run",
            "#!/usr/bin/ruby",
        ] {
            assert!(
                config_for_shebang(line).is_some(),
                "no configuration for {line}"
            );
        }
        assert!(config_for_shebang("not a shebang").is_none());
        assert!(config_for_shebang("#!/usr/bin/env nonsense").is_none());
    }

    /// An extension or file name must not be claimed twice: the first match
    /// wins, so a duplicate silently shadows a language. (Extensions and file
    /// names are separate namespaces — `foo.dockerfile` and a file called
    /// `dockerfile` both need an entry.)
    #[test]
    fn language_claims_should_not_overlap() {
        let namespaces: [fn(&LanguageDef) -> &'static [&'static str]; 2] =
            [|l| l.extensions, |l| l.filenames];
        for claims in namespaces {
            let mut seen: Vec<(&str, &str)> = Vec::new();
            for lang in LANGUAGES {
                let name = lang.name;
                for claim in claims(lang) {
                    if let Some((other, _)) = seen.iter().find(|(c, _)| c == claim) {
                        panic!("{claim:?} is claimed by both {other} and {name}");
                    }
                    seen.push((claim, name));
                }
            }
        }
    }

    /// A file name entry that an extension already resolves is dead weight,
    /// since extensions are matched first.
    #[test]
    fn filenames_should_not_duplicate_extension_matches() {
        for lang in LANGUAGES {
            for name in lang.filenames {
                let by_extension = Path::new(name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .is_some_and(|e| LANGUAGES.iter().any(|l| l.extensions.contains(&e.as_str())));
                assert!(
                    !by_extension,
                    "{name:?} in {} already resolves by extension",
                    lang.name
                );
            }
        }
    }

    #[test]
    fn extensions_should_be_lowercase() {
        for lang in LANGUAGES {
            for ext in lang.extensions {
                assert_eq!(
                    *ext,
                    ext.to_ascii_lowercase(),
                    "{} lists a non-lowercase extension",
                    lang.name
                );
            }
        }
    }
}
