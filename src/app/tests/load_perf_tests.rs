//! Measures how long a large working-tree diff takes to open, from the git
//! backend through to the first rendered frame, so load cost can be compared
//! across branches.
//!
//! `cargo test --release load_perf -- --ignored --nocapture`

use crate::app::*;
use crate::vcs::{DiffWhitespaceMode, GitBackend, GitBackendPreference, VcsBackend};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::fs;
use std::path::Path;
use std::time::Instant;

fn run_git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultRefFormat=files",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn line_text(file: usize, i: usize) -> String {
    match i % 6 {
        0 => format!("// comment line {i} in file {file}"),
        1 => format!("pub fn function_{i}(input: &str, count: usize) -> Result<String, Error> {{"),
        2 => format!("    let value_{i} = compute(input, {i}).map_err(|e| Error::from(e))?;"),
        3 => {
            format!("    if value_{i}.len() > count {{ return Ok(format!(\"{{}}\", value_{i})); }}")
        }
        4 => format!("    Ok(value_{i}.to_string())"),
        _ => "}".to_string(),
    }
}

fn file_content(file: usize, lines: usize, edited: bool) -> String {
    let mut out = String::new();
    for i in 0..lines {
        let mut text = line_text(file, i);
        if edited && i % 8 == 4 {
            text.push_str(" // edited");
        }
        out.push_str(&text);
        out.push('\n');
        if edited && i == lines / 2 {
            for j in 0..20 {
                out.push_str(&format!("    let inserted_{j} = {j};\n"));
            }
        }
    }
    out
}

/// A repo with `file_count` committed Rust files of `lines_per_file` lines,
/// every eighth line edited and a block inserted in the middle of each, plus
/// two untracked files: a large log and one over the untracked size cap.
fn build_repo(dir: &Path, file_count: usize, lines_per_file: usize) {
    run_git(dir, &["init", "-q"]);
    run_git(dir, &["config", "user.name", "Tuicr Bench"]);
    run_git(dir, &["config", "user.email", "bench@example.com"]);
    for f in 0..file_count {
        let sub = dir.join(format!("src/module_{}", f % 10));
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join(format!("file_{f}.rs")),
            file_content(f, lines_per_file, false),
        )
        .unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", "base"]);
    for f in 0..file_count {
        let sub = dir.join(format!("src/module_{}", f % 10));
        fs::write(
            sub.join(format!("file_{f}.rs")),
            file_content(f, lines_per_file, true),
        )
        .unwrap();
    }
    let mut log = String::new();
    for i in 0..10_000 {
        log.push_str(&format!(
            "2026-01-01T00:00:{:02}Z INFO request {i} handled in {}ms\n",
            i % 60,
            i % 97
        ));
    }
    fs::write(dir.join("server.log"), log).unwrap();
    fs::write(dir.join("huge.bin.txt"), "x".repeat(11 * 1024 * 1024)).unwrap();
}

fn diff_line_count(files: &[crate::model::DiffFile]) -> usize {
    files
        .iter()
        .flat_map(|f| &f.hunks)
        .map(|h| h.lines.len())
        .sum()
}

fn measure(file_count: usize, lines_per_file: usize) {
    let temp = tempfile::tempdir().expect("temp dir");
    build_repo(temp.path(), file_count, lines_per_file);

    let t = Instant::now();
    let backend = GitBackend::discover_from(
        temp.path(),
        GitBackendPreference::Libgit2,
        DiffWhitespaceMode::Normal,
    )
    .expect("open backend");
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let files = backend.get_working_tree_diff().expect("diff");
    let parse_ms = t.elapsed().as_secs_f64() * 1000.0;
    let lines = diff_line_count(&files);

    let t = Instant::now();
    let theme = crate::theme::Theme::dark();
    let highlighter = theme.syntax_highlighter();
    let highlighter_ms = t.elapsed().as_secs_f64() * 1000.0;

    // What eager highlighting used to cost at load, for comparison.
    let t = Instant::now();
    let mut eager = files.clone();
    highlighter.highlight_files_fully(&mut eager);
    let full_ms = t.elapsed().as_secs_f64() * 1000.0;
    drop(eager);

    let t = Instant::now();
    let vcs_info = backend.info().clone();
    let session = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );
    let mut app = App::build(
        Box::new(backend),
        vcs_info,
        theme,
        None,
        false,
        files,
        session,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
    )
    .expect("build app");
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;

    let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
    let t = Instant::now();
    terminal
        .draw(|frame| crate::ui::render(frame, &mut app))
        .expect("draw frame");
    let first_frame_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    for _ in 0..20 {
        app.cursor_down(1);
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("draw frame");
    }
    let scroll_frame_ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;

    let t = Instant::now();
    app.jump_to_bottom();
    terminal
        .draw(|frame| crate::ui::render(frame, &mut app))
        .expect("draw frame");
    let jump_ms = t.elapsed().as_secs_f64() * 1000.0;

    println!(
        "{file_count:>4} files x {lines_per_file:>5} lines = {lines:>7} diff lines: \
         open {open_ms:>7.1}ms  parse {parse_ms:>7.1}ms  highlighter-init {highlighter_ms:>7.1}ms  \
         highlight-all {full_ms:>8.1}ms  build {build_ms:>7.1}ms  first-frame {first_frame_ms:>7.1}ms  \
         scroll-frame {scroll_frame_ms:>6.2}ms  jump-end {jump_ms:>7.1}ms"
    );
}

#[test]
#[ignore = "timing measurement, run explicitly"]
fn load_perf_scaling() {
    for (files, lines) in [(10, 500), (50, 1000), (200, 1000), (100, 5000)] {
        measure(files, lines);
    }
}
