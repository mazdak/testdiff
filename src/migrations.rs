//! A source-only check of the migration graph of a proposed Git merge.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Expr, Stmt};
use ruff_python_parser::parse_module;

type Key = (String, String);

#[derive(Args, Debug)]
pub struct MigrationArgs {
    /// Merge this commit with --head without changing the checkout. Omit for an already merged tree.
    #[arg(long)]
    base: Option<String>,
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// Installed local app label and migration directory, e.g. core=core/migrations. Repeat per app.
    #[arg(long = "app", required = true)]
    apps: Vec<String>,
    /// Dependency app provided outside this repository. Its graph is not checked.
    #[arg(long = "external-app")]
    external_apps: Vec<String>,
    /// Setting used by swappable_dependency, e.g. AUTH_USER_MODEL=auth.User.
    #[arg(long = "setting")]
    settings: Vec<String>,
    /// Write competing app heads as a JSON object after graph validation.
    #[arg(long)]
    conflicts_json: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
struct Migration {
    path: String,
    dependencies: Vec<Key>,
    run_before: Vec<Key>,
    replaces: Vec<Key>,
}

fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).output()?;
    ensure!(
        output.status.success(),
        "git {} failed: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn pairs(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for value in values {
        let (key, value) = value.split_once('=').context("expected NAME=VALUE")?;
        ensure!(
            !key.is_empty() && !value.is_empty(),
            "expected nonempty NAME=VALUE"
        );
        ensure!(
            result.insert(key.to_owned(), value.to_owned()).is_none(),
            "duplicate key {key}"
        );
    }
    Ok(result)
}

pub fn check(args: &MigrationArgs) -> Result<()> {
    let apps = pairs(&args.apps)?;
    let settings = pairs(&args.settings)?;
    let external: BTreeSet<_> = args.external_apps.iter().cloned().collect();
    for (label, path) in &apps {
        ensure!(
            !external.contains(label),
            "app {label} is both local and external"
        );
        ensure!(
            !path.starts_with('/') && !path.ends_with('/') && !path.split('/').any(|p| p == ".."),
            "migration directories must be repository-relative: {path}"
        );
    }
    ensure!(
        apps.values().collect::<BTreeSet<_>>().len() == apps.len(),
        "duplicate migration directory"
    );
    let head = git(&[
        "rev-parse",
        "--verify",
        "--end-of-options",
        &format!("{}^{{commit}}", args.head),
    ])?;
    let tree = if let Some(base) = &args.base {
        let base = git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ])?;
        // merge-tree writes Git objects only: no checkout, index, or branch mutation.
        git(&["merge-tree", "--write-tree", base.trim(), head.trim()])?
            .lines()
            .next()
            .context("merge-tree returned no tree")?
            .to_owned()
    } else {
        head.trim().to_owned()
    };
    let entries = git(&["ls-tree", "-rz", &tree])?;
    let mut files = Vec::new();
    let mut present_dirs = BTreeSet::new();
    for entry in entries.split('\0').filter(|s| !s.is_empty()) {
        let (metadata, path) = entry.split_once('\t').context("invalid ls-tree entry")?;
        let Some((directory, filename)) = path.rsplit_once('/') else {
            continue;
        };
        let Some((app, _)) = apps.iter().find(|(_, dir)| dir.as_str() == directory) else {
            continue;
        };
        present_dirs.insert(directory);
        if !filename.ends_with(".py") || filename.starts_with(['_', '~']) {
            continue;
        }
        let fields: Vec<_> = metadata.split_whitespace().collect();
        ensure!(
            fields[0] == "100644" || fields[0] == "100755",
            "unsupported migration file mode: {path}"
        );
        files.push((
            (
                app.clone(),
                filename.strip_suffix(".py").unwrap().to_owned(),
            ),
            path.to_owned(),
            fields[2].to_owned(),
        ));
    }
    for path in apps.values() {
        ensure!(
            present_dirs.contains(path.as_str()),
            "configured migration directory is missing: {path}"
        );
    }
    ensure!(
        !files.is_empty(),
        "no migration files found in configured apps"
    );
    // Feed and drain concurrently: large histories must not deadlock on pipe capacity.
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let requests = files
        .iter()
        .map(|(_, _, oid)| format!("{oid}\n"))
        .collect::<String>();
    let writer = std::thread::spawn(move || stdin.write_all(requests.as_bytes()));
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("Git input writer panicked"))??;
    ensure!(
        output.status.success(),
        "git cat-file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut bytes = output.stdout.as_slice();
    let mut migrations = BTreeMap::new();
    for (key, path, _) in files {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .context("missing blob header")?;
        let header = std::str::from_utf8(&bytes[..end])?;
        let length: usize = header
            .split_whitespace()
            .nth(2)
            .context("missing blob length")?
            .parse()?;
        bytes = &bytes[end + 1..];
        ensure!(bytes.len() > length, "truncated Git blob");
        let source = std::str::from_utf8(&bytes[..length])?;
        let migration = parse(source, &path, &settings).with_context(|| path.clone())?;
        migrations.insert(key, migration);
        bytes = &bytes[length + 1..];
    }
    let count = migrations.len();
    validate(migrations, &external, args.conflicts_json.as_deref())?;
    println!("Migration graph: {count} files checked; no conflicts (source-only check).");
    Ok(())
}

