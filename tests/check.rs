mod common;

use common::Repo;
use predicates::prelude::*;
use predicates::str::contains;

const CONFIG: &str = r#"
[npm]
discover = true

[[unit]]
name = "@acme/api"
shipped = ["src/**", "package.json"]

[[group]]
name = "frontend"
units = ["@acme/web", "@acme/ui"]
"#;

/// A pnpm workspace on `main` with the units `@acme/api`, `@acme/web` and
/// `@acme/ui`, checked out on the branch `feature`.
fn workspace() -> Repo {
    let repo = Repo::with(
        CONFIG,
        &[
            ("package.json", r#"{"name": "acme", "private": true}"#),
            ("pnpm-workspace.yaml", "packages:\n  - packages/*\n"),
            (
                "packages/api/package.json",
                r#"{"name": "@acme/api", "version": "1.4.2"}"#,
            ),
            ("packages/api/src/index.ts", "export {};\n"),
            ("packages/api/README.md", "# api\n"),
            (
                "packages/web/package.json",
                r#"{"name": "@acme/web", "version": "2.1.0"}"#,
            ),
            ("packages/web/index.ts", "export {};\n"),
            (
                "packages/ui/package.json",
                r#"{"name": "@acme/ui", "version": "2.1.0"}"#,
            ),
            ("packages/ui/index.ts", "export {};\n"),
            (
                "packages/scripts/package.json",
                r#"{"name": "@acme/scripts", "private": true}"#,
            ),
            ("packages/scripts/run.ts", "export {};\n"),
        ],
    );
    repo.git(&["checkout", "--quiet", "-b", "feature"]);
    repo
}

fn changeset(releases: &[(&str, &str)], entry: &str) -> String {
    let mut text = String::from("---\n");
    for (unit, bump) in releases {
        text.push_str(&format!("\"{unit}\": {bump}\n"));
    }
    text.push_str(&format!("---\n\n{entry}\n"));
    text
}

#[test]
fn change_without_a_changeset_fails() {
    let repo = workspace();
    let base = repo.git(&["rev-parse", "main"]);
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.write("packages/api/src/new.ts", "export {};\n");
    repo.commit("change api");
    repo.cutwater()
        .arg("check")
        .env("CI_MERGE_REQUEST_DIFF_BASE_SHA", &base)
        .assert()
        .code(1)
        .stderr(contains("@acme/api (packages/api):"))
        .stderr(contains(
            "    packages/api/src/index.ts\n    packages/api/src/new.ts\n",
        ))
        .stderr(contains("cutwater add").and(contains("cutwater add --empty")))
        .stderr(contains("@acme/web").not());
}

#[test]
fn only_committed_changes_and_changesets_count() {
    let repo = workspace();
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.commit("change api");
    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("@acme/api", "patch")], "Fix the export."),
    );
    repo.write("packages/web/index.ts", "export const uncommitted = 1;\n");
    repo.cutwater_in(&repo.path().join("packages/api"))
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains("@acme/api (packages/api):"))
        .stderr(contains("@acme/web").not());
    repo.git(&["add", ".changeset"]);
    repo.git(&["commit", "--quiet", "--message", "add the changeset"]);
    repo.cutwater_in(&repo.path().join("packages/api"))
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains("@acme/api: .changeset/brave-lions-dance.md"));
}

#[test]
fn files_outside_the_shipped_patterns_need_no_changeset() {
    let repo = workspace();
    repo.write("packages/api/README.md", "# api, documented\n");
    repo.write("packages/scripts/run.ts", "export const run = 1;\n");
    repo.write("pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
    repo.commit("docs and tooling");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains("No unit's shipped files changed."));
}

#[test]
fn empty_changeset_covers_every_unit() {
    let repo = workspace();
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.write("packages/web/index.ts", "export const w = 1;\n");
    repo.write(
        ".changeset/quiet-owls-rest.md",
        "---\n---\n\nRefactoring only.\n",
    );
    repo.commit("refactor");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains("@acme/api: .changeset/quiet-owls-rest.md (empty)"))
        .stdout(contains("@acme/web: .changeset/quiet-owls-rest.md (empty)"));
}

#[test]
fn changeset_for_a_group_member_covers_the_group() {
    let repo = workspace();
    repo.write("packages/web/index.ts", "export const w = 1;\n");
    repo.write(
        ".changeset/green-ui.md",
        &changeset(&[("@acme/ui", "minor")], "New button."),
    );
    repo.commit("web and ui");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains("@acme/web: .changeset/green-ui.md"));
}

