use std::path::{Path, PathBuf};

use anyhow::Result;
use camino::Utf8PathBuf;
use clap::{Args as ClapArgs, Parser, Subcommand};
use shellexpand;

mod format;
mod git;
mod priority;
mod project;

use format::FormatArgs;
use git::gather_git_changed;
use project::{ProjectIndex, TestResult};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Suggest impacted Python tests for changed files"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Default mode: suggest impacted tests (same as running `testdiff` without a subcommand).
    #[command(flatten)]
    select: SelectArgs,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Format a pytest JUnit XML report as GitHub Actions annotations
    Format(FormatArgs),
}

#[derive(ClapArgs, Debug)]
#[command(next_help_heading = "Selection options")]
pub struct SelectArgs {
    /// Comma-separated list of changed files (relative to CWD or absolute)
    #[arg(long, value_delimiter = ',')]
    changed: Vec<String>,

    /// Diff against this Git ref (e.g., origin/main) to populate changed files
    #[arg(long)]
    git_diff: Option<String>,

    /// Use staged changes (`git diff --cached`) to populate changed files
    #[arg(long)]
    git_staged: bool,

    /// Use merge-base with this ref (implies git-diff if not provided explicitly)
    #[arg(long)]
    git_merge_base: Option<String>,

    /// Use working tree (staged + unstaged) changes against HEAD
    #[arg(long)]
    git_worktree: bool,

    /// Project root to scan (defaults to current directory)
    #[arg(long)]
    root: Option<PathBuf>,

    /// Maximum number of test files to output (most relevant first)
    #[arg(long)]
    max: Option<usize>,

    /// Limit graph distance from changed modules (0 = only tests directly in changed modules). If omitted, no distance cap.
    #[arg(long)]
    distance_limit: Option<usize>,

    /// Dry run: print diagnostics about changed files and selection, do not output plain list
    #[arg(long)]
    dry_run: bool,

    /// Treat any warning as an error (non-zero exit)
    #[arg(long)]
    warn_as_error: bool,

    /// Suppress warnings to stderr
    #[arg(long)]
    quiet: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(Command::Format(args)) = cli.command {
        return format::format_junit(&args);
    }

    let args = cli.select;
    let cwd = std::env::current_dir()?;
    let mut changed_abs = absolutize_changed(&args.changed, &cwd)?;

    if changed_abs.is_empty() {
        changed_abs = gather_git_changed(&args, &cwd)?;
    } else if args.git_staged || args.git_diff.is_some() || args.git_merge_base.is_some() {
        let git_paths = gather_git_changed(&args, &cwd)?;
        changed_abs.extend(git_paths);
    }

    // Limit the selection set to Python sources; config/shell/etc. should not trigger any tests.
    changed_abs = filter_python_files(changed_abs);

    if changed_abs.is_empty() {
        if !args.quiet {
            eprintln!("Info: no changed Python files detected; skipping.");
        }
        return Ok(());
    }

    let root = choose_root(args.root, &changed_abs, &cwd)?;
    let changed_paths = normalize_changed(&changed_abs)?;

    let project = ProjectIndex::build(&root)?;
    let impacted = project.impacted_tests(
        &changed_paths,
        args.max,
        args.distance_limit,
        args.quiet,
        args.warn_as_error,
    )?;

    if args.dry_run {
        print_dry_run(&root, &changed_paths, &impacted);
    } else {
        for res in impacted {
            println!("{}", res.path);
        }
    }

    Ok(())
}

