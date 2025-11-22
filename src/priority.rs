use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct Priority {
    pub filename_match: u8,
    pub distance: usize,
}

pub fn priority(path: &str, distance: usize, changed_leaves: &HashSet<String>) -> Priority {
    let filename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let mut filename_match = 2u8;
    for leaf in changed_leaves {
        if filename.starts_with(&format!("test_{leaf}")) || filename.contains(&format!("_{leaf}")) {
            filename_match = 0;
            break;
        }
        if filename.contains(leaf) {
            filename_match = filename_match.min(1);
        }
    }

    Priority {
        filename_match,
        distance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prioritizes_prefix_match_best() {
        let p = priority("tests/test_foo.py", 0, &leaves(&["foo"]));
        assert_eq!(p.filename_match, 0);
        assert_eq!(p.distance, 0);
    }

    #[test]
    fn partial_contains_is_secondary() {
        let p = priority("tests/integration_bar_test.py", 2, &leaves(&["bar"]));
        assert_eq!(p.filename_match, 0);
        assert_eq!(p.distance, 2);
    }

    #[test]
    fn unrelated_files_get_low_priority() {
        let p = priority("tests/other.py", 5, &leaves(&["foo"]));
        assert_eq!(p.filename_match, 2);
        assert_eq!(p.distance, 5);
    }
}
