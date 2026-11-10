//! `publish` against a mock of GitLab's API and stand-in registries: what
//! it publishes, in which order, what it tags, and what later pipelines
//! say about earlier releases.

mod common;

use common::publish::{GitLab, TOKEN};
use common::{Registry, Repo};
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::{Value, json};

const CONFIG: &str = "[npm]\ndiscover = true\n";

fn package_json(name: &str, version: &str, dependencies: &str) -> String {
    format!(
        "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"dependencies\": {{{dependencies}}}\n}}\n"
    )
}

/// An npm workspace with `core` and `api`, which depends on `core`, released
/// once as `core` 1.0.0 and `api` 1.4.2, and then, by the commit at HEAD,
/// as `core` 2.0.0 and `api` 1.4.3. Returns the repository and the two
/// release commits.
fn workspace() -> (Repo, String, String) {
    let repo = Repo::with(
        CONFIG,
        &[
            (
                "package.json",
                "{\n  \"name\": \"root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
            ),
            (
                ".cutwater/versions.toml",
                "[units]\napi = \"1.4.2\"\ncore = \"1.0.0\"\n",
            ),
            (
                "packages/core/package.json",
                &package_json("core", "1.0.0", ""),
            ),
            (
                "packages/api/package.json",
                &package_json("api", "1.4.2", "\"core\": \"^1.0.0\""),
            ),
        ],
    );
    let first = repo.git(&["rev-parse", "HEAD"]);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\napi = \"1.4.3\"\ncore = \"2.0.0\"\n",
    );
    repo.write(
        "packages/core/package.json",
        &package_json("core", "2.0.0", ""),
    );
    repo.write(
        "packages/api/package.json",
        &package_json("api", "1.4.3", "\"core\": \"^2.0.0\""),
    );
    repo.write(
        "packages/core/CHANGELOG.md",
        "# core\n\n## 2.0.0\n\n### Major Changes\n\n- Drop the old API.\n\n## 1.0.0\n\n- First.\n",
    );
    repo.write(
        "packages/api/CHANGELOG.md",
        "# api\n\n## 1.4.3\n\n### Patch Changes\n\n- Updated dependencies\n  - core@2.0.0\n",
    );
    let release = repo.commit("Release");
    (repo, first, release)
}

/// `publish` in the repository, against the mock and the registries, with
/// the bot token, in a pipeline of `main`.
fn publish(repo: &Repo, gitlab: &GitLab, registry: &Registry) -> assert_cmd::Command {
    let mut command = repo.cutwater();
    command
        .arg("publish")
        .env("PATH", registry.path())
        .env("GITLAB_CI", "true")
        .env("CI_COMMIT_BRANCH", "main")
        .env("CI_SERVER_URL", gitlab.url())
        .env("CI_PROJECT_ID", "7")
        .env("CUTWATER_GITLAB_TOKEN", TOKEN);
    command
}

fn record(output: &[u8]) -> Value {
    serde_json::from_slice(output).unwrap()
}

