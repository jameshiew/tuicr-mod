//! CLI argument parsing, backed by `clap`.
//!
//! The struct [`Cli`] is the clap-derived parser; [`CliArgs`] is the simple
//! POJO the rest of the binary consumes. Conversion lives in `From<Cli>`.

use clap::{ArgAction, Args, Parser, Subcommand};

use crate::theme::{AppearanceArg, ThemeArg};

/// CLI arguments consumed by the rest of the binary.
#[derive(Debug, Clone, Default)]
pub struct CliArgs {
    pub theme: Option<String>,
    pub appearance: Option<AppearanceArg>,
    /// Output to stdout instead of clipboard when exporting.
    pub output_to_stdout: bool,
    /// Commit/revision range to review.
    pub revisions: Option<String>,
    /// Skip commit selector and review uncommitted changes directly.
    pub working_tree: bool,
    /// Filter diff to a specific file or directory path.
    pub path_filter: Option<String>,
    /// Open a single file or directory for annotation (no VCS required).
    pub file_path: Option<String>,
    /// Whole-repo annotation mode.
    pub all_files: bool,
}

#[derive(Parser, Debug)]
#[command(
    name = "tuicr",
    version,
    about = "A local diff review TUI with vim keybindings.",
    after_help = "Press ? in the application for keybinding help.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    tui_options: TuiOptions,

    #[command(subcommand)]
    command: Option<Subcmd>,
}

/// Options that launch or configure the interactive TUI.
#[derive(Args, Debug, Clone, Default)]
struct TuiOptions {
    /// Commit range / revset to review (syntax depends on VCS backend).
    #[arg(
        short = 'r',
        long = "revisions",
        value_name = "REVSET",
        allow_hyphen_values = true
    )]
    revisions: Option<String>,

    /// Color theme to use. Bundled themes resolve first; local themes are
    /// loaded from the config `themes/` directory.
    #[arg(long, value_name = "THEME", value_parser = non_empty_theme_name)]
    theme: Option<String>,

    /// Appearance mode (light/dark/system); used when no explicit theme is set.
    #[arg(long, value_name = "MODE", value_parser = parse_appearance_arg)]
    appearance: Option<AppearanceArg>,

    /// Filter diff to a specific file or directory.
    #[arg(
        short = 'p',
        long = "path",
        value_name = "PATH",
        value_parser = non_empty_path,
        conflicts_with_all = ["file_path", "all_files"],
    )]
    path_filter: Option<String>,

    /// Include uncommitted changes (skip commit selector when used alone;
    /// combine with commits when used with -r).
    #[arg(
        short = 'w',
        long = "working-tree",
        action = ArgAction::SetTrue,
        conflicts_with_all = ["file_path", "all_files"],
    )]
    working_tree: bool,

    /// Open a file or directory for annotation (no VCS required).
    #[arg(
        long = "file",
        value_name = "PATH",
        value_parser = non_empty_path,
        conflicts_with_all = ["path_filter", "revisions", "working_tree", "all_files"],
    )]
    file_path: Option<String>,

    /// Review every tracked file in the cwd's git repo.
    #[arg(
        short = 'A',
        long = "all-files",
        action = ArgAction::SetTrue,
        conflicts_with_all = ["path_filter", "revisions", "working_tree", "file_path"],
    )]
    all_files: bool,

    /// Output to stdout instead of clipboard when exporting.
    #[arg(long = "stdout", action = ArgAction::SetTrue)]
    stdout: bool,
}

#[derive(Subcommand, Debug)]
enum Subcmd {
    /// Open the interactive TUI.
    Tui(TuiOptions),
}

impl From<Cli> for CliArgs {
    fn from(cli: Cli) -> Self {
        let options = match cli.command {
            Some(Subcmd::Tui(command_options)) => cli.tui_options.merge(command_options),
            None => cli.tui_options,
        };
        Self {
            theme: options.theme,
            appearance: options.appearance,
            output_to_stdout: options.stdout,
            revisions: options.revisions,
            working_tree: options.working_tree,
            path_filter: options.path_filter,
            file_path: options.file_path,
            all_files: options.all_files,
        }
    }
}

