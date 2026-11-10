//! `mr` against a mock of GitLab's API: what it writes, when it leaves the
//! release merge request alone, and how it handles the bot token.

mod common;

use std::sync::{Arc, Mutex};

use common::Repo;
use mockito::{Matcher, Mock, Server, ServerGuard};
use predicates::prelude::*;
use predicates::str::contains;

const TOKEN: &str = "glpat-integration-test-token";
const RELEASE_BRANCH: &str = "/api/v4/projects/7/repository/branches/cutwater%2Frelease";

/// A single npm package `api` at 1.4.2 with a pending minor changeset,
/// committed on `main`.
fn package() -> Repo {
    Repo::with(
        "[npm]\ndiscover = true\n\n[gitlab]\nmerge-queue = [\"merge-bot\"]\n",
        &[
            (".cutwater/versions.toml", "[units]\napi = \"1.4.2\"\n"),
            (
                "package.json",
                "{\n  \"name\": \"api\",\n  \"version\": \"1.4.2\"\n}\n",
            ),
            (
                ".changeset/brave-lions-dance.md",
                "---\napi: minor\n---\n\nAdd the export endpoint.\n",
            ),
        ],
    )
}

/// The request body as form fields, in order.
fn form(body: &[u8]) -> Vec<(String, String)> {
    form_urlencoded::parse(body).into_owned().collect()
}

