//! The release notes the workflow takes from CHANGELOG.md (`scripts/release-notes.sh`), and the
//! checks that the changelog and the workflow stay wired together.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_script(args: &[&str]) -> Output {
    Command::new("bash")
        .arg(manifest_dir().join("scripts/release-notes.sh"))
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("bash runs the script")
}

/// Runs the script on a changelog with the given text.
fn notes_from(changelog: &str, version: &str) -> Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "changelog-{}-{}.md",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::write(&path, changelog).unwrap();
    run_script(&[version, path.to_str().unwrap()])
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const CHANGELOG: &str = "# Changelog

## 0.12.0 (2026-11-01)

Newest notes.

### Added

- one

## 0.11.0: 2026-10-16

Intro paragraph.

### Highlights

- a
- b

Closing line.

## 0.1.0

Oldest.
";

#[test]
fn a_version_gets_the_text_under_its_heading_up_to_the_next_version() {
    let output = notes_from(CHANGELOG, "0.11.0");

    assert!(output.status.success());
    assert_eq!(text(&output), "Intro paragraph.\n\n### Highlights\n\n- a\n- b\n\nClosing line.\n");
}

#[test]
fn the_heading_may_have_a_date_after_a_colon_or_in_brackets_or_nothing() {
    assert_eq!(text(&notes_from(CHANGELOG, "0.12.0")), "Newest notes.\n\n### Added\n\n- one\n", "(date)");
    assert!(text(&notes_from(CHANGELOG, "0.11.0")).starts_with("Intro paragraph."), ": date");
    assert_eq!(text(&notes_from(CHANGELOG, "0.1.0")), "Oldest.\n", "nothing after the version");
}

#[test]
fn sub_headings_are_part_of_the_notes_and_only_a_version_heading_ends_them() {
    let notes = text(&notes_from(CHANGELOG, "0.12.0"));

    assert!(notes.contains("### Added"));
    assert!(!notes.contains("## 0.11.0") && !notes.contains("Intro paragraph."));
}

#[test]
fn a_version_is_not_confused_with_one_that_starts_the_same_way() {
    // 0.1.0 is not a prefix match for 0.11.0 or 0.12.0, and a shorter version has no section.
    assert_eq!(text(&notes_from(CHANGELOG, "0.1.0")), "Oldest.\n");
    for missing in ["0.1", "0", "0.11", "0.12", "0.11.00", "0.11.0.1", "1.11.0"] {
        let output = notes_from(CHANGELOG, missing);
        assert_eq!(output.status.code(), Some(1), "{missing}");
        assert!(output.stdout.is_empty(), "{missing}");
    }
}

#[test]
fn a_version_with_no_section_exits_one_and_prints_nothing() {
    let output = notes_from(CHANGELOG, "9.9.9");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn a_section_with_nothing_in_it_counts_as_missing() {
    let changelog = "## 2.0.0\n\n\n## 1.0.0\n\nreal notes\n\n## 0.9.0\n   \n\t\n";

    assert_eq!(notes_from(changelog, "2.0.0").status.code(), Some(1), "followed by another version");
    assert_eq!(notes_from(changelog, "0.9.0").status.code(), Some(1), "at the end of the file");
    assert_eq!(text(&notes_from(changelog, "1.0.0")), "real notes\n");
}

#[test]
fn blank_lines_at_either_end_are_dropped_and_the_ones_between_are_kept() {
    let changelog = "## 1.0.0\n\n\n\nfirst\n\n\nsecond\n\n\n\n## 0.9.0\n\nold\n";

    assert_eq!(text(&notes_from(changelog, "1.0.0")), "first\n\n\nsecond\n");
}

#[test]
fn the_last_section_runs_to_the_end_of_the_file() {
    assert_eq!(text(&notes_from("## 1.0.0\n\nonly notes\nsecond line", "1.0.0")), "only notes\nsecond line\n");
}

#[test]
fn a_changelog_with_no_version_headings_has_no_notes() {
    assert_eq!(notes_from("# Changelog\n\nnothing yet\n", "1.0.0").status.code(), Some(1));
    assert_eq!(notes_from("", "1.0.0").status.code(), Some(1));
}

#[test]
fn running_it_without_a_version_is_a_usage_error() {
    for args in [&[][..], &[""]] {
        let output = run_script(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"), "{args:?}");
    }
}

#[test]
fn a_missing_changelog_file_is_a_failure_with_no_notes() {
    let output = run_script(&["1.0.0", "/nonexistent/CHANGELOG.md"]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn the_changelog_has_notes_for_the_version_being_built() {
    let version = env!("CARGO_PKG_VERSION");

    let output = run_script(&[version]);

    assert!(
        output.status.success(),
        "CHANGELOG.md has no notes under a heading `## {version}`, so the release would fall back to \
         generated notes. Add them before bumping the version."
    );
    let notes = text(&output);
    assert!(notes.trim().len() > 100, "the notes for {version} are suspiciously short: {notes:?}");
    assert!(!notes.lines().any(|line| line.starts_with("## ")), "only this version's section");
}

#[test]
fn the_changelog_leads_with_the_newest_version() {
    let changelog = fs::read_to_string(manifest_dir().join("CHANGELOG.md")).unwrap();

    let first_heading = changelog.lines().find(|line| line.starts_with("## ")).unwrap();

    assert!(first_heading.starts_with(&format!("## {}", env!("CARGO_PKG_VERSION"))), "{first_heading}");
}

#[test]
fn the_workflow_uses_the_changelog_and_falls_back_to_generated_notes_only_when_it_has_none() {
    let workflow = fs::read_to_string(manifest_dir().join(".github/workflows/release.yml")).unwrap();

    assert!(workflow.contains("scripts/release-notes.sh \"$version\""), "it asks the script for this version");
    assert!(workflow.contains("--notes-file release-notes.md"));
    assert_eq!(workflow.matches("--generate-notes").count(), 1, "generated notes are only the fallback");
    let else_branch = workflow.find("else").unwrap();
    assert!(workflow.find("--generate-notes").unwrap() > else_branch, "and only in the else branch");
    assert!(workflow.contains("::warning::CHANGELOG.md has no notes"), "and it says so");
    assert!(workflow.contains("\"${notes[@]}\""), "the chosen option is what is passed to gh");
}

#[test]
fn the_release_job_has_the_changelog_and_the_script_to_read() {
    let workflow = fs::read_to_string(manifest_dir().join(".github/workflows/release.yml")).unwrap();

    let release_job = &workflow[workflow.find("\n  release:").unwrap()..];
    let checkout = release_job.find("actions/checkout").expect("the release job checks out the repository");
    let create = release_job.find("Create GitHub release").unwrap();
    assert!(checkout < create, "the repository is checked out before the release is made");
    assert!(manifest_dir().join("scripts/release-notes.sh").is_file());
}
