use super::index::ProjectIndex;
use super::utils::is_test_file;
use camino::{Utf8Path, Utf8PathBuf};
use std::fs;
use tempfile::tempdir;

fn write_file(root: &Utf8Path, relative: &str, contents: &str) -> Utf8PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent.as_std_path()).unwrap();
    }
    fs::write(path.as_std_path(), contents).unwrap();
    path
}

#[test]
fn import_graph_selects_reverse_dep_tests() {
    let tmp = tempdir().unwrap();
    let root_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let root: &Utf8Path = root_path.as_ref();

    write_file(root, "pkg/__init__.py", "");
    let changed_path = write_file(root, "pkg/foo.py", "def f():\n    return 1\n");
    write_file(root, "tests/test_foo.py", "from pkg import foo\n\n");

    let index = ProjectIndex::build(root).unwrap();
    let changed = vec![changed_path];
    let impacted = index
        .impacted_tests(&changed, None, None, true, false)
        .unwrap();

    let names: Vec<_> = impacted.iter().map(|t| t.path.as_str()).collect();
    assert!(
        names.contains(&"tests/test_foo.py"),
        "expected tests/test_foo.py in impacted tests, got {:?}",
        names
    );
}

#[test]
fn distance_limit_prunes_beyond_bound() {
    let tmp = tempdir().unwrap();
    let root_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let root: &Utf8Path = root_path.as_ref();

    write_file(root, "pkg/__init__.py", "");
    let core_path = write_file(root, "pkg/core.py", "def core():\n    return 1\n");
    write_file(
        root,
        "pkg/service.py",
        "from pkg import core\n\ndef use():\n    return core.core()\n",
    );
    write_file(
        root,
        "tests/test_service.py",
        "from pkg import service\n\ndef test_use():\n    assert service.use() is not None\n",
    );

    let index = ProjectIndex::build(root).unwrap();
    let changed = vec![core_path];

    let impacted_unbounded = index
        .impacted_tests(&changed, None, None, true, false)
        .unwrap();
    let names_unbounded: Vec<_> = impacted_unbounded.iter().map(|t| t.path.as_str()).collect();
    assert!(
        names_unbounded.contains(&"tests/test_service.py"),
        "expected tests/test_service.py without distance limit, got {:?}",
        names_unbounded
    );

    let impacted_capped = index
        .impacted_tests(&changed, None, Some(1), true, false)
        .unwrap();
    let names_capped: Vec<_> = impacted_capped.iter().map(|t| t.path.as_str()).collect();
    assert!(
        !names_capped.contains(&"tests/test_service.py"),
        "did not expect tests/test_service.py with distance_limit=1, got {:?}",
        names_capped
    );
}

#[test]
fn deleted_python_file_still_impacts_importers() {
    let tmp = tempdir().unwrap();
    let root_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let root: &Utf8Path = root_path.as_ref();

    write_file(root, "pkg/__init__.py", "");
    let removed_path = write_file(root, "pkg/foo.py", "def f():\n    return 1\n");
    write_file(root, "tests/test_foo.py", "from pkg import foo\n\n");

    // simulate deletion
    std::fs::remove_file(removed_path.as_std_path()).unwrap();

    let index = ProjectIndex::build(root).unwrap();
    let changed = vec![removed_path];
    let impacted = index
        .impacted_tests(&changed, None, None, true, false)
        .unwrap();

    let names: Vec<_> = impacted.iter().map(|t| t.path.as_str()).collect();
    assert!(
        names.contains(&"tests/test_foo.py"),
        "expected tests/test_foo.py when foo.py is removed, got {:?}",
        names
    );
}

#[test]
fn deleted_top_level_module_impacts_importers() {
    let tmp = tempdir().unwrap();
    let root_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let root: &Utf8Path = root_path.as_ref();

    let removed_path = write_file(root, "foo.py", "def f():\n    return 1\n");
    write_file(
        root,
        "tests/test_bar.py",
        "import foo\n\ndef test_bar():\n    assert hasattr(foo, 'f')\n",
    );

    std::fs::remove_file(removed_path.as_std_path()).unwrap();

    let index = ProjectIndex::build(root).unwrap();
    let changed = vec![removed_path];
    let impacted = index
        .impacted_tests(&changed, None, None, true, false)
        .unwrap();

    let names: Vec<_> = impacted.iter().map(|t| t.path.as_str()).collect();
    assert!(
        names.contains(&"tests/test_bar.py"),
        "expected tests/test_bar.py when foo.py is removed, got {:?}",
        names
    );
}

#[test]
fn conftest_is_not_considered_a_test() {
    let tmp = tempdir().unwrap();
    let root_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let root: &Utf8Path = root_path.as_ref();

    let conftest = write_file(root, "tests/conftest.py", "# helper fixtures\n");
    let real_test = write_file(
        root,
        "tests/test_real.py",
        "def test_ok():\n    assert True\n",
    );

    assert!(
        !is_test_file(conftest.as_std_path()),
        "conftest.py should not be treated as a test file"
    );
    assert!(
        is_test_file(real_test.as_std_path()),
        "test_*.py should be treated as a test file"
    );
}