fn field(form: &[(String, String)], key: &str) -> Option<String> {
    form.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

/// A mock of project 7 on GitLab that accepts the token.
struct GitLab {
    server: ServerGuard,
}

impl GitLab {
    /// The mock of a project that merges with `merge_method`, `merge`,
    /// `rebase_merge` or `ff`, without merged results pipelines.
    fn new(merge_method: &str) -> GitLab {
        GitLab::with_project(&format!(
            r#"{{"id": 7, "merge_method": "{merge_method}", "merge_pipelines_enabled": false}}"#
        ))
    }

    /// The mock of a project that the API describes as `project`.
    fn with_project(project: &str) -> GitLab {
        let mut server = Server::new();
        server
            .mock("GET", "/api/v4/user")
            .match_query(Matcher::Any)
            .match_header("private-token", TOKEN)
            .with_body(r#"{"id": 1, "username": "release-bot"}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7")
            .match_query(Matcher::Any)
            .with_body(project)
            .create();
        GitLab { server }
    }

    fn get(&mut self, path: &str, body: &str) -> Mock {
        self.server
            .mock("GET", path)
            .match_query(Matcher::Any)
            .with_body(body)
            .create()
    }

    fn tip(&mut self, commit: &str) -> Mock {
        self.get(
            "/api/v4/projects/7/repository/branches/main",
            &format!(r#"{{"name": "main", "commit": {{"id": "{commit}", "parent_ids": []}}}}"#),
        )
    }

    fn merge_requests(&mut self, json: &str) -> Mock {
        self.get("/api/v4/projects/7/merge_requests", json)
    }

    fn release_branch(&mut self, head: &str, parent: &str) -> Mock {
        self.get(
            RELEASE_BRANCH,
            &format!(
                r#"{{"name": "cutwater/release", "commit": {{"id": "{head}", "parent_ids": ["{parent}"]}}}}"#
            ),
        )
    }

    fn no_release_branch(&mut self) -> Mock {
        self.server
            .mock("GET", RELEASE_BRANCH)
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Branch Not Found"}"#)
            .create()
    }

    /// A mock that answers `response` and keeps the request's body.
    fn capture(&mut self, method: &str, path: &str, response: &str) -> (Mock, Body) {
        let body = Body::default();
        let kept = body.clone();
        let response = response.to_string();
        let mock = self
            .server
            .mock(method, path)
            .match_query(Matcher::Any)
            .with_status(if method == "POST" { 201 } else { 200 })
            .with_body_from_request(move |request| {
                *kept.0.lock().unwrap() = request.body().unwrap().clone();
                response.clone().into()
            })
            .create();
        (mock, body)
    }

    fn url(&self) -> String {
        self.server.url()
    }
}

#[derive(Clone, Default)]
struct Body(Arc<Mutex<Vec<u8>>>);

impl Body {
    fn form(&self) -> Vec<(String, String)> {
        form(&self.0.lock().unwrap())
    }
}

fn merge_request(iid: u64, extra: &str) -> String {
    format!(
        r#"{{"iid": {iid}, "title": "Release", "description": "old",
            "web_url": "https://gitlab.example/acme/api/-/merge_requests/{iid}",
            "source_project_id": 7, "target_project_id": 7{extra}}}"#
    )
}

/// `mr` in the repository, against the mock, with the bot token.
fn mr(repo: &Repo, gitlab: &GitLab) -> assert_cmd::Command {
    let mut command = repo.cutwater();
    command
        .arg("mr")
        .env("CI_SERVER_URL", gitlab.url())
        .env("CI_PROJECT_ID", "7")
        .env("CUTWATER_GITLAB_TOKEN", TOKEN);
    command
}

/// The working trees of the clone, which `mr` leaves as they were.
fn worktrees(repo: &Repo) -> usize {
    repo.git(&["worktree", "list", "--porcelain"])
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

#[test]
fn first_release_merge_request() {
    let repo = package();
    let tip = repo.git(&["rev-parse", "HEAD"]);
    // A stale working tree of the clone, which `mr` must not prune.
    let stale = tempfile::tempdir().unwrap();
    let stale_tree = stale.path().join("stale");
    repo.git(&[
        "worktree",
        "add",
        "--detach",
        stale_tree.to_str().unwrap(),
        "HEAD",
    ]);
    std::fs::remove_dir_all(&stale_tree).unwrap();
    let before = repo.snapshot();
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    gitlab.no_release_branch();
    let (commit, commit_body) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/repository/commits",
        r#"{"id": "1111111111111111111111111111111111111111"}"#,
    );
    let (create, create_body) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/merge_requests",
        &merge_request(5, ""),
    );

    mr(&repo, &gitlab)
        .assert()
        .success()
        .stdout(contains("api 1.4.2 -> 1.5.0: minor"))
        .stdout(contains(format!(
            "Wrote the release commit 1111111111111111111111111111111111111111 onto \
             `cutwater/release`, on top of {tip}."
        )))
        .stdout(contains(
            "Created the release merge request !5: \
             https://gitlab.example/acme/api/-/merge_requests/5",
        ))
        .stderr(contains("warning").not());
    commit.assert();
    create.assert();

    let commit = commit_body.form();
    assert_eq!(field(&commit, "branch").unwrap(), "cutwater/release");
    assert_eq!(field(&commit, "start_sha").unwrap(), tip);
    assert_eq!(field(&commit, "force").unwrap(), "true");
    assert_eq!(
        field(&commit, "commit_message").unwrap(),
        "Release\n\napi 1.5.0\n"
    );
    let actions: Vec<(String, String)> = commit
        .iter()
        .filter(|(key, _)| key == "actions[][action]" || key == "actions[][file_path]")
        .cloned()
        .collect();
    let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
    assert_eq!(
        actions,
        [
            pair("actions[][action]", "delete"),
            pair("actions[][file_path]", ".changeset/brave-lions-dance.md"),
            pair("actions[][action]", "update"),
            pair("actions[][file_path]", ".cutwater/versions.toml"),
            pair("actions[][action]", "create"),
            pair("actions[][file_path]", "CHANGELOG.md"),
            pair("actions[][action]", "update"),
            pair("actions[][file_path]", "package.json"),
        ]
    );
    assert!(commit.contains(&pair(
        "actions[][content]",
        "{\n  \"name\": \"api\",\n  \"version\": \"1.5.0\"\n}\n"
    )));

    let create = create_body.form();
    assert_eq!(field(&create, "source_branch").unwrap(), "cutwater/release");
    assert_eq!(field(&create, "target_branch").unwrap(), "main");
    assert_eq!(field(&create, "title").unwrap(), "Release");
    for key in ["assignee_id", "assignee_ids", "reviewer_ids"] {
        assert_eq!(field(&create, key), None, "{key}");
    }
    let description = field(&create, "description").unwrap();
    assert!(
        description
            .contains("## api 1.4.2 → 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.\n"),
        "{description}"
    );
    assert!(!description.contains("Warning"), "{description}");

    assert_eq!(repo.snapshot(), before);
    assert_eq!(worktrees(&repo), 2);
}

