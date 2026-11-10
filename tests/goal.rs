//! `goal` in the release merge request's pipeline. These tests run pnpm,
//! npm and cargo, which must be on the `PATH`.

mod common;

use common::Repo;
use predicates::prelude::*;
use predicates::str::contains;

fn changeset(releases: &[(&str, &str)], entry: &str) -> String {
    let mut text = String::from("---\n");
    for (unit, bump) in releases {
        text.push_str(&format!("\"{unit}\": {bump}\n"));
    }
    text.push_str(&format!("---\n\n{entry}\n"));
    text
}

/// The branch `cutwater/release` of an installed pnpm workspace whose
/// `main` has a major changeset for `@acme/core`, which `@acme/api` depends
/// on through `workspace:^`, holding the applied release; `@acme/web` is not
/// released.
fn pnpm_release(config: &str) -> Repo {
    let repo = Repo::with(
        &format!("[npm]\ndiscover = true\n{config}"),
        &[
            (".gitignore", "node_modules/\n"),
            (
                ".cutwater/versions.toml",
                "[units]\n\"@acme/api\" = \"1.4.2\"\n\"@acme/core\" = \"1.2.0\"\n\"@acme/web\" = \"0.3.0\"\n",
            ),
            ("package.json", r#"{"name": "acme", "private": true}"#),
            ("pnpm-workspace.yaml", "packages:\n  - packages/*\n"),
            (
                "packages/core/package.json",
                r#"{"name": "@acme/core", "version": "1.2.0"}"#,
            ),
            (
                "packages/api/package.json",
                r#"{"name": "@acme/api", "version": "1.4.2", "dependencies": {"@acme/core": "workspace:^"}}"#,
            ),
            (
                "packages/web/package.json",
                r#"{"name": "@acme/web", "version": "0.3.0"}"#,
            ),
            (
                ".changeset/brave-lions-dance.md",
                &changeset(&[("@acme/core", "major")], "Drop the old API."),
            ),
        ],
    );
    repo.run("pnpm", &["install", "--offline"]);
    repo.commit("install");
    repo.git(&["checkout", "--quiet", "-b", "cutwater/release"]);
    repo.cutwater().arg("version").assert().success();
    repo.commit("Release");
    repo
}

#[test]
fn release_merge_request_passes() {
    let repo = pnpm_release("");
    repo.cutwater()
        .args(["goal", "--base", "main"])
        .assert()
        .success()
        .stdout(contains(
            "This merge request releases:\n  @acme/core 1.2.0 -> 2.0.0\n  \
             @acme/api 1.4.2 -> 1.4.3\n",
        ))
        .stdout(contains(
            "The npm packages of the release pass the npm adapter's checks.",
        ));
}

#[test]
fn pending_changeset_for_a_released_unit_fails() {
    let repo = pnpm_release("");
    repo.write(
        ".changeset/quiet-owls-rest.md",
        &changeset(&[("@acme/web", "minor")], "Add a page."),
    );
    repo.commit("a changeset for another unit");
    repo.cutwater()
        .args(["goal", "--base", "main"])
        .assert()
        .success();

    repo.write(
        ".changeset/tidy-cats-nap.md",
        &changeset(
            &[("@acme/api", "patch"), ("@acme/web", "patch")],
            "Fix the export.",
        ),
    );
    repo.write(
        ".changeset/calm-dogs-sit.md",
        &changeset(&[("@acme/core", "none")], "Only tests."),
    );
    repo.commit("a changeset for a released unit");
    repo.cutwater()
        .args(["goal", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "these pending changesets name units this merge request releases",
        ))
        .stderr(contains("  .changeset/tidy-cats-nap.md: @acme/api\n"))
        .stderr(contains("quiet-owls-rest").not())
        .stderr(contains("calm-dogs-sit").not());
}

#[test]
fn manifest_and_files_must_agree() {
    let repo = pnpm_release("");
    repo.write(
        "packages/web/package.json",
        r#"{"name": "@acme/web", "version": "0.4.0"}"#,
    );
    repo.commit("a manual version");
    repo.cutwater()
        .args(["goal", "--base", "main"])
        .assert()
        .code(1)
        .stderr(contains(
            "  @acme/web: .cutwater/versions.toml has 0.3.0, packages/web/package.json `version` \
             has 0.4.0",
        ));
}

#[test]
fn packed_references_are_checked() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\napi = \"1.4.3\"\ncore = \"2.0.0\"\n",
            ),
            (
                "package.json",
                r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/core/package.json",
                r#"{"name": "core", "version": "2.0.0"}"#,
            ),
            (
                "packages/api/package.json",
                r#"{"name": "api", "version": "1.4.3", "dependencies": {"core": "^1.2.0"}, "peerDependencies": {"core": "workspace:^"}}"#,
            ),
        ],
    );
    let base = repo.git(&["rev-parse", "HEAD"]);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\napi = \"1.4.4\"\ncore = \"2.0.0\"\n",
    );
    repo.write(
        "packages/api/package.json",
        r#"{"name": "api", "version": "1.4.4", "dependencies": {"core": "^1.2.0"}, "peerDependencies": {"core": "workspace:^"}}"#,
    );
    repo.commit("Release");
    repo.cutwater()
        .args(["goal", "--base", &base])
        .assert()
        .code(1)
        .stderr(contains(
            "what the npm adapter would publish fails its checks:",
        ))
        .stderr(contains(
            "  api: the packed package.json has `dependencies.core` as `^1.2.0`, which does \
             not accept 2.0.0, the released version of `core`",
        ))
        .stderr(contains(
            "  api: the packed package.json still has `peerDependencies.core` as \
             `workspace:^`, which only the workspace resolves",
        ));
}