impl TuiOptions {
    fn merge(self, later: TuiOptions) -> Self {
        Self {
            theme: later.theme.or(self.theme),
            appearance: later.appearance.or(self.appearance),
            stdout: self.stdout || later.stdout,
            revisions: later.revisions.or(self.revisions),
            working_tree: self.working_tree || later.working_tree,
            path_filter: later.path_filter.or(self.path_filter),
            file_path: later.file_path.or(self.file_path),
            all_files: self.all_files || later.all_files,
        }
    }
}

fn parse_appearance_arg(s: &str) -> Result<AppearanceArg, String> {
    AppearanceArg::parse_name(s).ok_or_else(|| {
        let valid = AppearanceArg::valid_values_display();
        format!("Unknown appearance '{s}'. Valid options: {valid}")
    })
}

fn non_empty_theme_name(s: &str) -> Result<String, String> {
    if s.is_empty() {
        let valid = ThemeArg::valid_values_display();
        Err(format!("--theme requires a value ({valid})"))
    } else {
        Ok(s.to_string())
    }
}

fn non_empty_path(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("a file or directory path is required".to_string())
    } else {
        Ok(s.to_string())
    }
}

/// Parse CLI arguments from `std::env::args`. On `--help`/`--version`/parse
/// errors, clap prints to stdout/stderr and exits the process.
pub fn parse_cli_args() -> CliArgs {
    match Cli::try_parse() {
        Ok(cli) => cli.into(),
        Err(err) => err.exit(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse_for_test(args: &[&str]) -> Result<CliArgs, clap::Error> {
        Cli::try_parse_from(args).map(CliArgs::from)
    }

    #[test]
    fn should_parse_theme_when_provided() {
        let parsed = parse_for_test(&["tuicr", "--theme", "light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("light".to_string()));
    }

    #[test]
    fn should_parse_catppuccin_themes() {
        let parsed = parse_for_test(&["tuicr", "--theme", "catppuccin-mocha"])
            .expect("parse should succeed");
        assert_eq!(parsed.theme, Some("catppuccin-mocha".to_string()));

        let parsed =
            parse_for_test(&["tuicr", "--theme=catppuccin-latte"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("catppuccin-latte".to_string()));
    }

    #[test]
    fn should_parse_ayu_light_theme() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "ayu-light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("ayu-light".to_string()));
    }

    #[test]
    fn should_parse_onedark_theme() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "onedark"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("onedark".to_string()));
    }

    #[test]
    fn should_parse_gruvbox_themes() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "gruvbox-dark"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("gruvbox-dark".to_string()));

        let parsed =
            parse_for_test(&["tuicr", "--theme=gruvbox-light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("gruvbox-light".to_string()));
    }

    #[test]
    fn should_parse_everforest_themes() {
        let parsed =
            parse_for_test(&["tuicr", "--theme", "everforest-dark"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("everforest-dark".to_string()));

        let parsed =
            parse_for_test(&["tuicr", "--theme=everforest-light"]).expect("parse should succeed");
        assert_eq!(parsed.theme, Some("everforest-light".to_string()));
    }

    #[test]
    fn should_leave_theme_none_when_not_provided() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(parsed.theme, None);
    }

    #[test]
    fn should_parse_working_tree_short_flag() {
        let parsed = parse_for_test(&["tuicr", "-w"]).expect("parse should succeed");
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_parse_working_tree_long_flag() {
        let parsed = parse_for_test(&["tuicr", "--working-tree"]).expect("parse should succeed");
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_default_working_tree_to_false() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert!(!parsed.working_tree);
    }

    #[test]
    fn should_parse_working_tree_with_revisions() {
        let parsed =
            parse_for_test(&["tuicr", "-w", "-r", "HEAD~3..HEAD"]).expect("parse should succeed");
        assert!(parsed.working_tree);
        assert_eq!(parsed.revisions, Some("HEAD~3..HEAD".to_string()));
    }

    #[test]
    fn should_allow_custom_theme_name_in_separate_arg() {
        let parsed = parse_for_test(&["tuicr", "--theme", "tuicr-teal"])
            .expect("custom theme parse should succeed");
        assert_eq!(parsed.theme, Some("tuicr-teal".to_string()));
    }

    #[test]
    fn should_allow_custom_theme_name_in_equals_arg() {
        let parsed = parse_for_test(&["tuicr", "--theme=tuicr-teal"])
            .expect("custom theme parse should succeed");
        assert_eq!(parsed.theme, Some("tuicr-teal".to_string()));
    }

    #[test]
    fn should_error_when_theme_value_missing() {
        let err = parse_for_test(&["tuicr", "--theme"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn should_parse_appearance_when_provided() {
        let parsed =
            parse_for_test(&["tuicr", "--appearance", "system"]).expect("parse should succeed");
        assert_eq!(parsed.appearance, Some(AppearanceArg::System));
    }

    #[test]
    fn should_error_for_invalid_appearance() {
        let err =
            parse_for_test(&["tuicr", "--appearance", "nope"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().contains("Unknown appearance 'nope'"));
    }

    #[test]
    fn should_parse_path_short_flag() {
        let parsed = parse_for_test(&["tuicr", "-p", "src/main.rs"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("src/main.rs".to_string()));
    }

    #[test]
    fn should_parse_path_long_flag() {
        let parsed = parse_for_test(&["tuicr", "--path", "src/"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("src/".to_string()));
    }

    #[test]
    fn should_parse_path_equals_syntax() {
        let parsed = parse_for_test(&["tuicr", "--path=plans/current-plan.md"])
            .expect("parse should succeed");
        assert_eq!(
            parsed.path_filter,
            Some("plans/current-plan.md".to_string())
        );
    }

    #[test]
    fn should_error_when_path_value_missing() {
        let err = parse_for_test(&["tuicr", "--path"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn should_error_when_path_equals_empty() {
        let err = parse_for_test(&["tuicr", "--path="]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn should_default_path_filter_to_none() {
        let parsed = parse_for_test(&["tuicr"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, None);
    }

    #[test]
    fn should_parse_path_with_working_tree() {
        let parsed =
            parse_for_test(&["tuicr", "-p", "file.md", "-w"]).expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("file.md".to_string()));
        assert!(parsed.working_tree);
    }

    #[test]
    fn should_parse_path_with_revisions() {
        let parsed = parse_for_test(&["tuicr", "--path", "src/", "-r", "HEAD~3.."])
            .expect("parse should succeed");
        assert_eq!(parsed.path_filter, Some("src/".to_string()));
        assert_eq!(parsed.revisions, Some("HEAD~3..".to_string()));
    }

    #[test]
    fn should_reject_file_combined_with_path() {
        let err = parse_for_test(&["tuicr", "--file", "f.md", "--path", "src/"])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_file_combined_with_revisions() {
        let err = parse_for_test(&["tuicr", "--file", "f.md", "-r", "HEAD~1.."])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_file_combined_with_working_tree() {
        let err =
            parse_for_test(&["tuicr", "--file", "f.md", "-w"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_all_files_combined_with_path() {
        let err =
            parse_for_test(&["tuicr", "-A", "--path", "src/"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_reject_all_files_combined_with_file() {
        let err =
            parse_for_test(&["tuicr", "-A", "--file", "f.md"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn should_parse_all_files_short_flag() {
        let parsed = parse_for_test(&["tuicr", "-A"]).expect("parse should succeed");
        assert!(parsed.all_files);
    }

    #[test]
    fn should_parse_all_files_long_flag() {
        let parsed = parse_for_test(&["tuicr", "--all-files"]).expect("parse should succeed");
        assert!(parsed.all_files);
    }

    #[test]
    fn should_parse_stdout_flag() {
        let parsed = parse_for_test(&["tuicr", "--stdout"]).expect("parse should succeed");
        assert!(parsed.output_to_stdout);
    }

    #[test]
    fn should_reject_removed_update_interfaces() {
        let update = parse_for_test(&["tuicr", "update"]).expect_err("parse should fail");
        assert_eq!(update.kind(), ErrorKind::InvalidSubcommand);

        let flag = parse_for_test(&["tuicr", "--no-update-check"]).expect_err("parse should fail");
        assert_eq!(flag.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn should_parse_explicit_tui_command() {
        let parsed = parse_for_test(&["tuicr", "tui", "-w", "--theme", "dark"])
            .expect("parse should succeed");
        assert!(parsed.working_tree);
        assert_eq!(parsed.theme, Some("dark".to_string()));
    }

    #[test]
    fn should_reject_removed_pr_subcommand() {
        let err = parse_for_test(&["tuicr", "pr", "125"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn should_reject_removed_review_subcommand() {
        let err = parse_for_test(&["tuicr", "review", "list"]).expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn should_reject_removed_repo_url_flag() {
        let err = parse_for_test(&["tuicr", "--repo-url", "https://github.com/a/b"])
            .expect_err("parse should fail");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }
}
