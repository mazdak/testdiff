# testdiff

A lightweight CLI that suggests which Python test files to run after a code change,
and can reformat pytest JUnit XML reports into GitHub Actions annotations.

It always parses Python files using Ruff's parser, builds a module-level import
graph, walks reverse dependencies from the changed files, and prints impacted
test paths (one per line). Non-Python changes are ignored (exit 0, with a notice unless `--quiet`).


## Install (pre-built binaries)
```bash
curl -sSL https://raw.githubusercontent.com/mazdak/testdiff/master/scripts/install.sh | bash
```

## Usage

```bash
# From repo root
cargo run -p testdiff -- --changed src/foo.py,tests/test_bar.py

# Use git diff / merge-base helpers
cargo run -p testdiff -- --git-diff origin/main --max 50

# Reformat a pytest JUnit XML into GitHub Actions annotations
cargo run -p testdiff -- format junit-report.xml

# Include skipped tests as warnings
cargo run -p testdiff -- format junit-report.xml --include-skipped
```

Options (core):
- `--changed`: comma-separated paths (absolute or relative to the current working directory).
- `--git-diff`, `--git-merge-base`, `--git-staged`, `--git-worktree`: populate the changed file set from Git instead of `--changed`.
- `--root`: optional project root to scan (defaults to the current working directory).
- `--max`: cap the number of suggested tests.
- `--dry-run`: print diagnostics instead of a plain list.
- `--quiet`: suppress warnings.
- `--warn-as-error`: treat any warning as a non-zero exit.
- `--distance-limit`: optional maximum graph distance from changed modules.

Format subcommand (`testdiff format <path>`):
- Input: pytest JUnit XML (e.g., `pytest --junitxml=report.xml`).
- Output: GitHub Actions annotation lines printed to stdout (e.g., `::error file=tests/test_example.py,line=12::message`).
- `--include-skipped`: emit skipped tests as warnings (skips are ignored by default).
- If no failures/errors (and skips are excluded), a short message is printed to stderr.

## Heuristics
- Ranking: shortest import-graph distance first (directly changed tests have distance zero), then filename similarity to actual changed modules, then path for deterministic ties.
- The cap applies after ranking, even when changed tests outnumber the cap. Use `--dry-run` to inspect scores.
- Static imports do not capture every runtime relationship: dynamic imports, implicit fixtures, and registrations may be invisible to the graph.
- Test detection: files named `test_*.py` or `*_test.py`.
- Import-graph mode: relative imports are resolved against the current module path; unresolved imports fall back to matching `<module>.py` or `<module>/__init__.py` under the project root. Unresolved imports are reported as warnings.

## Status

Stateless by design (no persistent cache). Performance is kept modest by skipping common vendor/build directories (e.g., `.git`, `target`, `.venv`, `node_modules`).

## Django migration conflicts

Run from the repository root:

```bash
testdiff migrations --base origin/master --head HEAD \
  --app core=core/migrations --app users=users/migrations \
  --external-app auth --external-app contenttypes \
  --setting AUTH_USER_MODEL=auth.User
```

Fetch the base before invoking the command. `--head` is the commit being checked
(default `HEAD`), not the working tree. `--base` uses `git merge-tree --write-tree`
(Git 2.38+) to check the proposed merge without changing the index, checkout, or
branches. Omit `--base` to check an already merged CI/merge-queue commit. A Git
merge conflict also fails the command. A shallow clone needs enough history for
a merge base; missing history fails rather than checking the branch alone.

Repeat `--app LABEL=PATH` for each installed local migration module; explicit
paths avoid treating migration helpers and test fixtures as installed apps.
Declare third-party dependency apps with `--external-app`; their graphs are not
loaded. `--setting NAME=app.Model` resolves `swappable_dependency(settings.NAME)`.
Only the configured apps are checked, so update this list when installing apps.

The checker parses Python with Ruff, reads migration blobs in one Git batch, and
checks dependencies, competing same-app heads, missing references, cycles,
`run_before`, and literal squash `replaces` metadata. Existing valid merge
migrations pass; migration number reuse alone is not a conflict. It never imports
Python, starts Django, installs dependencies, or connects to a database.

This is an early source-only check, not a replacement for Django's own checks or
migration execution. Squashes are treated as replacements in an unapplied graph;
partially applied database histories still require Django validation. Computed
graph metadata, custom Migration bases, and unsupported class statements fail
explicitly. Keep graph metadata literal. Arbitrary Python side effects and model
changes without migrations are outside this check's scope.

`--conflicts-json PATH` writes an app-to-heads JSON object once the graph has
been validated (`{}` when there are no competing heads). It is not written for
Git, parsing, missing-reference, or cycle errors; callers should use a fresh
output path. This lets CI retain structured conflict evidence.

The command exits nonzero for conflicts, unsupported metadata, invalid input, or
Git failures. Errors identify the affected migration files. Test coverage includes
divergent Git branches, valid merges, deleted dependencies, squash replacements,
cross-app cycles, and preservation of staged/unstaged work.