#[test]
fn edits_and_deletions_of_pending_changesets_are_listed() {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("@acme/api", "major")], "Rework the API."),
    );
    repo.write(
        ".changeset/old-news.md",
        &changeset(&[("@acme/web", "patch")], "A fix that was reverted."),
    );
    repo.write(
        ".changeset/typo.md",
        &changeset(&[("@acme/ui", "patch")], "Fix a tpyo."),
    );
    repo.commit("pending changesets of earlier merge requests");
    repo.git(&["checkout", "--quiet", "-B", "feature"]);

    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("@acme/api", "minor")], "Rework the API."),
    );
    repo.remove(".changeset/old-news.md");
    repo.write(
        ".changeset/typo.md",
        &changeset(&[("@acme/ui", "patch")], "Fix a typo."),
    );
    repo.commit("correct the pending changesets");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains(
            "  .changeset/brave-lions-dance.md: edited, @acme/api major -> minor\n",
        ))
        .stdout(contains(
            "  .changeset/old-news.md: deleted, @acme/web patch\n",
        ))
        .stdout(contains(
            "  .changeset/typo.md: edited, same bumps (@acme/ui patch)\n",
        ));
}

#[test]
fn comparison_starts_at_the_merge_base() {
    let repo = workspace();
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.write(
        ".changeset/api.md",
        &changeset(&[("@acme/api", "patch")], "Fix."),
    );
    repo.commit("change api");
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write("packages/web/index.ts", "export const w = 2;\n");
    repo.commit("a later change on main");
    repo.git(&["checkout", "--quiet", "feature"]);
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains("@acme/web").not());
}

fn release_branch_change() -> Repo {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "-b", "cutwater/release"]);
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.commit("change api");
    repo
}

fn check_as_merge_request(
    repo: &Repo,
    source_project: &str,
    target_project: &str,
    project: &str,
) -> assert_cmd::assert::Assert {
    repo.cutwater()
        .args(["check", "--base", "main"])
        .env("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", "cutwater/release")
        .env("CI_MERGE_REQUEST_SOURCE_PROJECT_ID", source_project)
        .env("CI_MERGE_REQUEST_PROJECT_ID", target_project)
        .env("CI_PROJECT_ID", project)
        .assert()
}

#[test]
fn release_merge_request_is_exempt() {
    let repo = release_branch_change();
    check_as_merge_request(&repo, "7", "7", "7")
        .success()
        .stdout(contains("release merge request"));
}

#[test]
fn component_with_another_release_branch_is_refused() {
    let repo = release_branch_change();
    // The component would run `check`, not `goal`, in the release merge
    // request's pipeline.
    repo.cutwater()
        .args(["check", "--base", "main"])
        .env("CUTWATER_RELEASE_BRANCH", "release")
        .assert()
        .code(2)
        .stderr(
            contains("`release-branch` input is `release`")
                .and(contains("`release-branch` to `cutwater/release`")),
        );
}

#[test]
fn fork_branch_named_like_the_release_branch_is_not_exempt() {
    let repo = release_branch_change();
    // The pipeline in the target project, and the one in the fork.
    for project in ["7", "9"] {
        check_as_merge_request(&repo, "9", "7", project)
            .code(1)
            .stderr(contains("@acme/api (packages/api):"));
    }
}

#[test]
fn shallow_clone_without_the_diff_base() {
    let origin = workspace();
    let base = origin.git(&["rev-parse", "main"]);
    origin.write("packages/api/src/index.ts", "export const a = 1;\n");
    origin.commit("first");
    origin.write("packages/api/src/index.ts", "export const a = 2;\n");
    origin.commit("second");

    let clone = Repo::clone_of(&origin, &["--depth", "1", "--branch", "feature"]);
    clone
        .cutwater()
        .arg("check")
        .env("CI_MERGE_REQUEST_DIFF_BASE_SHA", &base)
        .assert()
        .code(2)
        .stderr(contains(format!("this clone lacks the commit `{base}`")))
        .stderr(contains("Raise the clone depth"));
}