#[test]
fn git_variables_pointing_elsewhere_are_ignored() {
    let repo = package();
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let siblings = tempfile::tempdir().unwrap();
    let sibling = siblings.path().join("sibling");
    repo.git(&[
        "worktree",
        "add",
        "--detach",
        sibling.to_str().unwrap(),
        "HEAD",
    ]);
    let admin = repo.path().join(".git/worktrees/sibling");
    let index = std::fs::read(admin.join("index")).unwrap();
    // A git hook runs with GIT_DIR set, here to another working tree's
    // entry, and maybe GIT_WORK_TREE to that working tree.
    for work_tree in [false, true] {
        let mut gitlab = GitLab::new("ff");
        gitlab.tip(&tip);
        gitlab.merge_requests("[]");
        gitlab.no_release_branch();
        gitlab.capture(
            "POST",
            "/api/v4/projects/7/repository/commits",
            r#"{"id": "1111111111111111111111111111111111111111"}"#,
        );
        gitlab.capture(
            "POST",
            "/api/v4/projects/7/merge_requests",
            &merge_request(5, ""),
        );
        let mut command = mr(&repo, &gitlab);
        command.env("GIT_DIR", &admin);
        if work_tree {
            command.env("GIT_WORK_TREE", &sibling);
        }
        command.assert().success();
        assert!(
            admin.join("gitdir").is_file(),
            "the sibling's entry is gone"
        );
        assert!(
            std::fs::read(admin.join("index")).unwrap() == index,
            "the sibling's index changed"
        );
        assert!(sibling.join(".changeset/brave-lions-dance.md").is_file());
        assert_eq!(
            std::fs::read_to_string(sibling.join("package.json")).unwrap(),
            "{\n  \"name\": \"api\",\n  \"version\": \"1.4.2\"\n}\n"
        );
        assert_eq!(worktrees(&repo), 2);
    }
}

/// Whether the installed git reads configuration from `GIT_CONFIG_COUNT`,
/// which git 2.31 introduced.
fn git_reads_config_count() -> bool {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8_lossy(&output.stdout);
    let mut numbers = version
        .split_whitespace()
        .nth(2)
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0)) >= (2, 31)
}

#[test]
fn the_callers_git_configuration_reaches_the_fetches() {
    let origin = package();
    let clone = Repo::clone_of(&origin, &["--depth", "1"]);
    origin.write(
        ".changeset/quiet-owls-rest.md",
        "---\napi: patch\n---\n\nFix the export of empty tables.\n",
    );
    let tip = origin.commit("add a changeset");
    // CI rewrites the remote's URL with configuration in the environment.
    let alias = "https://git.invalid/acme/api.git";
    clone.git(&["remote", "set-url", "origin", alias]);
    let key = format!("url.file://{}.insteadOf", origin.path().display());
    let mut configurations = vec![vec![(
        "GIT_CONFIG_PARAMETERS".to_string(),
        format!("'{key}={alias}'"),
    )]];
    if git_reads_config_count() {
        configurations.push(vec![
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            ("GIT_CONFIG_KEY_0".to_string(), key.clone()),
            ("GIT_CONFIG_VALUE_0".to_string(), alias.to_string()),
        ]);
    }
    for configuration in configurations {
        let mut gitlab = GitLab::new("ff");
        gitlab.tip(&tip);
        gitlab.merge_requests("[]");
        // A head the clone lacks, which `mr` looks up on `origin`.
        gitlab.release_branch(&"dead".repeat(10), &tip);
        let (commit, _) = gitlab.capture(
            "POST",
            "/api/v4/projects/7/repository/commits",
            r#"{"id": "9999999999999999999999999999999999999999"}"#,
        );
        gitlab.capture(
            "POST",
            "/api/v4/projects/7/merge_requests",
            &merge_request(5, ""),
        );
        mr(&clone, &gitlab)
            .envs(configuration.iter().map(|(name, value)| (name, value)))
            .assert()
            .success();
        commit.assert();
    }
}