fn string(expr: &Expr) -> Result<String> {
    match expr {
        Expr::StringLiteral(value) => Ok(value.value.to_string()),
        _ => bail!("expected a literal string"),
    }
}

fn references(expr: &Expr, settings: &BTreeMap<String, String>) -> Result<Vec<Key>> {
    let elements = match expr {
        Expr::List(value) => &value.elts,
        Expr::Tuple(value) => &value.elts,
        _ => bail!("computed migration dependencies are unsupported; use literal lists/tuples"),
    };
    elements.iter().map(|item| {
        if let Expr::Call(call) = item {
            if let Expr::Attribute(function) = call.func.as_ref()
                && function.attr.as_str() == "swappable_dependency"
                && matches!(function.value.as_ref(), Expr::Name(name) if name.id.as_str() == "migrations")
                && call.arguments.args.len() == 1
                && call.arguments.keywords.is_empty()
                && let Expr::Attribute(setting) = &call.arguments.args[0]
                && matches!(setting.value.as_ref(), Expr::Name(name) if name.id.as_str() == "settings")
            {
                let value = settings.get(setting.attr.as_str()).with_context(|| format!("supply --setting {}=app.Model", setting.attr))?;
                let (app, _) = value.split_once('.').context("swappable setting must be app.Model")?;
                return Ok((app.to_owned(), "__first__".to_owned()));
            }
            bail!("unsupported computed migration dependency");
        }
        let values = match item {
            Expr::Tuple(value) => &value.elts,
            Expr::List(value) => &value.elts,
            _ => bail!("expected (app, migration) dependency"),
        };
        ensure!(values.len() == 2, "expected (app, migration) dependency");
        Ok((string(&values[0])?, string(&values[1])?))
    }).collect()
}

// A reference to Migration outside its declaration may mutate or replace graph metadata.
#[derive(Default)]
struct MigrationReference(bool);
impl<'a> Visitor<'a> for MigrationReference {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if matches!(expr, Expr::Name(name) if name.id.as_str() == "Migration") {
            self.0 = true;
        }
        visitor::walk_expr(self, expr);
    }
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if matches!(stmt, Stmt::ClassDef(class) if class.name.as_str() == "Migration") {
            self.0 = true;
        }
        visitor::walk_stmt(self, stmt);
    }
}

