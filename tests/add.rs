mod common;

use common::Repo;
use predicates::prelude::*;
use predicates::str::contains;

const CONFIG: &str = r#"
[cargo]
discover = true
"#;

fn workspace() -> Repo {
    Repo::with(
        CONFIG,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"1.4.2\"\n",
            ),
            (
                "crates/api/Cargo.toml",
                "[package]\nname = \"api\"\nversion.workspace = true\n",
            ),
            (
                "crates/core/Cargo.toml",
                "[package]\nname = \"core\"\nversion.workspace = true\n",
            ),
            ("crates/api/src/lib.rs", ""),
            ("crates/core/src/lib.rs", ""),
        ],
    )
}

fn only_changeset(repo: &Repo) -> String {
    let files = repo.list(".changeset");
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with(".md"), "{files:?}");
    repo.read(&format!(".changeset/{}", files[0]))
}

#[test]
fn non_interactive_changeset() {
    let repo = workspace();
    repo.cutwater()
        .args([
            "add",
            "--unit",
            "api:minor",
            "--message",
            "Add the export endpoint",
        ])
        .assert()
        .success()
        .stdout(contains("Created .changeset/"));
    assert_eq!(
        only_changeset(&repo),
        "---\n\"api\": minor\n---\n\nAdd the export endpoint\n"
    );
}

#[test]
fn several_units() {
    let repo = workspace();
    repo.cutwater()
        .args([
            "add",
            "--unit",
            "core:major",
            "--unit",
            "api:none",
            "-m",
            "Drop the old API",
        ])
        .assert()
        .success();
    assert_eq!(
        only_changeset(&repo),
        "---\n\"core\": major\n\"api\": none\n---\n\nDrop the old API\n"
    );
}

#[test]
fn empty_changeset() {
    let repo = workspace();
    repo.cutwater().args(["add", "--empty"]).assert().success();
    assert_eq!(only_changeset(&repo), "---\n---\n");
}

#[test]
fn unknown_unit() {
    let repo = workspace();
    repo.cutwater()
        .args(["add", "--unit", "billing:patch", "--message", "Fix"])
        .assert()
        .code(2)
        .stderr(contains("`billing` is not a unit").and(contains("`api`, `core`")));
    assert!(repo.list(".changeset").is_empty());
}

#[test]
fn unknown_bump() {
    let repo = workspace();
    repo.cutwater()
        .args(["add", "--unit", "api:huge", "--message", "Fix"])
        .assert()
        .code(2)
        .stderr(contains("unknown bump `huge`"));
}

#[test]
fn unit_named_twice() {
    let repo = workspace();
    repo.cutwater()
        .args([
            "add",
            "--unit",
            "api:patch",
            "--unit",
            "api:minor",
            "-m",
            "Fix",
        ])
        .assert()
        .code(2)
        .stderr(contains("names `api` twice"));
}

#[test]
fn without_arguments_or_terminal() {
    let repo = workspace();
    repo.cutwater()
        .arg("add")
        .assert()
        .code(2)
        .stderr(contains("--unit <name>:<bump>").and(contains("--empty")));
    assert!(repo.list(".changeset").is_empty());
}

#[test]
fn created_changeset_passes_check() {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "-b", "feature"]);
    repo.write("crates/api/src/lib.rs", "pub fn export() {}\n");
    repo.cutwater()
        .args([
            "add",
            "--unit",
            "api:minor",
            "--message",
            "Add the export endpoint",
        ])
        .assert()
        .success();
    repo.commit("export");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains("api: .changeset/"));
}
