use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use once_cell::sync::Lazy;
use pathdiff::diff_paths;
use regex::Regex;
use roxmltree::{Document, Node};

/// Convert pytest-style JUnit XML into GitHub Actions log annotations.
#[derive(Args, Debug)]
pub struct FormatArgs {
    /// Path to a pytest JUnit XML report (e.g., produced with `pytest --junitxml=report.xml`)
    pub path: PathBuf,

    /// Emit warnings for skipped tests (by default, skips are ignored)
    #[arg(long)]
    pub include_skipped: bool,
}

/// Entry point for the `testdiff format` subcommand.
pub fn format_junit(args: &FormatArgs) -> Result<()> {
    let xml = std::fs::read_to_string(&args.path)
        .with_context(|| format!("Failed to read {}", args.path.display()))?;

    let doc = Document::parse(&xml)
        .with_context(|| format!("Failed to parse XML in {}", args.path.display()))?;

    let cwd = std::env::current_dir()?;
    let mut reported = 0usize;

    for case in doc
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
    {
        if let Some(child) = first_child(&case, &["failure", "error"]) {
            let (file, line) = derive_location(&case, child.text());
            let message = format!(
                "{}: {}",
                testcase_name(&case),
                pick_message(&child, "Test failed")
            );
            emit_annotation("error", file.as_deref(), line, &message, &cwd);
            reported += 1;
        } else if args.include_skipped {
            if let Some(child) = first_child(&case, &["skipped"]) {
                let (file, line) = derive_location(&case, child.text());
                let message = format!(
                    "{}: {}",
                    testcase_name(&case),
                    pick_message(&child, "Test skipped")
                );
                emit_annotation("warning", file.as_deref(), line, &message, &cwd);
                reported += 1;
            }
        }
    }

    if reported == 0 {
        eprintln!(
            "No failures, errors, or skipped tests found in {}",
            args.path.display()
        );
    }

    Ok(())
}

fn first_child<'a>(case: &'a Node<'_, '_>, names: &[&str]) -> Option<Node<'a, 'a>> {
    case.children()
        .find(|child| child.is_element() && names.iter().any(|tag| child.has_tag_name(*tag)))
}

fn testcase_name(case: &Node<'_, '_>) -> String {
    let class = case.attribute("classname");
    let name = case.attribute("name");

    match (class, name) {
        (Some(class), Some(name)) => format!("{class}.{name}"),
        (_, Some(name)) => name.to_string(),
        _ => "(unknown test)".to_string(),
    }
}

fn pick_message(node: &Node<'_, '_>, default: &str) -> String {
    if let Some(msg) = node.attribute("message") {
        if !msg.trim().is_empty() {
            return msg.trim().to_string();
        }
    }

    let text = node.text().unwrap_or_default().trim();
    if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
        return line.trim().to_string();
    }

    default.to_string()
}

fn derive_location(case: &Node<'_, '_>, body: Option<&str>) -> (Option<PathBuf>, Option<usize>) {
    let file_attr = case.attribute("file").map(PathBuf::from);
    let line_attr = case.attribute("line").and_then(|s| s.parse::<usize>().ok());

    if file_attr.is_some() || line_attr.is_some() {
        return (file_attr, line_attr);
    }

    if let Some(body) = body {
        if let Some(caps) = FILE_LINE_RE.captures(body) {
            let file = caps.get(1).map(|m| PathBuf::from(m.as_str()));
            let line = caps.get(2).and_then(|m| m.as_str().parse::<usize>().ok());
            if file.is_some() || line.is_some() {
                return (file, line);
            }
        }
    }

    (None, None)
}

fn build_annotation(
    level: &str,
    file: Option<&Path>,
    line: Option<usize>,
    message: &str,
    cwd: &Path,
) -> String {
    let mut parts = Vec::new();
    if let Some(file) = file {
        let display = diff_paths(file, cwd).unwrap_or_else(|| file.to_path_buf());
        parts.push(format!("file={}", display.display()));
    }

    if let Some(line) = line {
        parts.push(format!("line={line}"));
    }

    let mut prefix = format!("::{level}");
    if !parts.is_empty() {
        prefix.push(' ');
        prefix.push_str(&parts.join(","));
    }
    prefix.push_str("::");

    format!("{}{}", prefix, escape_for_github(message))
}

fn emit_annotation(
    level: &str,
    file: Option<&Path>,
    line: Option<usize>,
    message: &str,
    cwd: &Path,
) {
    println!("{}", build_annotation(level, file, line, message, cwd));
}

fn escape_for_github(message: &str) -> String {
    message
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

static FILE_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    // Typical pytest traceback fragment: File "/path/to/test.py", line 12
    Regex::new(r#"File \"([^\"]+)\", line (\d+)"#)
        .expect("regex for pytest traceback should compile")
});

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn derives_locations_from_attributes() {
        let xml = r#"<testsuite><testcase classname="pkg.test" name="test_it" file="/tmp/test.py" line="10"><failure message="boom">Traceback</failure></testcase></testsuite>"#;

        let doc = Document::parse(xml).unwrap();
        let case = doc
            .descendants()
            .find(|n| n.has_tag_name("testcase"))
            .unwrap();
        let failure = first_child(&case, &["failure"]).unwrap();
        let (file, line) = derive_location(&case, failure.text());

        assert_eq!(file.unwrap().display().to_string(), "/tmp/test.py");
        assert_eq!(line, Some(10));
    }

    #[test]
    fn derives_locations_from_traceback() {
        let xml = r#"<testsuite><testcase classname="pkg.test" name="test_it"><failure><![CDATA[Traceback (most recent call last):
  File "/tmp/test.py", line 22, in test_it
    assert False]]></failure></testcase></testsuite>"#;

        let doc = Document::parse(xml).unwrap();
        let case = doc
            .descendants()
            .find(|n| n.has_tag_name("testcase"))
            .unwrap();
        let failure = first_child(&case, &["failure"]).unwrap();
        let (file, line) = derive_location(&case, failure.text());

        assert_eq!(file.unwrap().display().to_string(), "/tmp/test.py");
        assert_eq!(line, Some(22));
    }

    #[test]
    fn escape_for_github_replaces_specials() {
        let input = "line1%\r\nline2";
        let escaped = escape_for_github(input);
        assert_eq!(escaped, "line1%25%0D%0Aline2");
    }

    #[test]
    fn pick_message_prefers_attribute_then_first_line() {
        let xml_attr = r#"<failure message="boom">ignored body</failure>"#;
        let doc_attr = Document::parse(xml_attr).unwrap();
        let node_attr = doc_attr.root_element();
        assert_eq!(pick_message(&node_attr, "fallback"), "boom");

        let xml_body = r#"<failure>

line1
line2
</failure>"#;
        let doc_body = Document::parse(xml_body).unwrap();
        let node_body = doc_body.root_element();
        assert_eq!(pick_message(&node_body, "fallback"), "line1");
    }

    #[test]
    fn build_annotation_formats_rel_and_line() {
        let cwd = PathBuf::from("/repo");
        let file = PathBuf::from("/repo/tests/test_example.py");
        let msg = "fail!";
        let out = build_annotation("error", Some(&file), Some(12), msg, &cwd);
        assert_eq!(out, "::error file=tests/test_example.py,line=12::fail!");
    }
}
