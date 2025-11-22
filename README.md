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
- Test detection: files named `test_*.py` or `*_test.py`.
- Import-graph mode: relative imports are resolved against the current module path; unresolved imports fall back to matching `<module>.py` or `<module>/__init__.py` under the project root. Unresolved imports are reported as warnings.

## Status

Stateless by design (no persistent cache). Performance is kept modest by skipping common vendor/build directories (e.g., `.git`, `target`, `.venv`, `node_modules`).