#[test]
fn an_older_pipeline_applies_the_plan_to_the_newest_tip() {
    let origin = package();
    // The pipeline's shallow clone lacks the tip, which `mr` fetches.
    let clone = Repo::clone_of(&origin, &["--depth", "1"]);
    origin.write(
        ".changeset/quiet-owls-rest.md",
        "---\napi: patch\n---\n\nFix the export of empty tables.\n",
    );
    let tip = origin.commit("add a changeset");
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    gitlab.no_release_branch();
    let (_, commit_body) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/repository/commits",
        r#"{"id": "2222222222222222222222222222222222222222"}"#,
    );
    let (_, create_body) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/merge_requests",
        &merge_request(5, ""),
    );

    mr(&clone, &gitlab).assert().success();
    let commit = commit_body.form();
    assert_eq!(field(&commit, "start_sha").unwrap(), tip);
    assert!(commit.contains(&(
        "actions[][file_path]".to_string(),
        ".changeset/quiet-owls-rest.md".to_string()
    )));
    let description = field(&create_body.form(), "description").unwrap();
    assert!(
        description.contains("Fix the export of empty tables."),
        "{description}"
    );
}

/// The description `mr` gives the release of the repository's commit
/// `tip`, with nothing on GitLab yet.
fn release_description(repo: &Repo, tip: &str) -> String {
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(tip);
    gitlab.merge_requests("[]");
    gitlab.no_release_branch();
    gitlab.capture(
        "POST",
        "/api/v4/projects/7/repository/commits",
        r#"{"id": "3333333333333333333333333333333333333333"}"#,
    );
    let (_, create_body) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/merge_requests",
        &merge_request(5, ""),
    );
    mr(repo, &gitlab).assert().success();
    field(&create_body.form(), "description").unwrap()
}

/// Commits onto the branch `cutwater/release` of `origin`, on top of
/// `parent`, the release as `cutwater version` applies it, or with
/// `released` false another change; returns the commit.
fn release_commit(origin: &Repo, parent: &str, released: bool) -> String {
    origin.git(&["checkout", "--quiet", "-B", "cutwater/release", parent]);
    if released {
        origin.cutwater().arg("version").assert().success();
    } else {
        origin.write("package.json", "{}\n");
    }
    let head = origin.commit("Release");
    origin.git(&["checkout", "--quiet", "main"]);
    head
}

#[test]
fn the_same_release_is_not_written_twice() {
    let origin = package();
    let tip = origin.git(&["rev-parse", "HEAD"]);
    let description = release_description(&origin, &tip);
    let head = release_commit(&origin, &tip, true);
    // The pipeline's shallow clone lacks the release branch, which `mr`
    // fetches.
    let clone = Repo::clone_of(&origin, &["--depth", "1"]);
    let mut gitlab = GitLab::new("ff");
    gitlab.release_branch(&head, &tip);
    gitlab.tip(&tip);
    let json = serde_json::json!([{
        "iid": 5, "title": "Release", "description": description,
        "web_url": "https://gitlab.example/acme/api/-/merge_requests/5",
        "source_project_id": 7, "target_project_id": 7,
    }]);
    gitlab.merge_requests(&json.to_string());
    mr(&clone, &gitlab)
        .assert()
        .success()
        .stdout(contains(
            "The release branch `cutwater/release` already holds this release.",
        ))
        .stdout(contains("The release merge request !5 is up to date"));
}