/// A Cargo workspace whose `engine` released 2.0.0 and whose `api`, which
/// depends on it, released 1.0.1, committed on top of the release's target.
fn cargo_release(api_lib: &str) -> (Repo, String) {
    let api = |version: &str, requirement: &str| {
        format!(
            "[package]\nname = \"api\"\nversion = \"{version}\"\nedition = \"2024\"\n\
             include = [\"src/**\", \"Cargo.toml\"]\n\n[dependencies]\n\
             engine = {{ path = \"../engine\", version = \"{requirement}\" }}\n"
        )
    };
    let engine = |version: &str| {
        format!("[package]\nname = \"engine\"\nversion = \"{version}\"\nedition = \"2024\"\n")
    };
    let repo = Repo::with(
        "[cargo]\ndiscover = true\n",
        &[
            (".gitignore", "target/\n"),
            (
                ".cutwater/versions.toml",
                "[units]\napi = \"1.0.0\"\nengine = \"1.2.0\"\n",
            ),
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n",
            ),
            ("crates/engine/Cargo.toml", &engine("1.2.0")),
            ("crates/engine/src/lib.rs", "pub fn engine() {}\n"),
            ("crates/api/Cargo.toml", &api("1.0.0", "1.2.0")),
            ("crates/api/src/lib.rs", api_lib),
            ("crates/api/data.txt", "data\n"),
        ],
    );
    repo.run("cargo", &["generate-lockfile", "--offline"]);
    let base = repo.commit("lockfile");
    repo.write(
        ".cutwater/versions.toml",
        "[units]\napi = \"1.0.1\"\nengine = \"2.0.0\"\n",
    );
    repo.write("crates/engine/Cargo.toml", &engine("2.0.0"));
    repo.write("crates/api/Cargo.toml", &api("1.0.1", "2.0.0"));
    repo.run("cargo", &["update", "--workspace", "--offline"]);
    repo.commit("Release");
    (repo, base)
}

#[test]
fn cargo_packages_the_released_crates_together() {
    let (repo, base) = cargo_release("pub use engine::engine;\n");
    repo.cutwater()
        .args(["goal", "--base", &base])
        .assert()
        .success()
        .stdout(contains(
            "This merge request releases:\n  engine 1.2.0 -> 2.0.0\n  api 1.0.0 -> 1.0.1\n",
        ))
        .stdout(contains(
            "The cargo packages of the release pass the cargo adapter's checks.",
        ));
}

#[test]
fn cargo_package_failure_fails() {
    let (repo, base) = cargo_release("pub const DATA: &str = include_str!(\"../data.txt\");\n");
    repo.cutwater()
        .args(["goal", "--base", &base])
        .assert()
        .code(1)
        .stderr(contains(
            "what the cargo adapter would publish fails its checks:",
        ))
        .stderr(contains("engine, api: `cargo package` failed:"))
        .stderr(contains("data.txt"));
}

#[test]
fn repository_checks_run_after_the_adapters() {
    let input = tempfile::tempdir().unwrap();
    let input_file = input.path().join("input.json");
    let config = format!(
        "\n[[check]]\nname = \"exports\"\ncommand = [\"sh\", \"-c\", \"cat > \\\"$0\\\"; echo 'api exports the removed module ./legacy'; exit 3\", \"{}\"]\nsteps = [\"goal\"]\n\n[[check]]\nname = \"only-in-check\"\ncommand = [\"false\"]\nsteps = [\"check\"]\n",
        input_file.display()
    );
    let repo = pnpm_release(&config);
    repo.cutwater()
        .args(["goal", "--base", "main"])
        .assert()
        .code(1)
        .stdout(contains(
            "Repository check `exports`:\napi exports the removed module ./legacy\n",
        ))
        .stdout(contains("only-in-check").not())
        .stderr(contains(
            "error: repository check `exports` failed (exit status 3)",
        ));
    let input: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&input_file).unwrap()).unwrap();
    assert_eq!(input["step"], "goal");
    assert_eq!(
        input["plan"]["releases"],
        serde_json::json!([
            {"unit": "@acme/core", "adapter": "npm", "current": "1.2.0", "next": "2.0.0"},
            {"unit": "@acme/api", "adapter": "npm", "current": "1.4.2", "next": "1.4.3"},
        ])
    );
    assert_eq!(
        input["units"][2],
        serde_json::json!({
            "name": "@acme/web", "path": "packages/web", "adapter": "npm", "group": null,
            "released": "0.3.0", "declared": "0.3.0",
        })
    );
}