#[test]
fn base_that_is_no_revision() {
    let repo = workspace();
    repo.cutwater()
        .args(["check", "--base", "no-such-branch"])
        .assert()
        .code(2)
        .stderr(contains(
            "`no-such-branch` (--base) names no commit of this repository",
        ))
        .stderr(contains("clone depth").not());
}

#[test]
fn no_diff_base() {
    let repo = workspace();
    repo.cutwater()
        .arg("check")
        .assert()
        .code(2)
        .stderr(contains("CI_MERGE_REQUEST_DIFF_BASE_SHA").and(contains("--base")));
}

#[test]
fn unknown_units_and_bumps_name_file_unit_and_value() {
    let repo = workspace();
    repo.write(
        ".changeset/odd.md",
        &changeset(&[("@acme/apy", "minor"), ("@acme/web", "huge")], "Odd."),
    );
    repo.commit("odd changeset");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(2)
        .stderr(contains(
            ".changeset/odd.md: names `@acme/apy` with `minor`, but `@acme/apy` is not a unit",
        ))
        .stderr(contains(
            ".changeset/odd.md: unit `@acme/web`: unknown bump `huge`",
        ));
}

#[test]
fn readme_and_config_json_are_not_changesets() {
    let repo = workspace();
    repo.write(
        ".changeset/README.md",
        "# Changesets\n\nNo front matter here.\n",
    );
    repo.write(".changeset/config.json", "{\"fixed\": []}\n");
    repo.commit("Changesets files");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success();
}

#[test]
fn missing_configuration() {
    let repo = Repo::new();
    repo.write("package.json", r#"{"name": "tool"}"#);
    repo.commit("initial");
    repo.cutwater()
        .args(["check", "--base", "HEAD"])
        .assert()
        .code(2)
        .stderr(contains(".cutwater/config.toml is missing").and(contains("cutwater init")));
}

#[test]
fn unknown_setting() {
    let repo = Repo::with("[npm]\ndiscover = true\nworkspace = \"pnpm\"\n", &[]);
    repo.cutwater()
        .args(["check", "--base", "HEAD"])
        .assert()
        .code(2)
        .stderr(contains("line 3").and(contains("unknown field `workspace`")));
}

#[test]
fn change_to_a_nested_unit_ships_only_that_unit() {
    // Discovered units, and a root unit whose patterns would match the
    // member's files.
    let listed_root = "[[unit]]\nname = \"app\"\nshipped = [\"src\", \"Cargo.toml\"]\n";
    for extra in ["", listed_root] {
        let repo = Repo::with(
            &format!("[cargo]\ndiscover = true\n{extra}"),
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\
                     [workspace]\nmembers = [\"crates/core\"]\n",
                ),
                ("src/main.rs", "fn main() {}\n"),
                (
                    "crates/core/Cargo.toml",
                    "[package]\nname = \"core\"\nversion = \"1.0.0\"\n",
                ),
                ("crates/core/src/lib.rs", "\n"),
            ],
        );
        repo.git(&["checkout", "--quiet", "-b", "feature"]);
        repo.write("crates/core/src/lib.rs", "pub fn core() {}\n");
        repo.commit("change core");
        repo.cutwater()
            .args(["check", "--base", "main"])
            .assert()
            .code(1)
            .stderr(contains(
                "  core (crates/core):\n    crates/core/src/lib.rs\n",
            ))
            .stderr(contains("app (.)").not());

        repo.write(
            ".changeset/core.md",
            &changeset(&[("core", "patch")], "Add core()."),
        );
        repo.commit("changeset for core");
        repo.cutwater()
            .args(["check", "--base", "main"])
            .assert()
            .success()
            .stdout(contains("core: .changeset/core.md"))
            .stdout(contains("app:").not());
    }
}

/// `main` gains a release commit after `feature` branched off: it changes
/// `@acme/web`, deletes an applied changeset and adds a changeset for
/// `@acme/api`. HEAD is the merged result of `feature` on that commit.
fn merged_result(feature_changeset: bool) -> (Repo, String, String) {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write(
        ".changeset/applied.md",
        &changeset(&[("@acme/web", "minor")], "Released."),
    );
    let diff_base = repo.commit("a pending changeset");
    repo.git(&["checkout", "--quiet", "-B", "feature"]);
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    if feature_changeset {
        repo.write(
            ".changeset/api.md",
            &changeset(&[("@acme/api", "patch")], "Fix the export."),
        );
    }
    repo.commit("change api");
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write("packages/web/index.ts", "export const released = 1;\n");
    repo.remove(".changeset/applied.md");
    repo.write(
        ".changeset/later.md",
        &changeset(&[("@acme/api", "minor")], "Another merge request."),
    );
    let target = repo.commit("release");
    repo.git(&["checkout", "--quiet", "--detach", &target]);
    repo.git(&["merge", "--quiet", "--no-ff", "--no-edit", "feature"]);
    (repo, diff_base, target)
}

