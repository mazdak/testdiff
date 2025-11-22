use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_parser::parse_module;

use crate::project::resolve::{ImportSpec, module_name, resolve_import};
use crate::project::utils::{filter_dir, is_python_file};

pub struct ModuleInfo {
    pub module: String,
    pub path: Utf8PathBuf,
    pub imports: Vec<String>,
}

pub struct ProjectIndex {
    pub root: Utf8PathBuf,
    pub modules: HashMap<String, ModuleInfo>,
    pub path_to_module: HashMap<Utf8PathBuf, String>,
    pub warnings: Vec<String>,
}

impl ProjectIndex {
    pub fn build(root: &Utf8Path) -> Result<Self> {
        let mut modules = HashMap::default();
        let mut path_to_module = HashMap::default();
        let mut warnings = Vec::new();

        for entry in WalkBuilder::new(root)
            .hidden(false)
            .ignore(true)
            .git_ignore(true)
            .git_exclude(true)
            .parents(true)
            .filter_entry(|e| filter_dir(e.path()))
            .build()
        {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warnings.push(format!("Skipping entry: {err}"));
                    continue;
                }
            };
            if !is_python_file(entry.path()) {
                continue;
            }

            match Self::parse_file(root, entry.path(), &mut warnings) {
                Ok(Some(info)) => {
                    path_to_module.insert(info.path.clone(), info.module.clone());
                    modules.insert(info.module.clone(), info);
                }
                Ok(None) => {}
                Err(err) => warnings.push(format!("{}: {err}", entry.path().display())),
            }
        }

        Ok(Self {
            root: root.to_owned(),
            modules,
            path_to_module,
            warnings,
        })
    }

    fn parse_file(
        root: &Utf8Path,
        path: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<Option<ModuleInfo>> {
        let utf8_path = match Utf8PathBuf::from_path_buf(path.to_path_buf()) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let source = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let parsed = match parse_module(&source) {
            Ok(parsed) => parsed,
            Err(err) => {
                warnings.push(format!("Failed to parse {}: {err}", path.display()));
                return Ok(None);
            }
        };

        let mut collector = ImportCollector::default();
        for stmt in &parsed.syntax().body {
            collector.visit_stmt(stmt);
        }

        let module = module_name(root, &utf8_path);
        let is_package = utf8_path
            .file_stem()
            .map(|s| s == "__init__")
            .unwrap_or(false);
        let imports = collector
            .imports
            .into_iter()
            .filter_map(|imp| resolve_import(&module, is_package, &imp))
            .collect();

        Ok(Some(ModuleInfo {
            module,
            path: utf8_path,
            imports,
        }))
    }
}

#[derive(Default)]
struct ImportCollector {
    imports: Vec<ImportSpec>,
}

impl<'a> Visitor<'a> for ImportCollector {
    fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
        match stmt {
            ast::Stmt::Import(ast::StmtImport { names, .. }) => {
                for alias in names {
                    self.imports.push(ImportSpec {
                        level: 0,
                        module: Some(alias.name.to_string()),
                        name: None,
                        kind: super::resolve::ImportKind::Import,
                    });
                }
            }
            ast::Stmt::ImportFrom(ast::StmtImportFrom {
                module,
                names,
                level,
                ..
            }) => {
                for alias in names {
                    self.imports.push(ImportSpec {
                        level: *level,
                        module: module.as_ref().map(|m| m.to_string()),
                        name: Some(alias.name.to_string()),
                        kind: super::resolve::ImportKind::ImportFrom,
                    });
                }
            }
            _ => {}
        }

        visitor::walk_stmt(self, stmt);
    }
}