fn states(record: &Value) -> Vec<(String, String)> {
    record["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| {
            (
                unit["unit"].as_str().unwrap().to_string(),
                unit["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[test]
fn release_commit_publishes_in_order_and_then_tags() {
    let (repo, _, release) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let (create, releases) = gitlab.releases();

    let assert = publish(&repo, &gitlab, &registry)
        .arg("--json")
        .assert()
        .success()
        .stderr(contains("Published `core` 2.0.0"))
        .stderr(contains(TOKEN).not());
    create.expect(2).assert();
    let output = assert.get_output();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));

    let queries = registry.queries();
    let publications: Vec<&str> = queries
        .lines()
        .filter(|line| line.starts_with("npm publish"))
        .collect();
    assert_eq!(
        publications,
        [
            "npm publish core@2.0.0 --tag latest",
            "npm publish api@1.4.3 --tag latest"
        ]
    );
    assert_eq!(
        releases.json(),
        [
            json!({
                "tag_name": "core-v2.0.0",
                "ref": release,
                "name": "core-v2.0.0",
                "description": "### Major Changes\n\n- Drop the old API."
            }),
            json!({
                "tag_name": "api-v1.4.3",
                "ref": release,
                "name": "api-v1.4.3",
                "description": "### Patch Changes\n\n- Updated dependencies\n  - core@2.0.0"
            }),
        ]
    );
    let record = record(&output.stdout);
    assert_eq!(
        states(&record),
        [pair("core", "published"), pair("api", "published")]
    );
    assert_eq!(record["commit"], json!(release));
    assert_eq!(record["units"][1]["tag"], json!("api-v1.4.3"));
    assert_eq!(record["units"][1]["dist_tag"], json!("latest"));
    assert_eq!(
        record["units"][1]["registry"],
        json!("the npm registry the environment configures")
    );
    assert_eq!(record["tags"][1]["tag"], json!("api-v1.4.3"));
    assert_eq!(record["tags"][1]["state"], json!("created"));
}

#[test]
fn retry_publishes_only_what_is_missing() {
    let (repo, _, release) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    registry.fail(
        "api",
        "npm error code E500\nnpm error 500 Internal Server Error",
    );
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let (create, releases) = gitlab.releases();

    publish(&repo, &gitlab, &registry)
        .assert()
        .code(2)
        .stdout(contains("Published `core` 2.0.0"))
        .stderr(contains("cannot publish `api` 1.4.3"))
        .stderr(contains("500 Internal Server Error"));
    create.expect(1).assert();
    assert_eq!(releases.json()[0]["tag_name"], json!("core-v2.0.0"));
    assert_eq!(registry.versions("core"), ["1.0.0", "2.0.0"]);

    registry.recover("api");
    let mut gitlab = GitLab::new();
    gitlab.tag("core-v2.0.0", &release, true);
    gitlab.no_tag("api-v1.4.3");
    let (create, releases) = gitlab.releases();
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stdout(contains("Published `api` 1.4.3"));
    create.expect(1).assert();
    assert_eq!(releases.json()[0]["tag_name"], json!("api-v1.4.3"));
    assert_eq!(registry.versions("core"), ["1.0.0", "2.0.0"]);
    assert_eq!(registry.versions("api"), ["1.4.2", "1.4.3"]);
}

#[test]
fn dependent_of_a_failed_unit_waits() {
    let (repo, _, _) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    registry.fail("core", "npm error code E500");
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();

    let assert = publish(&repo, &gitlab, &registry)
        .arg("--json")
        .assert()
        .code(2)
        .stderr(contains(
            "`api` is not published, because `core`, which it depends on, is not published",
        ));
    for mock in writes {
        mock.assert();
    }
    assert_eq!(registry.versions("api"), ["1.4.2"]);
    let record = record(&assert.get_output().stdout);
    assert_eq!(
        states(&record),
        [pair("core", "failed"), pair("api", "blocked")]
    );
    assert_eq!(record["tags"][0]["state"], json!("incomplete"));
}

#[test]
fn fixed_group_is_tagged_once_complete() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n\n[[group]]\nname = \"ui-kit\"\nunits = [\"ui\", \"web\"]\n",
        &[
            (
                "package.json",
                "{\n  \"name\": \"root\",\n  \"private\": true,\n  \"workspaces\": [\"ui\", \"web\"]\n}\n",
            ),
            (
                ".cutwater/versions.toml",
                "[units]\nui = \"2.1.0\"\nweb = \"2.1.0\"\n",
            ),
            ("ui/package.json", &package_json("ui", "2.1.0", "")),
            ("web/package.json", &package_json("web", "2.1.0", "")),
        ],
    );
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nui = \"2.2.0\"\nweb = \"2.2.0\"\n",
    );
    repo.write("ui/package.json", &package_json("ui", "2.2.0", ""));
    repo.write("web/package.json", &package_json("web", "2.2.0", ""));
    repo.write(
        "ui/CHANGELOG.md",
        "# ui\n\n## 2.2.0\n\n### Minor Changes\n\n- Add.\n",
    );
    repo.write(
        "web/CHANGELOG.md",
        "# web\n\n## 2.2.0\n\nNo changes in this release.\n",
    );
    let release = repo.commit("Release");
    let registry = Registry::new(&[("ui", &["2.1.0"]), ("web", &["2.1.0"])]);
    registry.fail("web", "npm error code E500");
    let mut gitlab = GitLab::new();
    gitlab.no_tag("v2.2.0");
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(2)
        .stdout(contains("`v2.2.0` is not created"));
    for mock in writes {
        mock.assert();
    }
    assert_eq!(registry.versions("ui"), ["2.1.0", "2.2.0"]);

    registry.recover("web");
    let mut gitlab = GitLab::new();
    gitlab.no_tag("v2.2.0");
    let (create, releases) = gitlab.releases();
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stdout(contains("`ui` 2.2.0 is in"));
    create.expect(1).assert();
    assert_eq!(
        releases.json(),
        [json!({
            "tag_name": "v2.2.0",
            "ref": release,
            "name": "v2.2.0",
            "description": "## ui\n\n### Minor Changes\n\n- Add.\n\n## web\n\nNo changes in this release."
        })]
    );
    assert_eq!(registry.versions("ui"), ["2.1.0", "2.2.0"]);
}