fn check_merged_result(repo: &Repo, diff_base: &str, target: &str) -> assert_cmd::assert::Assert {
    repo.cutwater()
        .arg("check")
        .env("CI_MERGE_REQUEST_EVENT_TYPE", "merged_result")
        .env("CI_MERGE_REQUEST_DIFF_BASE_SHA", diff_base)
        .env("CI_MERGE_REQUEST_TARGET_BRANCH_SHA", target)
        .assert()
}

#[test]
fn merged_results_pipeline_compares_with_the_merged_target_commit() {
    let (repo, diff_base, target) = merged_result(true);
    check_merged_result(&repo, &diff_base, &target)
        .success()
        .stdout(contains(format!("Comparing HEAD with {target}")))
        .stdout(contains("@acme/api: .changeset/api.md"))
        .stdout(contains("@acme/web").not())
        .stdout(contains("applied.md").not())
        .stdout(contains("later.md").not());
}

#[test]
fn changesets_the_target_branch_gained_do_not_cover_a_merged_result() {
    let (repo, diff_base, target) = merged_result(false);
    check_merged_result(&repo, &diff_base, &target)
        .code(1)
        .stderr(contains(
            "@acme/api (packages/api):\n    packages/api/src/index.ts\n",
        ))
        .stderr(contains("@acme/web").not());
}

#[test]
fn renamed_file_changes_both_units() {
    let repo = workspace();
    repo.git(&["mv", "packages/api/src/index.ts", "packages/web/api.ts"]);
    repo.commit("move");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "@acme/api (packages/api):\n    packages/api/src/index.ts\n",
        ))
        .stderr(contains(
            "@acme/web (packages/web):\n    packages/web/api.ts\n",
        ));
}

#[test]
fn deleted_shipped_file_needs_a_changeset() {
    let repo = workspace();
    repo.remove("packages/api/src/index.ts");
    repo.commit("delete");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "@acme/api (packages/api):\n    packages/api/src/index.ts\n",
        ));
}

/// `main` with a pending changeset of an earlier merge request for
/// `@acme/web`, and `feature` from it.
fn pending_web_changeset() -> Repo {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write(
        ".changeset/web.md",
        &changeset(&[("@acme/web", "minor")], "New page."),
    );
    repo.commit("pending changeset");
    repo.git(&["checkout", "--quiet", "-B", "feature"]);
    repo
}

#[test]
fn edited_changeset_made_empty_covers_nothing() {
    let repo = pending_web_changeset();
    repo.write(".changeset/web.md", "---\n---\n");
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.commit("empty the changeset");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stdout(contains(
            ".changeset/web.md: edited, @acme/web minor -> (not named)",
        ))
        .stderr(contains("@acme/api (packages/api):"));
}

#[test]
fn edited_changeset_of_another_merge_request_covers_what_it_names() {
    let repo = pending_web_changeset();
    repo.write(
        ".changeset/web.md",
        &changeset(
            &[("@acme/web", "minor"), ("@acme/api", "patch")],
            "New page.",
        ),
    );
    repo.write("packages/api/src/index.ts", "export const a = 1;\n");
    repo.commit("name api too");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success()
        .stdout(contains(
            ".changeset/web.md: edited, @acme/api (not named) -> patch",
        ))
        .stdout(contains("@acme/api: .changeset/web.md"));
}

#[test]
fn diff_base_without_a_merge_base() {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "--orphan", "unrelated"]);
    let unrelated = repo.commit("unrelated history");
    repo.git(&["checkout", "--quiet", "feature"]);
    repo.cutwater()
        .arg("check")
        .env("CI_MERGE_REQUEST_DIFF_BASE_SHA", &unrelated)
        .assert()
        .code(2)
        .stderr(contains("does not connect").and(contains("Raise the clone depth")));
}

