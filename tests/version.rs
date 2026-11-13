//! `version` applies the plan to the working tree. These tests run pnpm,
//! npm and cargo, which must be on the `PATH`.

mod common;

use common::Repo;
use predicates::str::contains;

fn changeset(releases: &[(&str, &str)], entry: &str) -> String {
    let mut text = String::from("---\n");
    for (unit, bump) in releases {
        text.push_str(&format!("\"{unit}\": {bump}\n"));
    }
    text.push_str(&format!("---\n\n{entry}\n"));
    text
}

/// A pnpm workspace: `@acme/api` depends on `@acme/core` through
/// `workspace:^`, and the private `@acme/app` on both, through the default
/// catalog and `workspace:*`; with its lockfile, committed.
fn pnpm_workspace() -> Repo {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\n\"@acme/api\" = \"1.4.2\"\n\"@acme/core\" = \"1.2.0\"\n",
            ),
            ("package.json", r#"{"name": "acme", "private": true}"#),
            (
                "pnpm-workspace.yaml",
                "packages:\n  - packages/*\n\n# Link the packages of the workspace.\nlinkWorkspacePackages: true\n\ncatalog:\n  \"@acme/core\": ^1.2.0\n",
            ),
            (
                "packages/core/package.json",
                "{\n  \"name\": \"@acme/core\",\n  \"version\": \"1.2.0\",\n  \"files\": [\"dist\"]\n}\n",
            ),
            (
                "packages/api/package.json",
                "{\n    \"name\": \"@acme/api\",\n    \"version\": \"1.4.2\",\n    \"dependencies\": {\n        \"@acme/core\": \"workspace:^\"\n    }\n}\n",
            ),
            (
                "packages/app/package.json",
                r#"{"name": "@acme/app", "private": true, "dependencies": {"@acme/core": "catalog:", "@acme/api": "workspace:*"}}"#,
            ),
        ],
    );
    repo.run(
        "pnpm",
        &["install", "--lockfile-only", "--no-frozen-lockfile"],
    );
    repo.commit("lockfile");
    repo
}

fn pending_release(repo: &Repo) {
    repo.write(
        ".changeset/brave-lions-dance.md",
        &changeset(&[("@acme/core", "major")], "Drop the old API."),
    );
    repo.write(
        ".changeset/quiet-owls-rest.md",
        &changeset(
            &[("@acme/api", "minor")],
            "Add the export endpoint.\n\nIt streams CSV.",
        ),
    );
    repo.write(
        ".changeset/tidy-cats-nap.md",
        &changeset(&[("@acme/api", "patch")], "Fix the export of empty tables."),
    );
}