#[test]
fn existing_tags() {
    let (repo, first, release) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0", "2.0.0"]), ("api", &["1.4.2", "1.4.3"])]);
    // A tag left without its release gets it.
    let mut gitlab = GitLab::new();
    gitlab.tag("core-v2.0.0", &release, true);
    gitlab.tag("api-v1.4.3", &release, false);
    let (create, releases) = gitlab.releases();
    let assert = publish(&repo, &gitlab, &registry)
        .arg("--json")
        .assert()
        .success()
        .stderr(contains(
            "Created the release of the existing tag `api-v1.4.3`.",
        ));
    create.expect(1).assert();
    assert_eq!(
        releases.json()[0]["description"],
        json!("### Patch Changes\n\n- Updated dependencies\n  - core@2.0.0")
    );
    let record = record(&assert.get_output().stdout);
    assert_eq!(
        states(&record),
        [pair("core", "present"), pair("api", "present")]
    );
    assert!(!registry.queries().contains("npm publish"));

    // A tag on another commit is never moved.
    let mut gitlab = GitLab::new();
    gitlab.tag("core-v2.0.0", &first, false);
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains(format!(
            "the tag `core-v2.0.0` points at {first}, but the release commit of `core` is \
             {release}"
        )));
    for mock in writes {
        mock.assert();
    }
    assert!(!registry.queries().contains("npm publish"));
}

#[test]
fn protected_tags_the_bot_may_not_create() {
    let (repo, _, release) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    let bot = r#"{"id": 1, "username": "release-bot"}"#;
    let mut gitlab = GitLab::with(
        bot,
        30,
        r#"[{"name": "*-v*", "create_access_levels": [{"id": 1, "access_level": 40}]}]"#,
    );
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains(
            "the bot `release-bot`, with the access level 30 (Developer), may not create the \
             tag `core-v2.0.0`, which the protected tag `*-v*` (may be created by Maintainer or \
             higher) covers",
        ))
        .stdout(contains("Nothing was published"));
    for mock in writes {
        mock.assert();
    }
    assert!(!registry.queries().contains("npm publish"));

    // The release of an existing protected tag needs the same right.
    let mut gitlab = GitLab::with(
        bot,
        30,
        r#"[{"name": "*-v*", "create_access_levels": [{"id": 1, "access_level": 40}]}]"#,
    );
    gitlab.tag("core-v2.0.0", &release, true);
    gitlab.tag("api-v1.4.3", &release, false);
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains(
            "may not create the release of the tag `api-v1.4.3`, which the protected tag `*-v*`",
        ));
    for mock in writes {
        mock.assert();
    }

    // A rule naming the bot allows it.
    let mut gitlab = GitLab::with(
        bot,
        30,
        r#"[{"name": "*-v*", "create_access_levels": [{"id": 1, "access_level": 40}, {"id": 2, "access_level": 40, "user_id": 1}]}]"#,
    );
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let (create, _) = gitlab.releases();
    publish(&repo, &gitlab, &registry).assert().success();
    create.expect(2).assert();

    // A rule naming the bot needs it to be a project member, even as an
    // instance administrator.
    let mut gitlab = GitLab::with(
        r#"{"id": 1, "username": "release-bot", "is_admin": true}"#,
        0,
        r#"[{"name": "*-v*", "create_access_levels": [{"id": 2, "access_level": 40, "user_id": 1}]}]"#,
    );
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains("may not create the tag `core-v2.0.0`"));
    for mock in writes {
        mock.assert();
    }
}

