use std::path::Path;

pub(crate) fn filter_dir(path: &Path) -> bool {
    const SKIP: &[&str] = &[
        ".git",
        "target",
        ".tox",
        ".venv",
        "venv",
        "__pycache__",
        "node_modules",
    ]; // keep scan lean
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if SKIP.contains(&name) {
            return false;
        }
    }
    true
}

pub(crate) fn is_python_file(path: &Path) -> bool {
    path.extension().map(|ext| ext == "py").unwrap_or(false)
}

pub(crate) fn is_test_file(path: &Path) -> bool {
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    filename.starts_with("test_") || filename.ends_with("_test.py")
}