#[test]
fn version_edited_by_hand_disagrees_with_the_manifest() {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\n\"@acme/api\" = \"1.4.2\"\n\"@acme/ui\" = { version = \"2.1.0\", seeded = true }\n",
    );
    repo.commit("record the released versions");
    repo.git(&["checkout", "--quiet", "-B", "feature"]);
    repo.write(
        "packages/api/package.json",
        r#"{"name": "@acme/api", "version": "1.5.0"}"#,
    );
    repo.write(
        "packages/web/package.json",
        r#"{"name": "@acme/web", "version": "2.2.0"}"#,
    );
    repo.write(
        ".changeset/bump.md",
        &changeset(&[("@acme/api", "minor"), ("@acme/web", "minor")], "Bump."),
    );
    repo.commit("bump the versions by hand");
    let disagreement = "  @acme/api: .cutwater/versions.toml has 1.4.2, \
                        packages/api/package.json `version` has 1.5.0\n";
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(disagreement))
        // A unit without a manifest entry is new.
        .stderr(contains("@acme/web:").not())
        .stdout(contains("@acme/api: .changeset/bump.md"));
    // The release merge request is checked the same way.
    repo.git(&["checkout", "--quiet", "-b", "cutwater/release"]);
    check_as_merge_request(&repo, "7", "7", "7")
        .code(1)
        .stderr(contains(disagreement));
}

const MANIFEST: &str =
    "[units]\n\"@acme/api\" = \"1.4.2\"\n\"@acme/ui\" = \"2.1.0\"\n\"@acme/web\" = \"2.1.0\"\n";

#[test]
fn creating_the_manifest_passes_and_changing_it_fails() {
    let repo = workspace();
    repo.write(".cutwater/versions.toml", MANIFEST);
    repo.commit("adopt cutwater");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.git(&["merge", "--quiet", "--ff-only", "feature"]);
    repo.git(&["checkout", "--quiet", "-B", "feature"]);
    repo.write(
        ".cutwater/versions.toml",
        &MANIFEST.replace("1.4.2", "1.4.3"),
    );
    repo.write(
        "packages/api/package.json",
        r#"{"name": "@acme/api", "version": "1.4.3"}"#,
    );
    repo.commit("release by hand");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "this merge request changes .cutwater/versions.toml, but only `cutwater version` writes it",
        ));
}

#[test]
fn cycle_introduced_by_a_merge_request() {
    let repo = workspace();
    repo.write(
        "packages/api/package.json",
        r#"{"name": "@acme/api", "version": "1.4.2", "dependencies": {"@acme/web": "^2.1.0"}}"#,
    );
    repo.write(
        "packages/web/package.json",
        r#"{"name": "@acme/web", "version": "2.1.0", "dependencies": {"@acme/api": "^1.4.2"}}"#,
    );
    repo.write(
        ".changeset/loop.md",
        &changeset(&[("@acme/api", "patch"), ("@acme/web", "patch")], "Loop."),
    );
    repo.commit("depend on each other");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains("this tree cannot be planned"))
        .stderr(contains(
            "  @acme/api -> @acme/web (runtime, packages/api/package.json)\n  \
             @acme/web -> @acme/api (runtime, packages/web/package.json)\n",
        ));
}

#[test]
fn release_merge_request_passes_after_version() {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write(".cutwater/versions.toml", MANIFEST);
    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("@acme/api", "minor"), ("@acme/ui", "patch")], "Add."),
    );
    repo.commit("adopt cutwater, with a pending changeset");
    repo.git(&["checkout", "--quiet", "-b", "cutwater/release"]);
    repo.cutwater().arg("version").assert().success();
    repo.commit("release");
    check_as_merge_request(&repo, "7", "7", "7")
        .success()
        .stderr("");
}

#[test]
fn revert_of_a_release_merge_request_fails() {
    let repo = workspace();
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write(".cutwater/versions.toml", MANIFEST);
    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("@acme/api", "minor")], "Add."),
    );
    repo.commit("adopt cutwater, with a pending changeset");
    repo.cutwater().arg("version").assert().success();
    let release = repo.commit("release");
    repo.git(&["checkout", "--quiet", "-B", "feature"]);
    repo.git(&["revert", "--no-edit", &release]);
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "this merge request changes .cutwater/versions.toml, but only `cutwater version` writes it",
        ));
}