#[test]
fn pnpm_workspace_release() {
    let repo = pnpm_workspace();
    pending_release(&repo);
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains(
            "Releases, in the order of publication:\n  \
             @acme/core 1.2.0 -> 2.0.0: major (.changeset/brave-lions-dance.md)\n  \
             @acme/api 1.4.2 -> 1.5.0: minor (.changeset/quiet-owls-rest.md, \
             .changeset/tidy-cats-nap.md); @acme/core 2.0.0 is outside its runtime range ^1.2.0\n",
        ))
        .stdout(contains("Refreshed:\n  pnpm-lock.yaml"));

    assert_eq!(
        repo.read("packages/core/package.json"),
        "{\n  \"name\": \"@acme/core\",\n  \"version\": \"2.0.0\",\n  \"files\": [\"dist\"]\n}\n"
    );
    assert_eq!(
        repo.read("packages/api/package.json"),
        "{\n    \"name\": \"@acme/api\",\n    \"version\": \"1.5.0\",\n    \"dependencies\": {\n        \"@acme/core\": \"workspace:^\"\n    }\n}\n"
    );
    // `workspace:*` follows the version by itself; the catalog range does
    // not accept 2.0.0, so it is rewritten.
    assert!(
        repo.read("packages/app/package.json")
            .contains(r#""@acme/core": "catalog:", "@acme/api": "workspace:*""#)
    );
    assert_eq!(
        repo.read("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n\n# Link the packages of the workspace.\nlinkWorkspacePackages: true\n\ncatalog:\n  \"@acme/core\": ^2.0.0\n"
    );
    // pnpm records the catalog's range; it records no version of a
    // workspace package, which it links.
    let lockfile = repo.read("pnpm-lock.yaml");
    assert!(
        lockfile.contains("specifier: ^2.0.0\n") && !lockfile.contains("^1.2.0"),
        "{lockfile}"
    );
    expect_test::expect![[r#"
        # The released version of every unit. Only `cutwater init` and `cutwater version`
        # write this file.

        [units]
        "@acme/api" = "1.5.0"
        "@acme/core" = "2.0.0"
    "#]]
    .assert_eq(&repo.read(".cutwater/versions.toml"));
    expect_test::expect![[r#"
        # @acme/api

        ## 1.5.0

        ### Minor Changes

        - Add the export endpoint.

          It streams CSV.

        ### Patch Changes

        - Fix the export of empty tables.
        - Updated dependencies
          - @acme/core@2.0.0
    "#]]
    .assert_eq(&repo.read("packages/api/CHANGELOG.md"));
    expect_test::expect![[r#"
        # @acme/core

        ## 2.0.0

        ### Major Changes

        - Drop the old API.
    "#]]
    .assert_eq(&repo.read("packages/core/CHANGELOG.md"));
    assert_eq!(repo.list(".changeset"), Vec::<String>::new());
}

#[test]
fn changelog_entry_goes_above_the_earlier_ones() {
    let repo = pnpm_workspace();
    repo.write(
        "packages/core/CHANGELOG.md",
        "# @acme/core\n\n## 1.2.0\n\n### Minor Changes\n\n- Earlier.\n",
    );
    repo.write(
        ".changeset/x.md",
        &changeset(&[("@acme/core", "patch")], "Later."),
    );
    repo.cutwater().arg("version").assert().success();
    assert_eq!(
        repo.read("packages/core/CHANGELOG.md"),
        "# @acme/core\n\n## 1.2.1\n\n### Patch Changes\n\n- Later.\n\n## 1.2.0\n\n### Minor Changes\n\n- Earlier.\n"
    );
}

#[test]
fn dry_run_writes_nothing() {
    let repo = pnpm_workspace();
    pending_release(&repo);
    let before = repo.snapshot();
    repo.cutwater()
        .args(["version", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("@acme/core 1.2.0 -> 2.0.0"))
        .stdout(contains("@acme/api 1.4.2 -> 1.5.0"))
        .stdout(contains(
            "Would write:\n  .cutwater/versions.toml\n  packages/api/CHANGELOG.md\n  \
             packages/api/package.json\n  packages/core/CHANGELOG.md\n  \
             packages/core/package.json\n  pnpm-workspace.yaml\n",
        ))
        .stdout(contains(
            "Would delete:\n  .changeset/brave-lions-dance.md\n  \
             .changeset/quiet-owls-rest.md\n  .changeset/tidy-cats-nap.md\n",
        ))
        .stdout(contains(
            "Would refresh:\n  pnpm-lock.yaml (`pnpm install --lockfile-only --no-frozen-lockfile --ignore-scripts`)\n",
        ));
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn nothing_pending_leaves_the_tree_unchanged() {
    let repo = pnpm_workspace();
    let before = repo.snapshot();
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("No pending changesets"));
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn missing_tool_is_named_before_anything_is_written() {
    let repo = pnpm_workspace();
    pending_release(&repo);
    let before = repo.snapshot();
    // A PATH with git alone.
    let bin = tempfile::tempdir().unwrap();
    let git = which::which("git").unwrap();
    std::os::unix::fs::symlink(&git, bin.path().join("git")).unwrap();
    repo.cutwater()
        .arg("version")
        .env("PATH", bin.path())
        .assert()
        .code(2)
        .stderr(contains(
            "pnpm-lock.yaml: refreshing it needs pnpm, which is not on the PATH",
        ));
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn failing_tool_leaves_the_tree_as_it_was() {
    let repo = pnpm_workspace();
    pending_release(&repo);
    let before = repo.snapshot();
    let bin = tempfile::tempdir().unwrap();
    let git = which::which("git").unwrap();
    std::os::unix::fs::symlink(&git, bin.path().join("git")).unwrap();
    let pnpm = bin.path().join("pnpm");
    std::fs::write(&pnpm, "#!/bin/sh\necho 'no lockfile today' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&pnpm, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    repo.cutwater()
        .arg("version")
        .env("PATH", bin.path())
        .assert()
        .code(2)
        .stderr(contains("no lockfile today"))
        .stderr(contains("the working tree is as it was"));
    assert_eq!(repo.snapshot(), before);
    repo.cutwater()
        .arg("status")
        .assert()
        .success()
        .stdout(contains("@acme/core 1.2.0 -> 2.0.0"));
}

/// An npm workspace with its lockfile.
fn npm_workspace(units: &str) -> Repo {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (".cutwater/versions.toml", units),
            (
                "package.json",
                r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/api/package.json",
                r#"{"name": "api", "version": "1.4.2", "dependencies": {"core": "^1.0.0"}}"#,
            ),
            (
                "packages/core/package.json",
                r#"{"name": "core", "version": "1.0.0"}"#,
            ),
            (
                "packages/export/package.json",
                r#"{"name": "export", "version": "0.1.0", "dependencies": {"api": "^1.4.0"}}"#,
            ),
        ],
    );
    repo.run(
        "npm",
        &[
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
    );
    repo.commit("lockfile");
    repo
}

#[test]
fn npm_lockfile_names_the_new_version() {
    let repo = npm_workspace("[units]\napi = \"1.4.2\"\ncore = \"1.0.0\"\nexport = \"0.1.0\"\n");
    repo.write(
        ".changeset/a.md",
        &changeset(&[("api", "patch")], "Fix the export of empty tables."),
    );
    repo.write(
        ".changeset/b.md",
        &changeset(&[("api", "minor")], "Add the export endpoint."),
    );
    repo.cutwater().arg("version").assert().success();
    let lockfile: serde_json::Value =
        serde_json::from_str(&repo.read("package-lock.json")).unwrap();
    assert_eq!(lockfile["packages"]["packages/api"]["version"], "1.5.0");
    assert!(repo
        .read("packages/api/CHANGELOG.md")
        .starts_with("# api\n\n## 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.\n\n### Patch Changes\n\n- Fix the export of empty tables.\n"));
}

#[test]
fn first_release_of_a_new_package_and_removal_of_an_old_one() {
    let repo = npm_workspace("[units]\napi = \"1.4.2\"\ncore = \"1.0.0\"\nold = \"3.1.0\"\n");
    repo.write(
        ".changeset/a.md",
        &changeset(&[("export", "minor")], "Export tables as CSV."),
    );
    repo.cutwater()
        .arg("status")
        .assert()
        .success()
        .stdout(contains(
            "  export new -> 0.1.0: first release, the version its files declare (.changeset/a.md)\n",
        ))
        .stdout(contains("whose manifest entries this release removes: old"));
    repo.cutwater().arg("version").assert().success();
    assert_eq!(
        repo.read(".cutwater/versions.toml")
            .lines()
            .skip(3)
            .collect::<Vec<_>>(),
        [
            "[units]",
            "api = \"1.4.2\"",
            "core = \"1.0.0\"",
            "export = \"0.1.0\""
        ]
    );
    assert_eq!(
        repo.read("packages/export/CHANGELOG.md"),
        "# export\n\n## 0.1.0\n\n### Minor Changes\n\n- Export tables as CSV.\n"
    );
    assert!(
        repo.read("packages/export/package.json")
            .contains("\"version\": \"0.1.0\"")
    );
}

#[test]
fn removal_rides_with_the_next_release() {
    let repo = npm_workspace(
        "[units]\napi = \"1.4.2\"\ncore = \"1.0.0\"\nexport = \"0.1.0\"\nlegacy = \"3.1.0\"\n",
    );
    repo.write(
        ".changeset/a.md",
        "---\n---\n\nDelete the legacy package.\n",
    );
    let before = repo.snapshot();
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("release no unit: nothing to release"))
        .stdout(contains(
            "whose manifest entries the next release removes: legacy",
        ));
    assert_eq!(repo.snapshot(), before);
    repo.write(".changeset/b.md", &changeset(&[("api", "patch")], "Fix."));
    repo.cutwater().arg("version").assert().success();
    assert!(!repo.read(".cutwater/versions.toml").contains("legacy"));
    assert_eq!(repo.list(".changeset"), Vec::<String>::new());
}

/// A Cargo workspace whose `api` and `core` inherit its version as a fixed
/// group, and whose `cli` has a version of its own.
/// A changelog is titled with the package's name, as in Changesets, also
/// where the unit has a name of its own.
#[test]
fn changelog_is_titled_with_the_package_name() {
    let repo = Repo::with(
        "[[unit]]\nname = \"cli-npm\"\npath = \"cli\"\nadapter = \"npm\"\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\n\"cli-npm\" = \"1.0.0\"\n",
            ),
            (
                "cli/package.json",
                "{\n  \"name\": \"@acme/cli\",\n  \"version\": \"1.0.0\"\n}\n",
            ),
            (
                ".changeset/a.md",
                &changeset(&[("cli-npm", "minor")], "Add the export command."),
            ),
        ],
    );
    repo.cutwater().arg("version").assert().success();
    assert_eq!(
        repo.read("cli/CHANGELOG.md"),
        "# @acme/cli\n\n## 1.1.0\n\n### Minor Changes\n\n- Add the export command.\n"
    );
}

fn cargo_workspace() -> Repo {
    let repo = Repo::with(
        "[cargo]\ndiscover = true\n\n[[group]]\nname = \"sdk\"\nunits = [\"api\", \"core\"]\n",
        &[
            (
                ".cutwater/versions.toml",
                "[units]\napi = \"1.4.2\"\ncli = \"0.3.0\"\ncore = \"1.4.2\"\n",
            ),
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.4.2\"\nedition = \"2024\"\n\n[workspace.dependencies]\ncore = { path = \"crates/core\", version = \"1.4.2\" }\n",
            ),
            (
                "crates/core/Cargo.toml",
                "[package]\nname = \"core\"\nversion.workspace = true\nedition.workspace = true\n",
            ),
            ("crates/core/src/lib.rs", ""),
            (
                "crates/api/Cargo.toml",
                "[package]\nname = \"api\"\nversion.workspace = true\nedition.workspace = true\n\n[dependencies]\ncore.workspace = true\n",
            ),
            ("crates/api/src/lib.rs", ""),
            (
                "crates/cli/Cargo.toml",
                "[package]\nname = \"cli\"\nversion = \"0.3.0\"\nedition = \"2024\"\n\n[dependencies]\ncore = { path = \"../core\", version = \"1.4\" } # the SDK\n",
            ),
            ("crates/cli/src/main.rs", "fn main() {}\n"),
        ],
    );
    repo.run("cargo", &["generate-lockfile", "--offline"]);
    repo.commit("lockfile");
    repo
}

#[test]
fn cargo_workspace_release() {
    let repo = cargo_workspace();
    repo.write(
        ".changeset/a.md",
        &changeset(&[("core", "major")], "Drop the old API."),
    );
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains(
            "  core 1.4.2 -> 2.0.0: major (.changeset/a.md)\n  \
             api 1.4.2 -> 2.0.0: fixed group sdk\n  \
             cli 0.3.0 -> 0.3.1: core 2.0.0 is outside its runtime range 1.4\n",
        ))
        .stdout(contains(
            "Refreshed:\n  Cargo.lock (`cargo update --workspace`)",
        ));
    assert_eq!(
        repo.read("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"2.0.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\ncore = { path = \"crates/core\", version = \"2.0.0\" }\n"
    );
    assert_eq!(
        repo.read("crates/cli/Cargo.toml"),
        "[package]\nname = \"cli\"\nversion = \"0.3.1\"\nedition = \"2024\"\n\n[dependencies]\ncore = { path = \"../core\", version = \"2.0.0\" } # the SDK\n"
    );
    let lockfile = repo.read("Cargo.lock");
    for (name, version) in [("api", "2.0.0"), ("cli", "0.3.1"), ("core", "2.0.0")] {
        assert!(
            lockfile.contains(&format!("name = \"{name}\"\nversion = \"{version}\"\n")),
            "{lockfile}"
        );
    }
    assert!(
        repo.read("crates/api/CHANGELOG.md")
            .contains("## 2.0.0\n\n### Patch Changes\n\n- Updated dependencies\n  - core@2.0.0\n")
    );
}

#[test]
fn cycle_is_refused() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (
                "package.json",
                r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/a/package.json",
                r#"{"name": "a", "version": "1.0.0", "dependencies": {"b": "^1.0.0"}}"#,
            ),
            (
                "packages/b/package.json",
                r#"{"name": "b", "version": "1.0.0", "dependencies": {"a": "^1.0.0"}}"#,
            ),
        ],
    );
    for command in ["status", "version"] {
        repo.cutwater()
            .arg(command)
            .assert()
            .code(2)
            .stderr(contains(
                "  a -> b (runtime, packages/a/package.json)\n  b -> a (runtime, packages/b/package.json)\n",
            ));
    }
}

#[test]
fn yarn_workspace_is_refused() {
    let repo = npm_workspace("[units]\napi = \"1.4.2\"\ncore = \"1.0.0\"\nexport = \"0.1.0\"\n");
    repo.write("yarn.lock", "");
    repo.write(".changeset/a.md", &changeset(&[("api", "patch")], "Fix."));
    repo.cutwater()
        .args(["version", "--dry-run"])
        .assert()
        .code(2)
        .stderr(contains("yarn.lock: the workspace is managed with Yarn"));
}

#[test]
fn closed_standard_output_ends_quietly_before_anything_is_written() {
    let repo = pnpm_workspace();
    pending_release(&repo);
    let before = repo.snapshot();
    for command in ["status", "version"] {
        let mut child = repo
            .cutwater_process(repo.path())
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // The reader leaves before the tool writes, as `head` does.
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{command}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "", "{command}");
    }
    assert_eq!(repo.snapshot(), before);
}

/// Maven units released as `billing` 1.3.0, `ledger` 2.1.0 and `orders`
/// 0.4.0, or as `manifest` says, with the snapshots of `units`.
fn maven_units(units: &common::maven::Units, manifest: &str) -> Repo {
    let repo = Repo::with(
        "[maven]\ndiscover = true\n",
        &[(".cutwater/versions.toml", manifest)],
    );
    units.write(&repo);
    repo.commit("maven units");
    repo
}

const MAVEN_RELEASED: &str =
    "[units]\nbilling = \"1.3.0\"\nledger = \"2.1.0\"\norders = \"0.4.0\"\n";

/// The value of the property `name` that the POM `file` defines.
fn property(repo: &Repo, file: &str, name: &str) -> String {
    let text = repo.read(file);
    let start = text.find(&format!("<{name}>")).unwrap() + name.len() + 2;
    let end = start + text[start..].find('<').unwrap();
    text[start..end].to_string()
}

#[test]
fn maven_release_writes_the_next_snapshots() {
    let repo = maven_units(&common::maven::Units::default(), MAVEN_RELEASED);
    repo.write(
        ".changeset/invoices.md",
        &changeset(&[("billing", "minor")], "Add invoices."),
    );
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("billing 1.3.0 -> 1.4.0: minor"))
        .stdout(contains(
            "orders 0.4.0 -> 0.4.1: billing 1.4.0 is outside its runtime range [1.3.0]",
        ));
    // The deployed POM of `orders` names `billing`'s version exactly, so
    // `orders` is released with it.
    expect_test::expect![[r#"
        # The released version of every unit. Only `cutwater init` and `cutwater version`
        # write this file.

        [units]
        billing = "1.4.0"
        ledger = "2.1.0"
        orders = "0.4.1"
    "#]]
    .assert_eq(&repo.read(".cutwater/versions.toml"));
    assert_eq!(
        property(&repo, "billing/pom.xml", "revision"),
        "1.4.1-SNAPSHOT"
    );
    assert_eq!(
        property(&repo, "orders/pom.xml", "billing.version"),
        "1.4.1-SNAPSHOT"
    );
    assert_eq!(
        property(&repo, "orders/pom.xml", "revision"),
        "0.4.2-SNAPSHOT"
    );
    assert!(
        repo.read("billing/CHANGELOG.md")
            .starts_with("# com.acme:billing\n\n## 1.4.0\n")
    );
    expect_test::expect![[r#"
        # com.acme:orders

        ## 0.4.1

        ### Patch Changes

        - Updated dependencies
          - billing@1.4.0
    "#]]
    .assert_eq(&repo.read("orders/CHANGELOG.md"));
}

#[test]
fn maven_test_dependency_releases_no_dependent() {
    let units = common::maven::Units::default();
    let repo = maven_units(&units, MAVEN_RELEASED);
    repo.write(
        "orders/pom.xml",
        &units.orders_pom().replace(
            "<artifactId>billing-api</artifactId>\n      <version>${billing.version}</version>\n      <type>pom</type>\n",
            "<artifactId>billing-api</artifactId>\n      <version>${billing.version}</version>\n      <type>pom</type>\n      <scope>test</scope>\n",
        ),
    );
    repo.commit("test scope");
    repo.write(
        ".changeset/invoices.md",
        &changeset(&[("billing", "minor")], "Add invoices."),
    );
    repo.cutwater().arg("version").assert().success();
    assert!(
        repo.read(".cutwater/versions.toml")
            .contains("orders = \"0.4.0\"")
    );
    assert_eq!(
        property(&repo, "orders/pom.xml", "billing.version"),
        "1.4.1-SNAPSHOT"
    );
}

#[test]
fn maven_bom_that_manages_a_unit_importing_it() {
    let units = common::maven::Units::default();
    let repo = maven_units(
        &units,
        "[units]\nbilling = \"1.3.0\"\nledger = \"2.1.0\"\norders = \"0.4.0\"\nplatform = \"1.0.0\"\n",
    );
    repo.write(
        "platform/pom.xml",
        "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  \
         <groupId>com.acme</groupId>\n  <artifactId>platform</artifactId>\n  <version>${revision}</version>\n  \
         <packaging>pom</packaging>\n  <properties>\n    <revision>1.0.1-SNAPSHOT</revision>\n    \
         <orders.version>0.4.1-SNAPSHOT</orders.version>\n  </properties>\n  <dependencyManagement>\n    \
         <dependencies>\n      <dependency>\n        <groupId>com.acme</groupId>\n        \
         <artifactId>orders</artifactId>\n        <version>${orders.version}</version>\n      </dependency>\n    \
         </dependencies>\n  </dependencyManagement>\n</project>\n",
    );
    repo.write(
        "orders/pom.xml",
        &units
            .orders_pom()
            .replace(
                "    <ledger.version>",
                "    <platform.version>1.0.1-SNAPSHOT</platform.version>\n    <ledger.version>",
            )
            .replace(
                "  <dependencies>\n",
                "  <dependencyManagement>\n    <dependencies>\n      <dependency>\n        \
                 <groupId>com.acme</groupId>\n        <artifactId>platform</artifactId>\n        \
                 <version>${platform.version}</version>\n        <type>pom</type>\n        \
                 <scope>import</scope>\n      </dependency>\n    </dependencies>\n  \
                 </dependencyManagement>\n  <dependencies>\n",
            ),
    );
    repo.commit("platform");
    repo.write(
        ".changeset/invoices.md",
        &changeset(&[("billing", "minor")], "Add invoices."),
    );
    // `platform` manages `orders`' version, and `orders` imports
    // `platform`: both follow `billing`, and `platform` is no cycle for
    // publication, which managing a version does not order.
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains(
            "Releases, in the order of publication:\n  billing 1.3.0 -> 1.4.0: minor",
        ));
    let manifest = repo.read(".cutwater/versions.toml");
    for entry in [
        "billing = \"1.4.0\"",
        "orders = \"0.4.1\"",
        "platform = \"1.0.1\"",
    ] {
        assert!(manifest.contains(entry), "{entry}\n{manifest}");
    }
}

#[test]
fn maven_prerelease_leads_to_its_final_version() {
    let repo = maven_units(&common::maven::Units::default(), MAVEN_RELEASED);
    repo.cutwater()
        .args(["pre", "enter", "beta"])
        .assert()
        .success();
    repo.write(
        ".changeset/invoices.md",
        &changeset(&[("billing", "minor")], "Add invoices."),
    );
    repo.cutwater().arg("version").assert().success();
    assert!(
        repo.read(".cutwater/versions.toml")
            .contains("billing = \"1.4.0-beta.0\"")
    );
    assert_eq!(
        property(&repo, "billing/pom.xml", "revision"),
        "1.4.0-SNAPSHOT"
    );
    assert_eq!(
        property(&repo, "orders/pom.xml", "billing.version"),
        "1.4.0-SNAPSHOT"
    );
}

#[test]
fn maven_first_release_of_a_new_unit() {
    let repo = maven_units(
        &common::maven::Units {
            orders: "0.1.0-SNAPSHOT",
            ..common::maven::Units::default()
        },
        "[units]\nbilling = \"1.3.0\"\nledger = \"2.1.0\"\n",
    );
    repo.write(
        ".changeset/orders.md",
        &changeset(&[("orders", "minor")], "First release."),
    );
    repo.cutwater()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("orders new -> 0.1.0"));
    assert!(
        repo.read(".cutwater/versions.toml")
            .contains("orders = \"0.1.0\"")
    );
    assert_eq!(
        property(&repo, "orders/pom.xml", "revision"),
        "0.1.1-SNAPSHOT"
    );
}