#[test]
fn an_unchanged_release_on_an_older_tip_stays_under_merged_results() {
    let origin = package();
    let older = origin.git(&["rev-parse", "HEAD"]);
    let head = release_commit(&origin, &older, true);
    origin.write("README.md", "# api\n");
    let tip = origin.commit("a merge request without changesets");

    for (merge_method, merged_results, rewritten) in [
        ("merge", true, false),
        ("rebase_merge", false, true),
        ("ff", false, true),
    ] {
        let clone = Repo::clone_of(&origin, &["--depth", "1"]);
        let mut gitlab = GitLab::with_project(&format!(
            r#"{{"id": 7, "merge_method": "{merge_method}", "merge_pipelines_enabled": {merged_results}}}"#
        ));
        gitlab.tip(&tip);
        gitlab.merge_requests(&format!("[{}]", merge_request(5, "")));
        gitlab.release_branch(&head, &older);
        let commit = gitlab
            .server
            .mock("POST", "/api/v4/projects/7/repository/commits")
            .match_query(Matcher::Any)
            .with_status(201)
            .with_body(r#"{"id": "5555555555555555555555555555555555555555"}"#)
            .expect(usize::from(rewritten))
            .create();
        gitlab
            .server
            .mock("PUT", "/api/v4/projects/7/merge_requests/5")
            .match_query(Matcher::Any)
            .with_body(merge_request(5, ""))
            .create();
        mr(&clone, &gitlab).assert().success();
        commit.assert();
    }
}

#[test]
fn a_release_branch_head_that_origin_lacks_is_rewritten() {
    let origin = package();
    let tip = origin.git(&["rev-parse", "HEAD"]);
    let clone = Repo::clone_of(&origin, &[]);
    let moved = release_commit(&origin, &tip, true);
    let missing = "dead".repeat(10);
    // The branch was deleted, or moved past the head GitLab reported.
    for exists in [false, true] {
        if exists {
            origin.git(&["branch", "--force", "cutwater/release", &moved]);
        } else {
            origin.git(&["branch", "--delete", "--force", "cutwater/release"]);
        }
        let mut gitlab = GitLab::new("ff");
        gitlab.tip(&tip);
        gitlab.merge_requests("[]");
        gitlab.release_branch(&missing, &tip);
        let (commit, _) = gitlab.capture(
            "POST",
            "/api/v4/projects/7/repository/commits",
            r#"{"id": "7777777777777777777777777777777777777777"}"#,
        );
        gitlab.capture(
            "POST",
            "/api/v4/projects/7/merge_requests",
            &merge_request(5, ""),
        );
        mr(&clone, &gitlab).assert().success();
        commit.assert();
    }

    // A reused clone that holds the head but not its parent, where the
    // project merges with merge commits, after the branch was deleted.
    let origin = package();
    let older = origin.git(&["rev-parse", "HEAD"]);
    let head = release_commit(&origin, &older, true);
    origin.write("README.md", "# api\n");
    let tip = origin.commit("a merge request without changesets");
    let clone = Repo::clone_of(&origin, &["--depth", "1", "--branch", "cutwater/release"]);
    origin.git(&["branch", "--delete", "--force", "cutwater/release"]);
    let mut gitlab = GitLab::new("merge");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    gitlab.release_branch(&head, &older);
    let (commit, _) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/repository/commits",
        r#"{"id": "8888888888888888888888888888888888888888"}"#,
    );
    gitlab.capture(
        "POST",
        "/api/v4/projects/7/merge_requests",
        &merge_request(5, ""),
    );
    mr(&clone, &gitlab).assert().success();
    commit.assert();
}