#[test]
fn repository_checks_receive_the_units_and_the_plan() {
    let input = tempfile::tempdir().unwrap();
    let input_file = input.path().join("input.json");
    let config = format!(
        "[npm]\ndiscover = true\n\n[[check]]\nname = \"record\"\ncommand = [\"sh\", \"-c\", \"cat > \\\"$0\\\"; echo 'no findings'\", \"{}\"]\nsteps = [\"check\", \"goal\"]\n\n[[check]]\nname = \"exports\"\ncommand = [\"sh\", \"-c\", \"echo 'api exports the removed module ./legacy'; exit 1\"]\nsteps = [\"check\"]\n",
        input_file.display()
    );
    let repo = Repo::with(
        &config,
        &[
            (".cutwater/versions.toml", "[units]\napi = \"1.4.2\"\n"),
            ("package.json", r#"{"name": "api", "version": "1.4.2"}"#),
        ],
    );
    repo.git(&["checkout", "--quiet", "-b", "feature"]);
    repo.write("index.js", "export {};\n");
    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("api", "minor")], "Add the export endpoint."),
    );
    repo.commit("change api");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stdout(contains(
            "Repository check `record`:\nno findings\nRepository check `record` passed.\n",
        ))
        .stdout(contains(
            "Repository check `exports`:\napi exports the removed module ./legacy\n",
        ))
        .stderr(contains(
            "error: repository check `exports` failed (exit status 1)",
        ));
    let input: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&input_file).unwrap()).unwrap();
    assert_eq!(
        input,
        serde_json::json!({
            "step": "check",
            "units": [{
                "name": "api", "path": ".", "adapter": "npm", "group": null,
                "released": "1.4.2", "declared": "1.4.2",
            }],
            "plan": {"releases": [
                {"unit": "api", "adapter": "npm", "current": "1.4.2", "next": "1.5.0"},
            ]},
        })
    );
}

/// Maven units on `main`, released as `billing` 1.3.0, `ledger` 2.1.0 and
/// `orders` 0.4.0, with the snapshots of their next patches, checked out on
/// the branch `feature`.
fn maven_units(units: &common::maven::Units) -> Repo {
    let repo = Repo::with(
        "[maven]\ndiscover = true\n",
        &[(
            ".cutwater/versions.toml",
            "[units]\nbilling = \"1.3.0\"\nledger = \"2.1.0\"\norders = \"0.4.0\"\n",
        )],
    );
    units.write(&repo);
    repo.commit("maven units");
    repo.git(&["checkout", "--quiet", "-b", "feature"]);
    repo
}

#[test]
fn maven_producer_property_out_of_step() {
    let units = common::maven::Units::default();
    let repo = maven_units(&units);
    common::maven::Units {
        billing_property: "1.3.0",
        ..units
    }
    .write(&repo);
    repo.write(
        ".changeset/older-billing.md",
        &changeset(
            &[("orders", "patch")],
            "Build against the released billing.",
        ),
    );
    repo.commit("pin billing");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "orders/pom.xml: the property `billing.version` is `1.3.0`, but it refers to unit \
             `billing`, whose revision is `1.3.1-SNAPSHOT`",
        ));
}

#[test]
fn maven_snapshot_below_the_released_version() {
    let units = common::maven::Units::default();
    let repo = maven_units(&units);
    common::maven::Units {
        ledger: "2.1.0-SNAPSHOT",
        ..units
    }
    .write(&repo);
    repo.write(
        ".changeset/ledger.md",
        &changeset(&[("ledger", "patch")], "Go back."),
    );
    repo.commit("ledger snapshot");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "do not lead to a version above the one .cutwater/versions.toml records:\n  \
             ledger: .cutwater/versions.toml has 2.1.0, ledger/pom.xml `properties.revision` has \
             2.1.0-SNAPSHOT",
        ));
}

#[test]
fn maven_revision_that_is_no_snapshot_of_a_semantic_version() {
    let units = common::maven::Units::default();
    let repo = maven_units(&units);
    common::maven::Units {
        billing: "1.0-SNAPSHOT",
        ..units
    }
    .write(&repo);
    repo.commit("two-part version");
    for args in [&["check", "--base", "main"][..], &["status"]] {
        repo.cutwater().args(args).assert().code(2).stderr(contains(
            "billing/pom.xml: unit `billing` sets `revision` to `1.0-SNAPSHOT`, but a Maven \
                 unit's revision is a semantic version followed by `-SNAPSHOT`",
        ));
    }
}