fn parse(source: &str, path: &str, settings: &BTreeMap<String, String>) -> Result<Migration> {
    let parsed = parse_module(source).map_err(|e| anyhow::anyhow!("invalid Python: {e}"))?;
    let classes: Vec<_> = parsed
        .suite()
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::ClassDef(class) if class.name.as_str() == "Migration" => Some(class),
            _ => None,
        })
        .collect();
    ensure!(classes.len() == 1, "expected one Migration class");
    let class = classes[0];
    for stmt in parsed.suite() {
        if matches!(stmt, Stmt::ClassDef(candidate) if std::ptr::eq(candidate, class)) {
            continue;
        }
        let mut reference = MigrationReference::default();
        reference.visit_stmt(stmt);
        ensure!(
            !reference.0,
            "Migration references outside the class are unsupported"
        );
    }
    let bases = class
        .arguments
        .as_ref()
        .context("expected migrations.Migration base class")?;
    ensure!(
        bases.args.len() == 1
            && bases.keywords.is_empty()
            && matches!(&bases.args[0], Expr::Attribute(base) if base.attr.as_str() == "Migration"
            && matches!(base.value.as_ref(), Expr::Name(name) if name.id.as_str() == "migrations")),
        "custom Migration base classes are unsupported"
    );
    ensure!(
        class.decorator_list.is_empty(),
        "decorated Migration classes are unsupported"
    );
    let mut migration = Migration {
        path: path.to_owned(),
        ..Migration::default()
    };
    let mut fields = BTreeSet::new();
    for stmt in &class.body {
        match stmt {
            Stmt::Assign(assignment) => {
                for target in &assignment.targets {
                    let Expr::Name(name) = target else {
                        bail!("computed Migration assignment is unsupported")
                    };
                    let name = name.id.as_str();
                    let field = match name {
                        "dependencies" => &mut migration.dependencies,
                        "run_before" => &mut migration.run_before,
                        "replaces" => &mut migration.replaces,
                        _ => continue,
                    };
                    ensure!(
                        fields.insert(name),
                        "repeated {name} assignment is unsupported"
                    );
                    *field = references(&assignment.value, settings)?;
                }
            }
            Stmt::Pass(_) => (),
            Stmt::Expr(expr) if matches!(expr.value.as_ref(), Expr::StringLiteral(_)) => (),
            _ => bail!("dynamic Migration class body is unsupported; use literal graph metadata"),
        }
    }
    ensure!(
        fields.contains("dependencies"),
        "Migration must declare literal dependencies"
    );
    Ok(migration)
}

fn canonical(key: &Key, replacements: &BTreeMap<Key, Key>) -> Result<Key> {
    let mut key = key.clone();
    let mut seen = BTreeSet::new();
    while let Some(next) = replacements.get(&key) {
        ensure!(
            seen.insert(key.clone()),
            "cyclic squash replacements at {}.{}",
            key.0,
            key.1
        );
        key = next.clone();
    }
    Ok(key)
}