#[test]
fn an_open_merge_request_is_updated() {
    let repo = package();
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let mut gitlab = GitLab::new("merge");
    gitlab.tip(&tip);
    gitlab.merge_requests(&format!(
        "[{}]",
        merge_request(5, r#", "assignees": [{"username": "maintainer"}]"#)
    ));
    let head = release_commit(&repo, &tip, false);
    gitlab.release_branch(&head, &tip);
    let (commit, _) = gitlab.capture(
        "POST",
        "/api/v4/projects/7/repository/commits",
        r#"{"id": "6666666666666666666666666666666666666666"}"#,
    );
    let (edit, edit_body) = gitlab.capture(
        "PUT",
        "/api/v4/projects/7/merge_requests/5",
        &merge_request(5, ""),
    );
    mr(&repo, &gitlab)
        .assert()
        .success()
        .stdout(contains("Updated the release merge request !5"))
        .stderr(contains(
            "warning: the project merges with merge commits and runs neither merged results \
             pipelines nor merge trains",
        ));
    commit.assert();
    edit.assert();
    let edit = edit_body.form();
    assert_eq!(field(&edit, "title").unwrap(), "Release");
    assert!(
        field(&edit, "description")
            .unwrap()
            .contains("> **Warning:** the project merges with merge commits")
    );
    for key in ["state_event", "assignee_id", "assignee_ids"] {
        assert_eq!(field(&edit, key), None, "{key}");
    }
}

#[test]
fn a_queued_merge_request_is_left_alone() {
    let repo = package();
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let cases = [
        (
            r#", "assignees": [{"username": "Merge-Bot"}]"#,
            false,
            "is assigned to the merge queue account `Merge-Bot`",
        ),
        (
            r#", "merge_when_pipeline_succeeds": true"#,
            false,
            "is set to merge automatically",
        ),
        ("", true, "is in a merge train"),
    ];
    for (extra, train, reason) in cases {
        let mut gitlab = GitLab::with_project(&format!(
            r#"{{"id": 7, "merge_method": "ff", "merge_trains_enabled": {train}}}"#
        ));
        gitlab.tip(&tip);
        gitlab.merge_requests(&format!("[{}]", merge_request(5, extra)));
        if train {
            gitlab.get(
                "/api/v4/projects/7/merge_trains/merge_requests/5",
                r#"{"id": 3, "status": "fresh"}"#,
            );
        }
        mr(&repo, &gitlab)
            .assert()
            .success()
            .stdout(contains(format!("The release merge request !5 {reason}")));
    }
}

#[test]
fn nothing_pending_closes_the_merge_request() {
    let repo = package();
    repo.remove(".changeset/brave-lions-dance.md");
    let tip = repo.commit("revert the change");
    // A queued release merge request is closed too: it would release
    // changes that are no longer pending.
    for extra in ["", r#", "assignees": [{"username": "merge-bot"}]"#] {
        let mut gitlab = GitLab::new("ff");
        gitlab.tip(&tip);
        gitlab.merge_requests(&format!("[{}]", merge_request(5, extra)));
        let (note, note_body) = gitlab.capture(
            "POST",
            "/api/v4/projects/7/merge_requests/5/notes",
            r#"{"id": 9}"#,
        );
        let (close, close_body) = gitlab.capture(
            "PUT",
            "/api/v4/projects/7/merge_requests/5",
            &merge_request(5, ""),
        );
        mr(&repo, &gitlab)
            .assert()
            .success()
            .stdout(contains("No pending changesets: nothing to release."))
            .stdout(contains(
                "Closed the release merge request !5: nothing is pending.",
            ));
        note.assert();
        close.assert();
        assert!(
            field(&note_body.form(), "body")
                .unwrap()
                .contains("no changeset is pending")
        );
        assert_eq!(
            close_body.form(),
            [("state_event".to_string(), "close".to_string())]
        );
    }
}

#[test]
fn a_changelog_behind_a_symbolic_link() {
    let repo = package();
    repo.write("docs/changes.md", "# api\n");
    std::os::unix::fs::symlink("docs/changes.md", repo.path().join("CHANGELOG.md")).unwrap();
    let tip = repo.commit("a changelog in the documentation");
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    gitlab.no_release_branch();
    mr(&repo, &gitlab)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains(
            "  delete .changeset/brave-lions-dance.md\n  update .cutwater/versions.toml\n  \
             update docs/changes.md\n  update package.json\n",
        ))
        .stdout(contains("CHANGELOG.md").not());

    repo.remove("CHANGELOG.md");
    std::os::unix::fs::symlink("../changes.md", repo.path().join("CHANGELOG.md")).unwrap();
    let tip = repo.commit("a changelog outside the repository");
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    mr(&repo, &gitlab)
        .arg("--dry-run")
        .assert()
        .code(2)
        .stderr(contains(
            "CHANGELOG.md is a symbolic link to ../changes.md, which does not resolve",
        ));

    let outside = tempfile::tempdir().unwrap();
    let changes = outside.path().join("changes.md");
    std::fs::write(&changes, "# api\n").unwrap();
    repo.remove("CHANGELOG.md");
    std::os::unix::fs::symlink(&changes, repo.path().join("CHANGELOG.md")).unwrap();
    let tip = repo.commit("a changelog elsewhere");
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    mr(&repo, &gitlab)
        .arg("--dry-run")
        .assert()
        .code(2)
        .stderr(contains(format!(
            "CHANGELOG.md is a symbolic link to {}, outside the repository",
            changes.display()
        )));
    assert_eq!(std::fs::read_to_string(&changes).unwrap(), "# api\n");
}

