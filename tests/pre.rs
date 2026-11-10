mod common;

use common::Repo;
use predicates::str::contains;

fn package() -> Repo {
    Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (".cutwater/versions.toml", "[units]\napi = \"1.4.2\"\n"),
            ("package.json", r#"{"name": "api", "version": "1.4.2"}"#),
        ],
    )
}

/// Applies a changeset `name` that releases `api`, and commits.
fn release(repo: &Repo, name: &str, bump: &str, entry: &str) -> String {
    repo.write(
        &format!(".changeset/{name}.md"),
        &format!(
            "---
api: {bump}
---

{entry}
"
        ),
    );
    let output = repo.cutwater().arg("version").assert().success();
    repo.commit(entry);
    String::from_utf8_lossy(&output.get_output().stdout).into_owned()
}

#[test]
fn prerelease_sequence() {
    let repo = package();
    repo.cutwater()
        .args(["pre", "enter", "beta"])
        .assert()
        .success();
    repo.commit("enter prerelease mode");
    let first = release(&repo, "first", "minor", "Add the export endpoint.");
    assert!(
        first.contains("  api 1.4.2 -> 1.5.0-beta.0: minor"),
        "{first}"
    );
    let second = release(&repo, "second", "minor", "Accept filters.");
    assert!(
        second.contains("  api 1.5.0-beta.0 -> 1.5.0-beta.1: minor"),
        "{second}"
    );
    expect_test::expect![[r##"
        # Prerelease mode: `cutwater pre enter` and `cutwater pre exit` set the mode,
        # `cutwater version` records the prereleases.

        mode = "pre"
        label = "beta"

        [units.api]
        version = "1.5.0"
        prereleases = 2

        [[units.api.entry]]
        changeset = ".changeset/first.md"
        bump = "minor"
        text = "Add the export endpoint."

        [[units.api.entry]]
        changeset = ".changeset/second.md"
        bump = "minor"
        text = "Accept filters."
    "##]]
    .assert_eq(&repo.read(".cutwater/pre.toml"));

    repo.cutwater().args(["pre", "exit"]).assert().success();
    repo.commit("leave prerelease mode");
    repo.cutwater()
        .arg("status")
        .assert()
        .success()
        .stdout(contains(
            "Leaving prerelease mode `beta`: the next release is final.\nNo pending changesets.\n",
        ))
        .stdout(contains(
            "  api 1.5.0-beta.1 -> 1.5.0: final release after the `beta` prereleases\n",
        ));
    repo.cutwater()
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(contains("\"reasons\": [\n        {\n          \"kind\": \"prerelease\",\n          \"label\": \"beta\"\n"));
    repo.cutwater().arg("version").assert().success();
    assert!(!repo.path().join(".cutwater/pre.toml").exists());
    assert!(
        repo.read(".cutwater/versions.toml")
            .contains("api = \"1.5.0\"\n")
    );
    assert!(repo.read("package.json").contains("\"version\": \"1.5.0\""));
    expect_test::expect![[r#"
        # api

        ## 1.5.0

        ### Minor Changes

        - Add the export endpoint.
        - Accept filters.

        ## 1.5.0-beta.1

        ### Minor Changes

        - Accept filters.

        ## 1.5.0-beta.0

        ### Minor Changes

        - Add the export endpoint.
    "#]]
    .assert_eq(&repo.read("CHANGELOG.md"));
}

#[test]
fn entering_and_leaving() {
    let repo = package();
    repo.cutwater()
        .args(["pre", "enter", "beta.1"])
        .assert()
        .code(2)
        .stderr(contains("`beta.1` cannot label prereleases"));
    repo.cutwater()
        .args(["pre", "exit"])
        .assert()
        .code(2)
        .stderr(contains("not in prerelease mode"));
    repo.cutwater()
        .args(["pre", "enter", "rc"])
        .assert()
        .success();
    repo.cutwater()
        .args(["pre", "enter", "beta"])
        .assert()
        .code(2)
        .stderr(contains("already records prerelease mode `rc`"));
    // Nothing was prereleased, so leaving needs no release.
    repo.cutwater()
        .args(["pre", "exit"])
        .assert()
        .success()
        .stdout(contains("Deleted .cutwater/pre.toml"));
    assert!(!repo.path().join(".cutwater/pre.toml").exists());
}

#[test]
fn changing_the_label_keeps_counting() {
    let repo = package();
    repo.cutwater()
        .args(["pre", "enter", "beta"])
        .assert()
        .success();
    release(&repo, "first", "minor", "Add the export endpoint.");
    repo.cutwater().args(["pre", "exit"]).assert().success();
    repo.cutwater()
        .args(["pre", "enter", "rc"])
        .assert()
        .success();
    let output = release(&repo, "third", "patch", "Fix.");
    assert!(
        output.contains("  api 1.5.0-beta.0 -> 1.5.0-rc.1: patch"),
        "{output}"
    );
}

#[test]
fn final_release_after_prereleases_caused_by_a_dependency() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\nui = \"0.4.1\"\nweb = \"1.3.0\"\n",
            ),
            (
                "package.json",
                r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/web/package.json",
                r#"{"name": "web", "version": "1.3.0"}"#,
            ),
            (
                "packages/ui/package.json",
                r#"{"name": "ui", "version": "0.4.1", "dependencies": {"web": "^1.3.0"}}"#,
            ),
        ],
    );
    repo.cutwater()
        .args(["pre", "enter", "beta"])
        .assert()
        .success();
    repo.write(".changeset/a.md", "---\nweb: minor\n---\n\nAdd.\n");
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains(
            "  ui 0.4.1 -> 0.4.2-beta.0: web 1.4.0-beta.0 is outside",
        ));
    assert!(
        repo.read("packages/ui/package.json")
            .contains(r#""web": "^1.4.0-beta.0""#)
    );
    repo.cutwater().args(["pre", "exit"]).assert().success();
    repo.cutwater().arg("version").assert().success();
    // No final release depends on a prerelease, and the final entry names
    // the dependency's final version.
    assert_eq!(
        repo.read("packages/ui/package.json"),
        r#"{"name": "ui", "version": "0.4.2", "dependencies": {"web": "^1.4.0"}}"#
    );
    assert!(
        repo.read("packages/ui/CHANGELOG.md").starts_with(
            "# ui\n\n## 0.4.2\n\n### Patch Changes\n\n- Updated dependencies\n  - web@1.4.0\n\n## 0.4.2-beta.0\n"
        ),
        "{}",
        repo.read("packages/ui/CHANGELOG.md")
    );
}

