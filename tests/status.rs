mod common;

use common::Repo;
use predicates::str::contains;

/// An npm workspace: `api` depends on `core` with `^1.2.0`, and on
/// `testkit` for development only; `web` and `ui` are a fixed group.
fn workspace() -> Repo {
    Repo::with(
        "[npm]\ndiscover = true\n\n[[group]]\nname = \"frontend\"\nunits = [\"web\", \"ui\"]\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\napi = \"1.4.2\"\ncore = \"1.2.0\"\ntestkit = { version = \"0.4.0\", seeded = true }\nui = \"2.1.0\"\nweb = \"2.1.0\"\n",
            ),
            (
                "package.json",
                r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/api/package.json",
                r#"{"name": "api", "version": "1.4.2", "dependencies": {"core": "^1.2.0"}, "devDependencies": {"testkit": "^0.4.0"}}"#,
            ),
            (
                "packages/core/package.json",
                r#"{"name": "core", "version": "1.2.0"}"#,
            ),
            (
                "packages/testkit/package.json",
                r#"{"name": "testkit", "version": "0.4.0"}"#,
            ),
            (
                "packages/web/package.json",
                r#"{"name": "web", "version": "2.1.0", "dependencies": {"ui": "^2.1.0"}}"#,
            ),
            (
                "packages/ui/package.json",
                r#"{"name": "ui", "version": "2.1.0"}"#,
            ),
            (
                ".changeset/brave-lions-dance.md",
                "---\ncore: major\ntestkit: major\n---\n\nDrop the old API.\n",
            ),
            (
                ".changeset/green-ui.md",
                "---\n\"ui\": minor\n---\n\nNew button.\n",
            ),
            (".changeset/quiet-owls-rest.md", "---\n---\n\nDocs only.\n"),
        ],
    )
}

#[test]
fn status_lists_changesets_and_releases() {
    let repo = workspace();
    let output = repo.cutwater().arg("status").assert().success();
    expect_test::expect![[r#"
        Pending changesets:
          .changeset/brave-lions-dance.md: core major, testkit major
          .changeset/green-ui.md: ui minor
          .changeset/quiet-owls-rest.md: empty
        Releases, in the order of publication:
          core 1.2.0 -> 2.0.0: major (.changeset/brave-lions-dance.md)
          api 1.4.2 -> 1.4.3: core 2.0.0 is outside its runtime range ^1.2.0
          testkit 0.4.0 (seeded) -> 1.0.0: major (.changeset/brave-lions-dance.md)
          ui 2.1.0 -> 2.2.0: minor (.changeset/green-ui.md)
          web 2.1.0 -> 2.2.0: fixed group frontend
    "#]]
    .assert_eq(&String::from_utf8_lossy(&output.get_output().stdout));
}

#[test]
fn status_as_json() {
    let repo = workspace();
    let output = repo
        .cutwater()
        .args(["status", "--json"])
        .assert()
        .success();
    expect_test::expect![[r#"
        {
          "prerelease": null,
          "changesets": [
            {
              "file": ".changeset/brave-lions-dance.md",
              "releases": {
                "core": "major",
                "testkit": "major"
              }
            },
            {
              "file": ".changeset/green-ui.md",
              "releases": {
                "ui": "minor"
              }
            },
            {
              "file": ".changeset/quiet-owls-rest.md",
              "releases": {}
            }
          ],
          "releases": [
            {
              "unit": "core",
              "adapter": "npm",
              "current": "1.2.0",
              "seeded": false,
              "next": "2.0.0",
              "bump": "major",
              "reasons": [
                {
                  "kind": "changesets",
                  "bump": "major",
                  "changesets": [
                    ".changeset/brave-lions-dance.md"
                  ]
                }
              ]
            },
            {
              "unit": "api",
              "adapter": "npm",
              "current": "1.4.2",
              "seeded": false,
              "next": "1.4.3",
              "bump": "patch",
              "reasons": [
                {
                  "kind": "dependencies",
                  "dependencies": [
                    {
                      "unit": "core",
                      "version": "2.0.0",
                      "range": "^1.2.0",
                      "kind": "runtime"
                    }
                  ]
                }
              ]
            },
            {
              "unit": "testkit",
              "adapter": "npm",
              "current": "0.4.0",
              "seeded": true,
              "next": "1.0.0",
              "bump": "major",
              "reasons": [
                {
                  "kind": "changesets",
                  "bump": "major",
                  "changesets": [
                    ".changeset/brave-lions-dance.md"
                  ]
                }
              ]
            },
            {
              "unit": "ui",
              "adapter": "npm",
              "current": "2.1.0",
              "seeded": false,
              "next": "2.2.0",
              "bump": "minor",
              "reasons": [
                {
                  "kind": "changesets",
                  "bump": "minor",
                  "changesets": [
                    ".changeset/green-ui.md"
                  ]
                }
              ]
            },
            {
              "unit": "web",
              "adapter": "npm",
              "current": "2.1.0",
              "seeded": false,
              "next": "2.2.0",
              "bump": "minor",
              "reasons": [
                {
                  "kind": "group",
                  "group": "frontend"
                }
              ]
            }
          ],
          "removed": []
        }
    "#]]
    .assert_eq(&String::from_utf8_lossy(&output.get_output().stdout));
}

#[test]
fn unknown_unit_in_a_changeset_is_refused() {
    let repo = workspace();
    repo.write(".changeset/odd.md", "---\napy: minor\n---\n\nOdd.\n");
    repo.cutwater()
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains(
            ".changeset/odd.md: names `apy` with `minor`, but `apy` is not a unit",
        ));
}

#[test]
fn inherited_version_outside_a_group_is_refused() {
    let repo = Repo::with(
        "[cargo]\ndiscover = true\n",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.0.0\"\nedition = \"2024\"\n",
            ),
            (
                "crates/core/Cargo.toml",
                "[package]\nname = \"core\"\nversion.workspace = true\nedition.workspace = true\n",
            ),
            ("crates/core/src/lib.rs", ""),
            (
                "crates/api/Cargo.toml",
                "[package]\nname = \"api\"\nversion.workspace = true\nedition.workspace = true\n",
            ),
            ("crates/api/src/lib.rs", ""),
        ],
    );
    repo.cutwater()
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains(
            "units `api`, `core` share their version, written in Cargo.toml \
         `workspace.package.version`; make them one fixed group",
        ));
}