fn absolutize_changed(inputs: &[String], cwd: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for raw in inputs {
        let expanded = shellexpand::tilde(raw).into_owned();
        let candidate = PathBuf::from(expanded);
        let path = if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        };
        // Prefer canonical paths; fall back to absolute on error (e.g., missing file).
        if let Ok(real) = path.canonicalize() {
            paths.push(real);
        } else {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn filter_python_files(inputs: Vec<PathBuf>) -> Vec<PathBuf> {
    inputs
        .into_iter()
        .filter(|p| p.extension().map(|ext| ext == "py").unwrap_or(false))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{choose_root, common_ancestor_dirs, filter_python_files};
    use camino::Utf8PathBuf;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn filters_to_python_only() {
        let files = vec![
            PathBuf::from("foo.py"),
            PathBuf::from("bar.txt"),
            PathBuf::from("scripts/test"),
            PathBuf::from("nested/baz.py"),
        ];

        let filtered = filter_python_files(files);
        let strings: Vec<String> = filtered
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();

        assert_eq!(strings, vec!["foo.py", "nested/baz.py"]);
    }

    #[test]
    fn choose_root_prefers_nearest_pyproject() {
        let tmp = tempdir().unwrap();
        let cwd = tmp.path();
        let workspace = cwd.join("repo");
        let nested = workspace.join("pkg").join("module");
        fs::create_dir_all(&nested).unwrap();
        fs::write(workspace.join("pyproject.toml"), "").unwrap();
        let changed = nested.join("file.py");
        fs::write(&changed, "print('ok')").unwrap();

        let root =
            choose_root(None, &[changed.clone()], &workspace).expect("root resolution failed");
        assert_eq!(root, Utf8PathBuf::from_path_buf(workspace.clone()).unwrap());
    }

    #[test]
    fn common_ancestor_dirs_finds_shared_parent() {
        let a = PathBuf::from("/tmp/a/b/c.py");
        let b = PathBuf::from("/tmp/a/b/d.py");
        let ancestor = common_ancestor_dirs(&[a, b]).unwrap();
        assert_eq!(ancestor, PathBuf::from("/tmp/a/b"));
    }
}

fn choose_root(explicit: Option<PathBuf>, changed: &[PathBuf], cwd: &Path) -> Result<Utf8PathBuf> {
    // 1) explicit --root wins.
    // 2) nearest ancestor of each changed file containing pyproject.toml or .git; pick shortest ascent.
    // 3) common ancestor of parent dirs of changed files.
    // 4) fallback to cwd.

    let pick_dir = |p: &Path| {
        if p.is_dir() {
            p.to_path_buf()
        } else {
            p.parent().unwrap_or(p).to_path_buf()
        }
    };

    let path = if let Some(root) = explicit {
        pick_dir(&root)
    } else {
        let mut candidates: Vec<(usize, PathBuf)> = Vec::new();
        for path in changed {
            let mut depth = 0usize;
            let mut current = pick_dir(path);
            loop {
                if current.join("pyproject.toml").exists() || current.join(".git").exists() {
                    candidates.push((depth, current.clone()));
                    break;
                }
                if let Some(parent) = current.parent() {
                    if parent == current {
                        break;
                    }
                    current = parent.to_path_buf();
                    depth += 1;
                } else {
                    break;
                }
            }
        }

        if let Some((_, best)) = candidates.into_iter().min_by_key(|(d, _)| *d) {
            best
        } else if let Some(common) = common_ancestor_dirs(changed) {
            common
        } else {
            cwd.to_path_buf()
        }
    };

    let path = if path.parent().is_none() {
        cwd.to_path_buf()
    } else {
        path
    };

    Utf8PathBuf::from_path_buf(path)
        .map_err(|_| anyhow::anyhow!("Project root must be valid UTF-8"))
}

fn common_ancestor_dirs(paths: &[PathBuf]) -> Option<PathBuf> {
    let parents: Vec<PathBuf> = paths
        .iter()
        .map(|p| p.parent().unwrap_or(p).to_path_buf())
        .collect();
    if parents.is_empty() {
        return None;
    }
    let mut iter = parents.iter();
    let first = iter.next()?.components().collect::<Vec<_>>();
    let mut prefix_len = first.len();

    for path in iter {
        let comps = path.components().collect::<Vec<_>>();
        prefix_len = prefix_len.min(comps.len());
        for i in 0..prefix_len {
            if first[i] != comps[i] {
                prefix_len = i;
                break;
            }
        }
    }

    if prefix_len == 0 {
        None
    } else {
        Some(first[..prefix_len].iter().collect())
    }
}

fn normalize_changed(inputs: &[PathBuf]) -> Result<Vec<Utf8PathBuf>> {
    let mut out = Vec::new();
    for path in inputs {
        if let Ok(p) = Utf8PathBuf::from_path_buf(path.clone()) {
            out.push(p);
        } else {
            return Err(anyhow::anyhow!(
                "Changed path must be valid UTF-8: {}",
                path.display()
            ));
        }
    }
    Ok(out)
}

fn print_dry_run(root: &Utf8PathBuf, changed: &[Utf8PathBuf], impacted: &[TestResult]) {
    eprintln!("Root: {}", root);
    eprintln!("Changed files ({}):", changed.len());
    for p in changed {
        eprintln!("  - {}", p);
    }
    eprintln!("\nSelected tests ({}):", impacted.len());
    for res in impacted {
        eprintln!(
            "  - {} (distance={}, filename_match={})",
            res.path, res.distance, res.priority.filename_match
        );
    }
}
