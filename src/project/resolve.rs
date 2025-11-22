use camino::Utf8Path;

#[derive(Clone, Copy)]
pub(super) enum ImportKind {
    Import,
    ImportFrom,
}

#[derive(Clone)]
pub(super) struct ImportSpec {
    pub level: u32,
    pub module: Option<String>,
    pub name: Option<String>,
    pub kind: ImportKind,
}

pub(super) fn module_name(root: &Utf8Path, path: &Utf8Path) -> String {
    let mut package_parts = Vec::new();
    let mut current = path.parent();

    while let Some(dir) = current {
        if dir.join("__init__.py").exists() {
            if let Some(name) = dir.file_name() {
                package_parts.push(name.to_string());
            }
            current = dir.parent();
        } else {
            break;
        }
    }

    package_parts.reverse();

    let stem = path
        .file_stem()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "".to_string());

    if stem == "__init__" {
        return package_parts.join(".");
    }

    if !package_parts.is_empty() {
        let mut parts = package_parts;
        parts.push(stem);
        return parts.join(".");
    }

    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut components: Vec<String> = rel.components().map(|c| c.as_str().to_string()).collect();
    if let Some(last) = components.last_mut() {
        if let Some(stripped) = last.strip_suffix(".py") {
            *last = stripped.to_string();
        }
    }
    components.join(".")
}

pub(super) fn resolve_import(
    current_module: &str,
    is_package: bool,
    spec: &ImportSpec,
) -> Option<String> {
    // Relative imports are encoded as levels (number of leading dots).
    let relative = spec.level > 0;

    let base = if relative {
        let mut parts: Vec<&str> = current_module.split('.').collect();
        // If current module is a package (__init__), don't pop the last part when handling relative imports.
        if !(is_package && spec.level == 1) {
            let pops = spec.level.min(parts.len() as u32) as usize;
            for _ in 0..pops {
                parts.pop();
            }
        }
        parts
    } else {
        Vec::new()
    };

    let mut target_parts = base;

    if let Some(module) = &spec.module {
        if !module.is_empty() {
            target_parts.extend(module.split('.'));
        }
    }

    // `from x import y` should resolve to `x.y` (unless y is "*").
    if let ImportKind::ImportFrom = spec.kind {
        if let Some(name) = &spec.name {
            if name != "*" {
                target_parts.push(name);
            }
        }
    }

    if target_parts.is_empty() {
        None
    } else {
        Some(target_parts.join("."))
    }
}