fn validate(
    mut migrations: BTreeMap<Key, Migration>,
    external: &BTreeSet<String>,
    report: Option<&Path>,
) -> Result<()> {
    let mut replacements = BTreeMap::new();
    for (key, migration) in &migrations {
        for replaced in &migration.replaces {
            ensure!(
                key.0 == replaced.0,
                "cross-app squash replacement in {}",
                migration.path
            );
            ensure!(
                replacements.insert(replaced.clone(), key.clone()).is_none(),
                "overlapping squash replacements for {}.{}",
                replaced.0,
                replaced.1
            );
        }
    }
    for key in replacements.keys() {
        canonical(key, &replacements)?;
    }
    migrations.retain(|key, _| !replacements.contains_key(key));
    let mut parents: BTreeMap<Key, BTreeSet<Key>> = migrations
        .keys()
        .map(|k| (k.clone(), BTreeSet::new()))
        .collect();
    let mut special = Vec::new();
    for (key, migration) in &migrations {
        for (dependency, before) in migration
            .dependencies
            .iter()
            .map(|d| (d, false))
            .chain(migration.run_before.iter().map(|d| (d, true)))
        {
            if external.contains(&dependency.0) {
                continue;
            }
            if dependency.1 == "__first__" || dependency.1 == "__latest__" {
                if dependency.0 != key.0 {
                    special.push((key.clone(), dependency.clone(), before));
                }
                continue;
            }
            let dependency = canonical(dependency, &replacements)?;
            ensure!(
                migrations.contains_key(&dependency),
                "{}: missing dependency {}.{} (declare --external-app only for installed third-party apps)",
                migration.path,
                dependency.0,
                dependency.1
            );
            let (child, parent) = if before {
                (&dependency, key)
            } else {
                (key, &dependency)
            };
            parents.get_mut(child).unwrap().insert(parent.clone());
        }
    }
    // Resolve Django's symbolic first/latest references after ordinary edges.
    for (key, dependency, before) in special {
        let candidates: Vec<_> = parents
            .keys()
            .filter(|candidate| candidate.0 == dependency.0)
            .filter(|candidate| {
                if dependency.1 == "__first__" {
                    !parents[*candidate].iter().any(|p| p.0 == candidate.0)
                } else {
                    !parents
                        .iter()
                        .any(|(child, deps)| child.0 == candidate.0 && deps.contains(*candidate))
                }
            })
            .cloned()
            .collect();
        ensure!(
            candidates.len() == 1,
            "cannot resolve {}.{} to a single migration",
            dependency.0,
            dependency.1
        );
        let (child, parent) = if before {
            (candidates[0].clone(), key)
        } else {
            (key, candidates[0].clone())
        };
        parents.get_mut(&child).unwrap().insert(parent);
    }
    // Topological removal detects cycles, including cross-app cycles.
    let mut remaining = parents.clone();
    while !remaining.is_empty() {
        let ready: BTreeSet<_> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        ensure!(
            !ready.is_empty(),
            "migration dependency cycle involving {:?}",
            remaining.keys().collect::<Vec<_>>()
        );
        remaining.retain(|k, _| !ready.contains(k));
        for deps in remaining.values_mut() {
            deps.retain(|k| !ready.contains(k));
        }
    }
    let mut leaves: BTreeMap<&str, Vec<&Key>> = BTreeMap::new();
    for key in parents.keys() {
        if !parents
            .iter()
            .any(|(child, deps)| child.0 == key.0 && deps.contains(key))
        {
            leaves.entry(&key.0).or_default().push(key);
        }
    }
    if let Some(path) = report {
        let conflicts: BTreeMap<_, _> = leaves
            .iter()
            .filter(|(_, heads)| heads.len() > 1)
            .map(|(app, heads)| (*app, heads.iter().map(|key| &key.1).collect::<Vec<_>>()))
            .collect();
        std::fs::write(path, serde_json::to_vec(&conflicts)?)?;
    }
    let conflicts: Vec<_> = leaves
        .iter()
        .filter(|(_, heads)| heads.len() > 1)
        .map(|(app, heads)| {
            format!(
                "{app}: {}",
                heads
                    .iter()
                    .map(|key| migrations[*key].path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    ensure!(
        conflicts.is_empty(),
        "Migration conflicts:\n{}\nResolve migration ordering explicitly; generate a merge migration when appropriate.",
        conflicts.join("\n")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(migrations: BTreeMap<Key, Migration>, external: &BTreeSet<String>) -> Result<()> {
        super::validate(migrations, external, None)
    }

    fn graph(entries: &[(&str, &str, &str)]) -> BTreeMap<Key, Migration> {
        entries
            .iter()
            .map(|(app, name, body)| {
                let source = format!("class Migration(migrations.Migration):\n    {body}\n");
                (
                    (app.to_string(), name.to_string()),
                    parse(
                        &source,
                        &format!("{app}/migrations/{name}.py"),
                        &BTreeMap::new(),
                    )
                    .unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn competing_heads_and_valid_merge() {
        let mut g = graph(&[
            ("app", "1", "dependencies = []"),
            ("app", "2a", "dependencies = [('app', '1')]"),
            ("app", "2b", "dependencies = [('app', '1')]"),
        ]);
        assert!(
            validate(g.clone(), &BTreeSet::new())
                .unwrap_err()
                .to_string()
                .contains("Migration conflicts")
        );
        g.extend(graph(&[(
            "app",
            "3",
            "dependencies = [('app', '2a'), ('app', '2b')]",
        )]));
        validate(g, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn same_number_is_valid_when_ordered() {
        validate(
            graph(&[
                ("app", "1_a", "dependencies = []"),
                ("app", "1_b", "dependencies = [('app', '1_a')]"),
            ]),
            &BTreeSet::new(),
        )
        .unwrap();
    }

    #[test]
    fn missing_dependencies_and_cross_app_cycles() {
        assert!(
            validate(
                graph(&[("app", "2", "dependencies = [('app', 'missing')]")]),
                &BTreeSet::new()
            )
            .unwrap_err()
            .to_string()
            .contains("missing dependency")
        );
        assert!(
            validate(
                graph(&[
                    ("a", "1", "dependencies = [('b', '1')]"),
                    ("b", "1", "dependencies = [('a', '1')]")
                ]),
                &BTreeSet::new()
            )
            .unwrap_err()
            .to_string()
            .contains("cycle")
        );
    }

    #[test]
    fn cross_app_child_does_not_hide_a_conflicting_head() {
        assert!(
            validate(
                graph(&[
                    ("a", "1", "dependencies = []"),
                    ("a", "2", "dependencies = []"),
                    ("b", "1", "dependencies = [('a', '1')]")
                ]),
                &BTreeSet::new()
            )
            .unwrap_err()
            .to_string()
            .contains("Migration conflicts")
        );
    }

    #[test]
    fn run_before_orders_migrations() {
        validate(
            graph(&[
                ("a", "1", "dependencies = []\n    run_before = [('a', '2')]"),
                ("a", "2", "dependencies = []"),
            ]),
            &BTreeSet::new(),
        )
        .unwrap();
    }

    #[test]
    fn squashes_redirect_dependencies_with_or_without_originals() {
        for originals in [false, true] {
            let mut g = graph(&[
                (
                    "a",
                    "squash",
                    "dependencies = []\n    replaces = [('a', '1'), ('a', '2')]",
                ),
                ("a", "3", "dependencies = [('a', '2')]"),
            ]);
            if originals {
                g.extend(graph(&[
                    ("a", "1", "dependencies = []"),
                    ("a", "2", "dependencies = [('a', '1')]"),
                ]));
            }
            validate(g, &BTreeSet::new()).unwrap();
        }
    }

    #[test]
    fn cyclic_replacements_fail() {
        assert!(
            validate(
                graph(&[
                    ("a", "1", "dependencies = []\n    replaces = [('a', '2')]"),
                    ("a", "2", "dependencies = []\n    replaces = [('a', '1')]")
                ]),
                &BTreeSet::new()
            )
            .unwrap_err()
            .to_string()
            .contains("cyclic squash")
        );
    }

    #[test]
    fn external_apps_are_explicit_and_first_latest_resolve() {
        let g = graph(&[("a", "1", "dependencies = [('auth', '1')]")]);
        assert!(validate(g.clone(), &BTreeSet::new()).is_err());
        validate(g, &BTreeSet::from(["auth".to_owned()])).unwrap();
        validate(
            graph(&[
                ("a", "1", "dependencies = []"),
                ("a", "2", "dependencies = [('a', '1')]"),
                (
                    "b",
                    "1",
                    "dependencies = [('a', '__first__'), ('a', '__latest__')]",
                ),
            ]),
            &BTreeSet::new(),
        )
        .unwrap();
    }

    #[test]
    fn unsupported_metadata_does_not_pass_silently() {
        for body in [
            "dependencies = calculate()",
            "dependencies = []\n    dependencies += [('a', '1')]",
            "dependencies = []\n    if True:\n        dependencies = [('a', '1')]",
        ] {
            assert!(
                parse(
                    &format!("class Migration(migrations.Migration):\n    {body}\n"),
                    "x.py",
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn swappable_setting_is_required() {
        let source = "class Migration(migrations.Migration):\n    dependencies = [migrations.swappable_dependency(settings.AUTH_USER_MODEL)]\n";
        assert!(parse(source, "x.py", &BTreeMap::new()).is_err());
        let settings = BTreeMap::from([("AUTH_USER_MODEL".to_owned(), "auth.User".to_owned())]);
        assert_eq!(
            parse(source, "x.py", &settings).unwrap().dependencies,
            vec![("auth".to_owned(), "__first__".to_owned())]
        );
    }
}