#[test]
fn final_release_keeps_a_peer_major_from_prerelease_mode() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\nui = \"0.4.1\"\nweb = \"1.3.0\"\n",
            ),
            (
                "package.json",
                r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/web/package.json",
                r#"{"name": "web", "version": "1.3.0"}"#,
            ),
            (
                "packages/ui/package.json",
                r#"{"name": "ui", "version": "0.4.1", "peerDependencies": {"web": "^1.0.0"}}"#,
            ),
        ],
    );
    repo.cutwater()
        .args(["pre", "enter", "beta"])
        .assert()
        .success();
    repo.write(".changeset/a.md", "---\nweb: major\n---\n\nBreak.\n");
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("  ui 0.4.1 -> 1.0.0-beta.0:"));
    assert!(
        repo.read(".cutwater/pre.toml")
            .contains("dependency_bump = \"major\"\n")
    );
    repo.cutwater().args(["pre", "exit"]).assert().success();
    repo.cutwater().arg("version").assert().success();
    let changelog = repo.read("packages/ui/CHANGELOG.md");
    assert!(
        changelog.starts_with(
            "# ui\n\n## 1.0.0\n\n### Major Changes\n\n- Updated dependencies\n  - web@2.0.0\n\n## 1.0.0-beta.0\n"
        ),
        "{changelog}"
    );
}
