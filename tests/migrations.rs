use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(root: &Path, name: &str, deps: &str) {
    let dir = root.join("app/migrations");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{name}.py")), format!("from django.db import migrations\nclass Migration(migrations.Migration):\n    dependencies = {deps}\n")).unwrap();
}

fn commit(root: &Path) -> String {
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "fixture"]);
    git(root, &["rev-parse", "HEAD"])
}

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "master"]);
    write(dir.path(), "0001", "[]");
    commit(dir.path());
    dir
}

fn check(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_testdiff"))
        .current_dir(root)
        .args(["migrations", "--app", "app=app/migrations"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn detects_conflict_only_in_combined_tree_without_touching_checkout() {
    let repo = repo();
    let root = repo.path();
    git(root, &["checkout", "-qb", "feature"]);
    write(root, "0002_feature", "[('app', '0001')]");
    let feature = commit(root);
    git(root, &["checkout", "-q", "master"]);
    write(root, "0002_master", "[('app', '0001')]");
    commit(root);
    assert!(check(root, &["--head", "feature"]).status.success());
    assert!(check(root, &[]).status.success());
    // Dirty and staged files must survive the check and must not affect its result.
    fs::write(root.join("scratch"), "local work").unwrap();
    git(root, &["add", "scratch"]);
    let before = git(root, &["status", "--porcelain"]);
    let report = repo.path().join("conflicts.json");
    let result = check(
        root,
        &[
            "--base",
            "master",
            "--head",
            &feature,
            "--conflicts-json",
            report.to_str().unwrap(),
        ],
    );
    let conflicts: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(
        conflicts,
        serde_json::json!({"app": ["0002_feature", "0002_master"]})
    );
    fs::remove_file(report).unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Migration conflicts"));
    assert_eq!(before, git(root, &["status", "--porcelain"]));
    assert_eq!("master", git(root, &["branch", "--show-current"]));
    assert!(!root.join(".git/MERGE_HEAD").exists());
    write(
        root,
        "0003_merge",
        "[('app', '0002_feature'), ('app', '0002_master')]",
    );
    commit(root);
    assert!(
        check(root, &["--base", "master", "--head", &feature])
            .status
            .success()
    );
}

#[test]
fn reports_git_content_conflict() {
    let repo = repo();
    let root = repo.path();
    git(root, &["checkout", "-qb", "feature"]);
    write(root, "0002", "[('app', '0001')]");
    commit(root);
    git(root, &["checkout", "-q", "master"]);
    write(root, "0002", "[]");
    commit(root);
    let result = check(root, &["--base", "master", "--head", "feature"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("CONFLICT"));
}

#[test]
fn deleted_parent_and_invalid_python_fail() {
    let repo = repo();
    let root = repo.path();
    write(root, "0002", "[('app', '0001')]");
    fs::remove_file(root.join("app/migrations/0001.py")).unwrap();
    commit(root);
    assert!(String::from_utf8_lossy(&check(root, &[]).stderr).contains("missing dependency"));
    fs::write(root.join("app/migrations/0002.py"), "class Migration(:").unwrap();
    commit(root);
    assert!(String::from_utf8_lossy(&check(root, &[]).stderr).contains("invalid Python"));
}

#[test]
fn dynamic_metadata_fails_and_python_is_never_executed() {
    let repo = repo();
    let root = repo.path();
    let file = root.join("app/migrations/0001.py");
    fs::write(&file, "raise RuntimeError('must not execute')\nclass Migration(migrations.Migration):\n    dependencies = []\n").unwrap();
    commit(root);
    assert!(check(root, &[]).status.success());
    fs::write(&file, "class Migration(migrations.Migration):\n    dependencies = []\nMigration.dependencies.append(('app', 'missing'))\n").unwrap();
    commit(root);
    assert!(String::from_utf8_lossy(&check(root, &[]).stderr).contains("outside the class"));
}

#[test]
fn batch_reader_handles_more_than_pipe_capacity() {
    let repo = repo();
    let root = repo.path();
    for i in 2..1100 {
        write(
            root,
            &format!("{i:04}"),
            &format!("[('app', '{:04}')]", i - 1),
        );
    }
    commit(root);
    let result = check(root, &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
