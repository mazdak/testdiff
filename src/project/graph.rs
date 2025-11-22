use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use camino::Utf8PathBuf;

use crate::priority::{Priority, priority};
use crate::project::resolve::module_name;
use crate::project::utils::is_test_file;

use super::index::ProjectIndex;

pub struct TestResult {
    pub path: String,
    pub priority: Priority,
    pub distance: usize,
}

impl ProjectIndex {
    pub fn impacted_tests(
        &self,
        changed: &[Utf8PathBuf],
        max: Option<usize>,
        distance_limit: Option<usize>,
        quiet: bool,
        warn_as_error: bool,
    ) -> Result<Vec<TestResult>> {
        let mut warnings = self.warnings.clone();

        let top_levels: HashSet<&str> = self
            .modules
            .keys()
            .filter_map(|name| name.split('.').next())
            .collect();

        let mut reverse: HashMap<String, HashSet<String>> = HashMap::default();
        for info in self.modules.values() {
            for import in &info.imports {
                let target = self
                    .resolve_known_module(import)
                    .or_else(|| self.heuristic_map(import))
                    .or_else(|| self.trim_to_known_module(import))
                    .unwrap_or_else(|| {
                        if top_levels.contains(import.split('.').next().unwrap_or("")) {
                            warnings.push(format!(
                                "Unresolved import `{}` in module `{}`",
                                import, info.module
                            ));
                        }
                        String::new()
                    });
                if target.is_empty() {
                    continue;
                }
                reverse
                    .entry(target)
                    .or_default()
                    .insert(info.module.clone());
            }
        }

        if !quiet {
            for w in &warnings {
                eprintln!("Warning: {w}");
            }
        }

        let mut impacted_modules: HashSet<String> = HashSet::new();
        let mut distances: HashMap<String, usize> = HashMap::default();
        let mut queue: VecDeque<String> = VecDeque::new();

        for path in changed {
            if let Some(module) = self.path_to_module.get(path) {
                if impacted_modules.insert(module.clone()) {
                    distances.insert(module.clone(), 0);
                    queue.push_back(module.clone());
                }
                continue;
            }

            // Handle Python files that no longer exist or failed to parse (e.g., `git rm`).
            // We approximate a module name from the path and resolve it using the same
            // heuristics as for imports, then seed the graph from that module.
            if path.extension().map(|ext| ext == "py").unwrap_or(false) {
                let guessed_module = module_name(&self.root, path.as_ref());
                let target = self
                    .resolve_known_module(&guessed_module)
                    .or_else(|| self.heuristic_map(&guessed_module))
                    .or_else(|| self.trim_to_known_module(&guessed_module))
                    .unwrap_or(guessed_module.clone());

                if impacted_modules.insert(target.clone()) {
                    distances.insert(target.clone(), 0);
                    queue.push_back(target);
                }

                if !quiet {
                    eprintln!(
                        "Warning: changed file not indexed (using module `{}`): {}",
                        guessed_module, path
                    );
                }
            }
        }

        while let Some(module) = queue.pop_front() {
            let current_dist = distances.get(&module).copied().unwrap_or(0);
            if let Some(limit) = distance_limit {
                if current_dist >= limit {
                    continue; // prune beyond limit
                }
            }

            if let Some(children) = reverse.get(&module) {
                for dep in children {
                    if impacted_modules.insert(dep.clone()) {
                        let dist = current_dist + 1;
                        distances.insert(dep.clone(), dist);
                        queue.push_back(dep.clone());
                    }
                }
            }
        }

        let changed_leaves: HashSet<String> = distances
            .iter()
            .filter(|(m, _)| impacted_modules.contains(*m))
            .filter_map(|(m, _)| m.split('.').last().map(str::to_string))
            .collect();

        let mut tests: Vec<TestResult> = Vec::new();

        for module in impacted_modules {
            if let Some(info) = self.modules.get(&module) {
                if is_test_file(info.path.as_std_path()) {
                    if let Ok(rel) = info.path.strip_prefix(&self.root) {
                        let p = priority(
                            rel.as_str(),
                            distances.get(&module).copied().unwrap_or(usize::MAX),
                            &changed_leaves,
                        );
                        tests.push(TestResult {
                            path: rel.to_string(),
                            priority: p,
                            distance: distances.get(&module).copied().unwrap_or(usize::MAX),
                        });
                    } else {
                        let p = priority(
                            info.path.as_str(),
                            distances.get(&module).copied().unwrap_or(usize::MAX),
                            &changed_leaves,
                        );
                        tests.push(TestResult {
                            path: info.path.to_string(),
                            priority: p,
                            distance: distances.get(&module).copied().unwrap_or(usize::MAX),
                        });
                    }
                }
            }
        }

        tests.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.path.cmp(&b.path))
        });
        if let Some(limit) = max {
            tests.truncate(limit);
        }
        if warn_as_error && !warnings.is_empty() {
            anyhow::bail!(
                "Warnings treated as errors ({} warnings). First: {}",
                warnings.len(),
                warnings[0]
            );
        }
        Ok(tests)
    }

    fn heuristic_map(&self, import: &str) -> Option<String> {
        let candidate = import.replace('.', "/");
        let file = self.root.join(format!("{candidate}.py"));
        if file.exists() {
            if let Some(module) = self.path_to_module.get(&file) {
                return Some(module.clone());
            }
        }
        let init = self.root.join(format!("{candidate}/__init__.py"));
        if init.exists() {
            if let Some(module) = self.path_to_module.get(&init) {
                return Some(module.clone());
            }
        }
        None
    }

    fn resolve_known_module(&self, import: &str) -> Option<String> {
        self.modules
            .contains_key(import)
            .then(|| import.to_string())
    }

    fn trim_to_known_module(&self, import: &str) -> Option<String> {
        let mut parts: Vec<&str> = import.split('.').collect();
        while parts.len() > 1 {
            parts.pop();
            let candidate = parts.join(".");
            if self.modules.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}