#[test]
fn dry_run_writes_nothing() {
    let repo = package();
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let mut gitlab = GitLab::new("ff");
    gitlab.tip(&tip);
    gitlab.merge_requests("[]");
    gitlab.no_release_branch();
    mr(&repo, &gitlab)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains(format!(
            "Would write the release commit onto `cutwater/release`, on top of {tip}:\n  \
             delete .changeset/brave-lions-dance.md\n  update .cutwater/versions.toml\n  \
             create CHANGELOG.md\n  update package.json\n"
        )))
        .stdout(contains(
            "Would create the release merge request `Release`.",
        ));
}

#[test]
fn the_job_token_is_refused() {
    let repo = package();
    let gitlab = GitLab::new("ff");
    let refusals = [
        (None, None),
        (Some("glcbt-64_abcdefghijklmnop"), None),
        (Some(TOKEN), Some(TOKEN)),
    ];
    for (token, job_token) in refusals {
        let mut command = repo.cutwater();
        command
            .arg("mr")
            .env("CI_SERVER_URL", gitlab.url())
            .env("CI_PROJECT_ID", "7");
        if let Some(token) = token {
            command.env("CUTWATER_GITLAB_TOKEN", token);
        }
        if let Some(job_token) = job_token {
            command.env("CI_JOB_TOKEN", job_token);
        }
        let assert = command
            .assert()
            .code(2)
            .stderr(contains("CUTWATER_GITLAB_TOKEN"))
            .stderr(contains("start no pipelines"));
        let output = assert.get_output();
        for stream in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(stream);
            assert!(!text.contains(TOKEN) && !text.contains("glcbt-"), "{text}");
        }
    }
}

#[test]
fn a_refused_token_is_not_shown() {
    let repo = package();
    // A self-managed instance may serve GitLab under a path.
    for prefix in ["", "/gitlab"] {
        let mut server = Server::new();
        server
            .mock("GET", format!("{prefix}/api/v4/user").as_str())
            .match_query(Matcher::Any)
            .with_status(401)
            .with_body(r#"{"message": "401 Unauthorized"}"#)
            .create();
        let mut command = repo.cutwater();
        let assert = command
            .args(["mr", "--dry-run"])
            .env("CI_SERVER_URL", format!("{}{prefix}", server.url()))
            .env("CI_PROJECT_ID", "7")
            .env("CUTWATER_GITLAB_TOKEN", TOKEN)
            .assert()
            .code(2)
            .stderr(contains("refuses the token in CUTWATER_GITLAB_TOKEN"));
        let output = assert.get_output();
        for stream in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(stream).contains(TOKEN));
        }
    }
}

#[test]
fn only_pipelines_of_the_default_branch() {
    let repo = package();
    repo.cutwater()
        .arg("mr")
        .env("GITLAB_CI", "true")
        .env("CI_COMMIT_BRANCH", "feature")
        .assert()
        .code(2)
        .stderr(contains(
            "`mr` runs in pipelines of the default branch `main`, but this pipeline is of \
             `feature`",
        ));
}