#[test]
fn bot_below_developer_publishes_nothing() {
    let (repo, _, _) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::with(r#"{"id": 1, "username": "release-bot"}"#, 20, "[]");
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains("has the access level 20 (Reporter)"));
    for mock in writes {
        mock.assert();
    }
}

#[test]
fn later_pipelines_publish_nothing() {
    let (repo, _, release) = workspace();
    repo.write("README.md", "later\n");
    repo.commit("A later merge request");

    // The release pipeline failed to publish `api`.
    let registry = Registry::new(&[("core", &["1.0.0", "2.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::new();
    gitlab.tag("core-v2.0.0", &release, true);
    gitlab.no_tag("api-v1.4.3");
    gitlab.pipeline(&release, "failed");
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains(format!(
            "`api` 1.4.3 is missing from the npm registry the environment configures, although \
             the pipeline of its release commit {release} finished; retry the `publish` job of \
             pipeline #41 of that commit: https://gitlab.example/acme/p/-/pipelines/41"
        )));
    for mock in writes {
        mock.assert();
    }
    let queries = registry.queries();
    assert!(!queries.contains("npm publish"), "{queries}");
    // The tag and its release prove `core`.
    assert!(!queries.contains("npm view core"), "{queries}");

    // The release pipeline still runs.
    let mut gitlab = GitLab::new();
    gitlab.tag("core-v2.0.0", &release, true);
    gitlab.no_tag("api-v1.4.3");
    gitlab.pipeline(&release, "running");
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stderr(contains(format!(
            "warning: `api` 1.4.3 is not yet in the npm registry the environment configures; \
             the pipeline of its release commit {release} still runs"
        )));

    // Published, but not tagged.
    let registry = Registry::new(&[("core", &["1.0.0", "2.0.0"]), ("api", &["1.4.2", "1.4.3"])]);
    let mut gitlab = GitLab::new();
    gitlab.tag("core-v2.0.0", &release, true);
    gitlab.no_tag("api-v1.4.3");
    gitlab.pipeline(&release, "failed");
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stderr(contains(format!(
            "warning: `api` 1.4.3 is published, but its release commit {release} has no tag \
             `api-v1.4.3` with a release yet; retry the `publish` job of pipeline #41"
        )));
    for mock in writes {
        mock.assert();
    }
}

#[test]
fn seeded_versions_need_neither_registry_nor_gitlab() {
    let repo = Repo::with(
        CONFIG,
        &[
            ("package.json", &package_json("api", "1.4.2", "")),
            (
                ".cutwater/versions.toml",
                "[units]\napi = { version = \"1.4.2\", seeded = true }\n",
            ),
        ],
    );
    let registry = Registry::new(&[]);
    repo.cutwater()
        .arg("publish")
        .env("PATH", registry.path())
        .assert()
        .success()
        .stdout(contains("No unit has a released version to publish."));
    assert_eq!(registry.queries(), "");
}

#[test]
fn dry_run_writes_nothing() {
    let (repo, _, release) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();
    let assert = publish(&repo, &gitlab, &registry)
        .args(["--dry-run", "--json"])
        .assert()
        .success()
        .stderr(contains("Would publish `core` 2.0.0"))
        .stderr(contains("Would publish `api` 1.4.3"))
        .stderr(contains(format!(
            "Would tag {release} as `api-v1.4.3` and create its release."
        )));
    for mock in writes {
        mock.assert();
    }
    assert!(!registry.queries().contains("npm publish"));
    let record = record(&assert.get_output().stdout);
    assert_eq!(record["dry_run"], json!(true));
    assert_eq!(
        states(&record),
        [pair("core", "would-publish"), pair("api", "would-publish")]
    );
    assert_eq!(record["tags"][0]["state"], json!("would-create"));
}

#[test]
fn maintenance_release_is_superseded_and_prerelease_takes_its_label() {
    let (repo, _, _) = workspace();
    repo.write(
        ".cutwater/versions.toml",
        "[units]\napi = \"1.4.4\"\ncore = \"2.0.0-beta.0\"\n",
    );
    repo.write(
        "packages/core/package.json",
        &package_json("core", "2.0.0-beta.0", ""),
    );
    repo.write(
        "packages/api/package.json",
        &package_json("api", "1.4.4", "\"core\": \"^2.0.0-beta.0\""),
    );
    repo.write(
        "packages/api/CHANGELOG.md",
        "# api\n\n## 1.4.4\n\n### Patch Changes\n\n- Fix.\n",
    );
    repo.write(
        "packages/core/CHANGELOG.md",
        "# core\n\n## 2.0.0-beta.0\n\n### Major Changes\n\n- Drop the old API.\n",
    );
    repo.commit("Prerelease");
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2", "2.0.0"])]);
    registry.latest("api", "2.0.0");
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0-beta.0");
    gitlab.no_tag("api-v1.4.4");
    let (create, _) = gitlab.releases();
    publish(&repo, &gitlab, &registry).assert().success();
    create.expect(2).assert();
    let queries = registry.queries();
    let publications: Vec<&str> = queries
        .lines()
        .filter(|line| line.starts_with("npm publish"))
        .collect();
    assert_eq!(
        publications,
        [
            "npm publish core@2.0.0-beta.0 --tag beta",
            "npm publish api@1.4.4 --tag superseded"
        ]
    );
}

#[test]
fn release_assets() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n\n[[unit]]\nname = \"@acme/cli\"\nassets = [\"dist/*.tar.gz\"]\n",
        &[
            ("package.json", &package_json("@acme/cli", "1.0.0", "")),
            (
                ".cutwater/versions.toml",
                "[units]\n\"@acme/cli\" = \"1.0.0\"\n",
            ),
            (
                "CHANGELOG.md",
                "# @acme/cli\n\n## 1.0.0\n\n### Major Changes\n\n- First.\n",
            ),
        ],
    );
    let release = repo.git(&["rev-parse", "HEAD"]);
    repo.write("dist/cli-linux.tar.gz", "linux");
    repo.write("dist/cli-macos.tar.gz", "macos");
    repo.write("dist/notes.txt", "not an asset");
    let registry = Registry::new(&[("@acme/cli", &["1.0.0"])]);
    let packages = "/api/v4/projects/7/packages";

    // A file of the same name with another checksum.
    let mut gitlab = GitLab::new();
    gitlab.no_tag("v1.0.0");
    gitlab.get(
        packages,
        r#"[{"id": 3, "name": "-acme-cli", "version": "1.0.0"}]"#,
    );
    gitlab.get(
        "/api/v4/projects/7/packages/3/package_files",
        r#"[{"file_name": "cli-linux.tar.gz", "file_sha256": "0000"}]"#,
    );
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .code(1)
        .stderr(contains(
            "the generic package `-acme-cli` 1.0.0 holds `cli-linux.tar.gz` with the checksum \
             0000, but dist/cli-linux.tar.gz has",
        ));
    for mock in writes {
        mock.assert();
    }

    // One file uploaded before, with the same checksum.
    let linux = cutwater::assets::sha256(b"linux");
    let mut gitlab = GitLab::new();
    gitlab.no_tag("v1.0.0");
    gitlab.get(
        packages,
        r#"[{"id": 3, "name": "-acme-cli", "version": "1.0.0"}]"#,
    );
    gitlab.get(
        "/api/v4/projects/7/packages/3/package_files",
        &format!(r#"[{{"file_name": "cli-linux.tar.gz", "file_sha256": "{linux}"}}]"#),
    );
    let (upload, uploads) = gitlab.capture(
        "PUT",
        "/api/v4/projects/7/packages/generic/-acme-cli/1.0.0/cli-macos.tar.gz",
    );
    let (create, releases) = gitlab.releases();
    let assert = publish(&repo, &gitlab, &registry)
        .arg("--json")
        .assert()
        .success();
    upload.expect(1).assert();
    assert_eq!(uploads.raw(), [b"macos".to_vec()]);
    create.expect(1).assert();
    let url = |file: &str| {
        format!(
            "{}/api/v4/projects/7/packages/generic/-acme-cli/1.0.0/{file}",
            gitlab.url()
        )
    };
    assert_eq!(
        releases.json()[0]["assets"],
        json!({"links": [
            {"name": "cli-linux.tar.gz", "url": url("cli-linux.tar.gz"),
             "direct_asset_path": "/cli-linux.tar.gz", "link_type": "package"},
            {"name": "cli-macos.tar.gz", "url": url("cli-macos.tar.gz"),
             "direct_asset_path": "/cli-macos.tar.gz", "link_type": "package"},
        ]})
    );
    assert_eq!(releases.json()[0]["ref"], json!(release));
    let record = record(&assert.get_output().stdout);
    let assets: Vec<(String, String)> = record["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|asset| {
            (
                asset["file"].as_str().unwrap().to_string(),
                asset["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        assets,
        [
            pair("cli-linux.tar.gz", "present"),
            pair("cli-macos.tar.gz", "uploaded")
        ]
    );
    assert_eq!(states(&record), [pair("@acme/cli", "present")]);
}

#[test]
fn cargo_crate() {
    let repo = Repo::with(
        "[cargo]\ndiscover = true\n",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"tool\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", ""),
            (".cutwater/versions.toml", "[units]\ntool = \"0.2.0\"\n"),
            (
                "CHANGELOG.md",
                "# tool\n\n## 0.2.0\n\n### Minor Changes\n\n- Add.\n",
            ),
        ],
    );
    repo.run("cargo", &["generate-lockfile", "--offline"]);
    repo.git(&["add", "Cargo.lock"]);
    repo.git(&["commit", "--quiet", "--amend", "--no-edit"]);
    let registry = Registry::new(&[("tool", &["0.1.0"])]);
    let mut gitlab = GitLab::new();
    gitlab.no_tag("v0.2.0");
    let (create, _) = gitlab.releases();
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stdout(contains(
            "Published `tool` 0.2.0 to the default Cargo registry.",
        ));
    create.expect(1).assert();
    assert!(
        registry
            .queries()
            .contains("cargo publish tool@0.2.0 --locked --package tool"),
        "{}",
        registry.queries()
    );
}

#[test]
fn missing_tool_is_named_before_anything_is_published() {
    let (repo, _, _) = workspace();
    repo.write("pnpm-workspace.yaml", "packages:\n  - packages/*\n");
    repo.git(&["add", "pnpm-workspace.yaml"]);
    repo.git(&["commit", "--quiet", "--amend", "--no-edit"]);
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let writes = gitlab.no_writes();
    // The stand-ins, git and the system's tools, but no pnpm.
    let git = which::which("git").unwrap();
    let path = std::env::join_paths(
        std::env::split_paths(&registry.path())
            .take(1)
            .chain([git.parent().unwrap().to_path_buf()])
            .chain(["/usr/bin".into(), "/bin".into()]),
    )
    .unwrap();
    assert!(which::which_in("pnpm", Some(&path), ".").is_err());
    publish(&repo, &gitlab, &registry)
        .env("PATH", path)
        .assert()
        .code(2)
        .stderr(contains(
            "publishing the npm units needs pnpm, which is not on the PATH",
        ));
    for mock in writes {
        mock.assert();
    }
}

#[test]
fn own_release_goes_ahead_of_an_earlier_missing_one() {
    let (repo, _, first_release) = workspace();
    repo.write(
        ".cutwater/versions.toml",
        "[units]\napi = \"1.4.3\"\ncore = \"2.0.1\"\n",
    );
    repo.write(
        "packages/core/package.json",
        &package_json("core", "2.0.1", ""),
    );
    repo.write(
        "packages/core/CHANGELOG.md",
        "# core\n\n## 2.0.1\n\n### Patch Changes\n\n- Fix.\n\n## 2.0.0\n\n- Drop.\n",
    );
    let release = repo.commit("Release core");
    let registry = Registry::new(&[("core", &["1.0.0", "2.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.1");
    gitlab.no_tag("api-v1.4.3");
    gitlab.pipeline(&first_release, "failed");
    let (create, releases) = gitlab.releases();
    let assert = publish(&repo, &gitlab, &registry)
        .arg("--json")
        .assert()
        .code(1)
        .stderr(contains("`api` 1.4.3 is missing"));
    create.expect(1).assert();
    assert_eq!(releases.json()[0]["tag_name"], json!("core-v2.0.1"));
    assert_eq!(releases.json()[0]["ref"], json!(release));
    let record = record(&assert.get_output().stdout);
    assert_eq!(
        states(&record),
        [pair("core", "published"), pair("api", "missing")]
    );
}

#[test]
fn scoped_tags_with_their_release_prove_publication() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\ntag = \"{unit}@{version}\"\n",
        &[
            ("package.json", &package_json("@acme/core", "2.0.0", "")),
            (
                ".cutwater/versions.toml",
                "[units]\n\"@acme/core\" = \"2.0.0\"\n",
            ),
            ("CHANGELOG.md", "# @acme/core\n\n## 2.0.0\n\n- Core.\n"),
        ],
    );
    let release = repo.git(&["rev-parse", "HEAD"]);
    let registry = Registry::new(&[]);

    // A retry in the release commit's pipeline after the run that tagged it.
    let mut gitlab = GitLab::new();
    gitlab.tag("@acme/core@2.0.0", &release, true);
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stdout(contains(
            "Everything this commit releases is published and tagged.",
        ));
    for mock in writes {
        mock.assert();
    }

    // A later pipeline asks no registry.
    repo.write("README.md", "later\n");
    repo.commit("A later merge request");
    let mut gitlab = GitLab::new();
    gitlab.tag("@acme/core@2.0.0", &release, true);
    let writes = gitlab.no_writes();
    publish(&repo, &gitlab, &registry)
        .assert()
        .success()
        .stderr(contains("warning").not());
    for mock in writes {
        mock.assert();
    }
    assert_eq!(registry.queries(), "");
}

#[test]
fn dependent_waits_for_a_missing_earlier_release() {
    let (repo, _, first_release) = workspace();
    repo.write(
        ".cutwater/versions.toml",
        "[units]\napi = \"1.4.4\"\ncore = \"2.0.0\"\n",
    );
    repo.write(
        "packages/api/package.json",
        &package_json("api", "1.4.4", "\"core\": \"^2.0.0\""),
    );
    repo.write(
        "packages/api/CHANGELOG.md",
        "# api\n\n## 1.4.4\n\n### Patch Changes\n\n- Fix.\n",
    );
    repo.commit("Release api");
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2", "1.4.3"])]);
    for (status, dry_run) in [("failed", false), ("running", false), ("running", true)] {
        let mut gitlab = GitLab::new();
        gitlab.no_tag("core-v2.0.0");
        gitlab.no_tag("api-v1.4.4");
        gitlab.pipeline(&first_release, status);
        let writes = gitlab.no_writes();
        let mut command = publish(&repo, &gitlab, &registry);
        if dry_run {
            command.arg("--dry-run");
        }
        let assert = command
            .arg("--json")
            .assert()
            .code(2)
            .stderr(contains("Would publish `api`").not())
            .stderr(contains(format!(
                "`api` is not published, because `core` 2.0.0, which it depends on, is missing \
                 from its registry; retry this job once the pipeline of its release commit \
                 {first_release} published it"
            )));
        for mock in writes {
            mock.assert();
        }
        assert!(!registry.queries().contains("npm publish"), "{status}");
        let record = record(&assert.get_output().stdout);
        assert_eq!(
            states(&record),
            [pair("core", "missing"), pair("api", "blocked")],
            "{status}"
        );
        assert_eq!(record["tags"][0]["state"], json!("incomplete"), "{status}");
    }
}

#[test]
fn adapter_failure_fails_only_its_unit() {
    let (repo, _, _) = workspace();
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    // Publishing `api` cannot even read its package once `core` is out.
    registry.after_publishing("core", "rm packages/api/package.json");
    let mut gitlab = GitLab::new();
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    let (create, releases) = gitlab.releases();
    let assert = publish(&repo, &gitlab, &registry)
        .arg("--json")
        .assert()
        .code(2)
        .stderr(contains(
            "cannot publish `api` 1.4.3: packages/api/package.json",
        ));
    create.expect(1).assert();
    assert_eq!(releases.json()[0]["tag_name"], json!("core-v2.0.0"));
    let record = record(&assert.get_output().stdout);
    assert_eq!(
        states(&record),
        [pair("core", "published"), pair("api", "failed")]
    );
    assert_eq!(record["tags"][0]["state"], json!("created"));
    assert_eq!(record["tags"][1]["state"], json!("incomplete"));
}

#[test]
fn group_rules_need_a_project_shared_with_the_group() {
    let (repo, _, _) = workspace();
    let bot = r#"{"id": 1, "username": "release-bot"}"#;
    let protected = r#"[{"name": "*-v*", "create_access_levels": [
        {"id": 1, "access_level": 40}, {"id": 2, "access_level": 40, "group_id": 40}]}]"#;
    let shares = |groups: &str| format!(r#"{{"id": 7, "shared_with_groups": [{groups}]}}"#);
    let shared = r#"{"group_id": 40, "group_name": "Release", "group_full_path": "acme/release", "group_access_level": 30}"#;
    let member = r#"{"id": 1, "username": "release-bot", "access_level": 30}"#;
    let cases = [
        // The bot is a member of the group, but the project is not shared
        // with it, as with the group that owns the project.
        (
            shares(""),
            Some(member),
            1,
            "may not create the tag `core-v2.0.0`",
        ),
        // Shared, but the bot is a member only through a group above it.
        (
            shares(shared),
            None,
            1,
            "may not create the tag `core-v2.0.0`",
        ),
        // GitLab does not say which groups the project is shared with.
        (
            r#"{"id": 7}"#.to_string(),
            Some(member),
            2,
            "cannot tell whether the bot `release-bot` may create the tag `core-v2.0.0`, which \
             a protected tag allows members of a group to create: group 40: GitLab does not list \
             the groups the project is shared with to the bot",
        ),
    ];
    for (project, membership, code, message) in cases {
        let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
        let mut gitlab = GitLab::with(bot, 30, protected);
        gitlab.no_tag("core-v2.0.0");
        gitlab.no_tag("api-v1.4.3");
        gitlab.get("/api/v4/projects/7", &project);
        match membership {
            Some(member) => gitlab.get("/api/v4/groups/40/members/1", member),
            None => gitlab.missing("/api/v4/groups/40/members/1"),
        };
        let writes = gitlab.no_writes();
        publish(&repo, &gitlab, &registry)
            .assert()
            .code(code)
            .stderr(contains(message));
        for mock in writes {
            mock.assert();
        }
        assert!(!registry.queries().contains("npm publish"), "{message}");
    }

    // Shared with the group, and the bot is a Developer of it.
    let registry = Registry::new(&[("core", &["1.0.0"]), ("api", &["1.4.2"])]);
    let mut gitlab = GitLab::with(bot, 30, protected);
    gitlab.no_tag("core-v2.0.0");
    gitlab.no_tag("api-v1.4.3");
    gitlab.get("/api/v4/projects/7", &shares(shared));
    gitlab.get("/api/v4/groups/40/members/1", member);
    let (create, _) = gitlab.releases();
    publish(&repo, &gitlab, &registry).assert().success();
    create.expect(2).assert();
}
