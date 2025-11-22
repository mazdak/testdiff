use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::SelectArgs;

pub fn gather_git_changed(args: &SelectArgs, cwd: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if args.git_staged {
        paths.extend(run_git_name_only(
            cwd,
            &["diff", "--name-only", "--cached"],
        )?)
    }

    if args.git_worktree {
        // staged + unstaged vs HEAD
        paths.extend(run_git_name_only(cwd, &["diff", "--name-only", "HEAD"])?)
    }

    let mut diff_ref = args.git_diff.clone();
    let merge_base = args.git_merge_base.as_deref();
    if diff_ref.is_none() && merge_base.is_some() {
        diff_ref = Some(merge_base.unwrap().to_string());
    }

    if let Some(base) = diff_ref {
        let base = if let Some(_mb) = merge_base {
            let mb_sha = run_git_single(cwd, &["merge-base", &base, "HEAD"])?;
            mb_sha.trim().to_string()
        } else {
            base
        };
        paths.extend(run_git_name_only(
            cwd,
            &["diff", "--name-only", &format!("{base}..HEAD")],
        )?)
    }

    let mut unique = BTreeSet::new();
    for p in paths {
        let path = if p.is_absolute() { p } else { cwd.join(p) };
        unique.insert(path);
    }
    Ok(unique.into_iter().collect())
}

fn run_git_name_only(cwd: &Path, args: &[&str]) -> Result<Vec<PathBuf>> {
    let out = run_git_single(cwd, args)?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(PathBuf::from)
        .collect())
}

fn run_git_single(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run git {:?}", args))?;

    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed with status {}: {}",
            args,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
