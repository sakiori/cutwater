mod common;

use common::{Registry, Repo};
use predicates::prelude::*;
use predicates::str::contains;

/// A pnpm workspace on Changesets: one fixed group, one ignored package,
/// one private package and two pending changesets.
fn changesets_workspace() -> Repo {
    let repo = Repo::new();
    for (path, content) in [
        ("package.json", r#"{"name": "acme", "private": true}"#),
        ("pnpm-workspace.yaml", "packages:\n  - packages/*\n"),
        (
            ".changeset/config.json",
            r#"{
  "$schema": "https://unpkg.com/@changesets/config@3.0.0/schema.json",
  "changelog": "@changesets/cli/changelog",
  "commit": false,
  "fixed": [["@acme/web", "@acme/ui-*"]],
  "linked": [],
  "access": "public",
  "baseBranch": "trunk",
  "ignore": ["@acme/docs*"]
}"#,
        ),
        (".changeset/README.md", "# Changesets\n"),
        (
            ".changeset/brave-lions-dance.md",
            "---\n\"@acme/api\": minor\n---\n\nAdd the export endpoint.\n",
        ),
        (
            ".changeset/quiet-owls-rest.md",
            "---\n\"@acme/web\": patch\n---\n\nFix.\n",
        ),
        (
            "packages/api/package.json",
            r#"{"name": "@acme/api", "version": "1.4.2"}"#,
        ),
        (
            "packages/web/package.json",
            r#"{"name": "@acme/web", "version": "2.1.0"}"#,
        ),
        (
            "packages/ui-kit/package.json",
            r#"{"name": "@acme/ui-kit", "version": "2.1.0"}"#,
        ),
        (
            "packages/docs/package.json",
            r#"{"name": "@acme/docs-site", "version": "0.0.3"}"#,
        ),
        (
            "packages/scripts/package.json",
            r#"{"name": "@acme/scripts", "version": "1.0.0", "private": true}"#,
        ),
    ] {
        repo.write(path, content);
    }
    repo.commit("on Changesets");
    repo
}

#[test]
fn repository_on_changesets() {
    let repo = changesets_workspace();
    let registry = Registry::new(&[
        ("@acme/api", &["1.4.1", "1.4.2"]),
        ("@acme/web", &["2.1.0"]),
        ("@acme/ui-kit", &["2.0.0", "2.1.0"]),
    ]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .success()
        .stdout(contains(
            "Imported from .changeset/config.json: the fixed groups `@acme/web`, the ignored \
             packages `@acme/docs-site`, the base branch `trunk`.\n\
             Kept the tag format of Changesets for the npm units, `{unit}@{version}`.\n",
        ))
        .stderr(
            "warning: .changeset/config.json: `access`, `changelog`, `commit` are not carried \
             over; cutwater has no such settings\n",
        );
    expect_test::expect![[r##"
        # The settings of cutwater. Its JSON Schema, schema/config.schema.json in the
        # cutwater repository, describes them.
        default-branch = "trunk"

        [npm]
        discover = true
        exclude = ["@acme/docs-site"]
        tag = "{unit}@{version}"

        [[group]]
        name = "@acme/web"
        units = ["@acme/web", "@acme/ui-kit"]
        tag = "{group}@{version}"
    "##]]
    .assert_eq(&repo.read(".cutwater/config.toml"));
    expect_test::expect![[r##"
        # The released version of every unit. Only `cutwater init` and `cutwater version`
        # write this file.

        [units]
        "@acme/api" = { version = "1.4.2", seeded = true }
        "@acme/ui-kit" = { version = "2.1.0", seeded = true }
        "@acme/web" = { version = "2.1.0", seeded = true }
    "##]]
    .assert_eq(&repo.read(".cutwater/versions.toml"));
    assert_eq!(
        repo.list(".changeset"),
        [
            "README.md",
            "brave-lions-dance.md",
            "config.json",
            "quiet-owls-rest.md"
        ]
    );
    // The adopted repository plans from the seeded versions.
    repo.cutwater()
        .arg("status")
        .assert()
        .success()
        .stdout(contains(
            "  @acme/api 1.4.2 (seeded) -> 1.5.0: minor (.changeset/brave-lions-dance.md)\n",
        ))
        .stdout(contains(
            "  @acme/ui-kit 2.1.0 (seeded) -> 2.1.1: fixed group @acme/web\n",
        ));
    repo.cutwater()
        .arg("init")
        .assert()
        .code(2)
        .stderr(contains(".cutwater/config.toml already exists"));
}

#[test]
fn changesets_prerelease_mode_is_refused() {
    for path in [".changeset/pre.json", ".changeset/pre/brave-lions-dance.md"] {
        let repo = changesets_workspace();
        repo.write(path, "{}");
        repo.cutwater()
            .arg("init")
            .assert()
            .code(2)
            .stderr(contains("Changesets is in prerelease mode"))
            .stderr(contains("changeset pre exit"));
        assert!(!repo.path().join(".cutwater").exists());
    }
}

#[test]
fn linked_groups_are_refused() {
    let repo = changesets_workspace();
    repo.write(
        ".changeset/config.json",
        r#"{"linked": [["@acme/api", "@acme/web"]]}"#,
    );
    repo.cutwater()
        .arg("init")
        .assert()
        .code(2)
        .stderr(contains("declares `linked` groups"));
}

#[test]
fn repository_without_changesets() {
    let repo = Repo::new();
    for (path, content) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"0.4.0\"\nedition = \"2024\"\npublish = [\"fake\"]\n",
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion.workspace = true\nedition.workspace = true\npublish.workspace = true\n",
        ),
        ("crates/core/src/lib.rs", ""),
        (
            "crates/macros/Cargo.toml",
            "[package]\nname = \"macros\"\nversion.workspace = true\nedition.workspace = true\npublish.workspace = true\n",
        ),
        ("crates/macros/src/lib.rs", ""),
        (
            "crates/xtask/Cargo.toml",
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n",
        ),
        ("crates/xtask/src/main.rs", "fn main() {}\n"),
        (
            "bindings/node/package.json",
            r#"{"name": "core-node", "version": "0.4.0"}"#,
        ),
    ] {
        repo.write(path, content);
    }
    repo.commit("a Cargo workspace");
    let registry = Registry::new(&[("core", &["0.3.0", "0.4.0"]), ("macros", &["0.4.0"])]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .success()
        .stdout(contains(
            "Units `core`, `macros` share their version, written in Cargo.toml \
             `workspace.package.version`, so they form the fixed group `workspace`.",
        ));
    expect_test::expect![[r##"
        # The settings of cutwater. Its JSON Schema, schema/config.schema.json in the
        # cutwater repository, describes them.

        [cargo]
        discover = true
        tag = "{unit}-v{version}"

        [[group]]
        name = "workspace"
        units = ["core", "macros"]
        tag = "v{version}"
    "##]]
    .assert_eq(&repo.read(".cutwater/config.toml"));
    expect_test::expect![[r##"
        # The released version of every unit. Only `cutwater init` and `cutwater version`
        # write this file.

        [units]
        core = { version = "0.4.0", seeded = true }
        macros = { version = "0.4.0", seeded = true }
    "##]]
    .assert_eq(&repo.read(".cutwater/versions.toml"));
    assert_eq!(
        registry.queries(),
        "cargo info core@0.4.0 --registry fake\ncargo info macros@0.4.0 --registry fake\n"
    );
}

#[test]
fn nothing_to_release() {
    let repo = Repo::new();
    repo.write("README.md", "# docs\n");
    repo.commit("docs");
    repo.cutwater()
        .arg("init")
        .assert()
        .code(2)
        .stderr(contains(
            "found no publishable npm package, Cargo crate or Maven unit",
        ));
}

#[test]
fn other_commands_suggest_init() {
    let repo = Repo::new();
    repo.commit("empty");
    repo.cutwater()
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains(".cutwater/config.toml is missing").and(contains("cutwater init")));
}

/// A Changesets repository with one publishable package, `@acme/core`.
fn single_package(workspace: bool) -> Repo {
    let repo = Repo::new();
    repo.write(
        ".changeset/config.json",
        r#"{"updateInternalDependencies": "minor"}"#,
    );
    if workspace {
        repo.write("package.json", r#"{"name": "acme", "private": true}"#);
        repo.write("pnpm-workspace.yaml", "packages:\n  - packages/*\n");
        repo.write(
            "packages/core/package.json",
            r#"{"name": "@acme/core", "version": "1.2.0"}"#,
        );
        repo.write(
            "packages/scripts/package.json",
            r#"{"name": "@acme/scripts", "version": "1.0.0", "private": true}"#,
        );
    } else {
        repo.write(
            "package.json",
            r#"{"name": "@acme/core", "version": "1.2.0"}"#,
        );
    }
    repo.commit("one package");
    repo
}

#[test]
fn tags_kept_on_adoption() {
    let registry = Registry::new(&[("@acme/core", &["1.2.0"])]);
    let repo = single_package(true);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .success()
        .stderr(contains("`updateInternalDependencies` is not carried over"));
    let config = repo.read(".cutwater/config.toml");
    assert!(
        config.contains("[npm]\ndiscover = true\ntag = \"{unit}@{version}\"\n"),
        "{config}"
    );
    let config = cutwater::config::Config::parse(&config).unwrap();
    let units = cutwater::units::Units::resolve(repo.path(), &config).unwrap();
    let core = units.get("@acme/core").unwrap();
    assert_eq!(
        core.tag.as_ref().unwrap().render(&core.name, "1.2.1"),
        "@acme/core@1.2.1"
    );

    let repo = single_package(false);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .success();
    assert!(repo.read(".cutwater/config.toml").ends_with(
        "[npm]\ndiscover = true\ntag = \"{unit}@{version}\"\n\n\
         [[unit]]\nname = \"@acme/core\"\ntag = \"v{version}\"\n"
    ));
}

#[test]
fn unit_added_after_adoption_keeps_the_tags() {
    let repo = Repo::new();
    repo.write("package.json", r#"{"name": "tool", "version": "1.2.0"}"#);
    repo.commit("one package");
    let registry = Registry::new(&[("tool", &["1.2.0"])]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .success();
    repo.commit("adopt cutwater");
    repo.git(&["checkout", "--quiet", "-b", "feature"]);
    repo.write(
        "package.json",
        r#"{"name": "tool", "version": "1.2.0", "workspaces": ["packages/*"]}"#,
    );
    repo.write(
        "packages/plugin/package.json",
        r#"{"name": "plugin", "version": "0.1.0"}"#,
    );
    repo.write(
        ".changeset/plugin.md",
        "---\nplugin: minor\ntool: patch\n---\n\nAdd the plugin.\n",
    );
    repo.commit("add a package");
    repo.cutwater()
        .args(["check", "--base", "main"])
        .assert()
        .success();
    repo.cutwater()
        .arg("status")
        .assert()
        .success()
        .stdout(contains("  plugin new -> 0.1.0"));
    let config = cutwater::config::Config::parse(&repo.read(".cutwater/config.toml")).unwrap();
    let units = cutwater::units::Units::resolve(repo.path(), &config).unwrap();
    let tag = |name: &str| {
        let unit = units.get(name).unwrap();
        unit.tag.as_ref().unwrap().render(name, "1.2.1")
    };
    assert_eq!(tag("tool"), "v1.2.1");
    assert_eq!(tag("plugin"), "plugin-v1.2.1");
}

/// An npm workspace whose `export` names its registry.
fn registry_workspace() -> Repo {
    let repo = Repo::new();
    for (path, content) in [
        (
            "package.json",
            r#"{"name": "acme", "private": true, "workspaces": ["packages/*"]}"#,
        ),
        (
            "packages/api/package.json",
            r#"{"name": "api", "version": "1.4.2"}"#,
        ),
        (
            "packages/export/package.json",
            r#"{"name": "@acme/export", "version": "0.1.0", "publishConfig": {"registry": "https://npm.example.com/"}}"#,
        ),
    ] {
        repo.write(path, content);
    }
    repo.commit("packages");
    repo
}

#[test]
fn package_never_published_is_refused_until_declared_new() {
    let repo = registry_workspace();
    let registry = Registry::new(&[("api", &["1.4.2"])]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .code(2)
        .stderr(contains(
            "unit `@acme/export`: the npm registry https://npm.example.com/ lists no version of \
             it. Pass `--new @acme/export` if it was never published, or `--published \
             @acme/export` if 0.1.0 is published but not listed.\n",
        ));
    assert!(!repo.path().join(".cutwater").exists());
    // A scoped package's registry is set for its scope, which outranks
    // `--registry`.
    assert_eq!(
        registry.queries(),
        "npm view @acme/export versions --json --registry=https://npm.example.com/ \
         --@acme:registry=https://npm.example.com/\n\
         npm view api versions --json\n"
    );
    repo.cutwater()
        .args(["init", "--new", "@acme/export", "--new", "exprt"])
        .env("PATH", registry.path())
        .assert()
        .code(2)
        .stderr(contains("`--new` names `exprt`, which is not a unit"));
    repo.cutwater()
        .args(["init", "--new", "@acme/export"])
        .env("PATH", registry.path())
        .assert()
        .success()
        .stdout(contains("New, without a release yet: @acme/export."));
    assert_eq!(
        repo.read(".cutwater/versions.toml")
            .lines()
            .skip(3)
            .collect::<Vec<_>>(),
        ["[units]", "api = { version = \"1.4.2\", seeded = true }"]
    );
    repo.write(
        ".changeset/a.md",
        "---\n\"@acme/export\": minor\n---\n\nExport tables.\n",
    );
    repo.cutwater()
        .arg("status")
        .assert()
        .success()
        .stdout(contains("  @acme/export new -> 0.1.0: first release"));
}

#[test]
fn other_published_versions_are_refused() {
    let repo = registry_workspace();
    let registry = Registry::new(&[
        ("api", &["1.4.1", "1.10.0", "1.5.0"]),
        ("@acme/export", &["0.1.0"]),
    ]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .code(2)
        .stderr(contains(
            "unit `api`: the npm registry the environment configures does not list 1.4.2, the \
             version it declares in packages/api/package.json `version`; the highest version \
             it lists is 1.10.0.",
        ));
    assert!(!repo.path().join(".cutwater").exists());
}

#[test]
fn crate_versions_from_its_registry() {
    let repo = Repo::new();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.2.0\"\nedition = \"2024\"\npublish = [\"fake\"]\n",
    );
    repo.write("src/main.rs", "fn main() {}\n");
    repo.commit("a crate");
    // 0.2.0 is yanked, so cargo lists only the others.
    let registry = Registry::new(&[("tool", &["0.1.0", "0.1.3"])]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .code(2)
        .stderr(contains(
            "unit `tool`: the Cargo registry `fake` does not list 0.2.0, the version it \
             declares in Cargo.toml `package.version`; the highest version it lists is 0.1.3. \
             Pass `--published tool` if 0.2.0 is published but not listed, as a yanked version \
             is.\n",
        ));
    assert_eq!(
        registry.queries(),
        "cargo info tool@0.2.0 --registry fake\ncargo info tool --registry fake\n"
    );
    repo.cutwater()
        .args(["init", "--new", "tool", "--published", "tool"])
        .env("PATH", registry.path())
        .assert()
        .code(2)
        .stderr(contains("`--new` and `--published` both name `tool`"));
    repo.cutwater()
        .args(["init", "--published", "tool"])
        .env("PATH", registry.path())
        .assert()
        .success();
    // No further query.
    assert_eq!(registry.queries().lines().count(), 2);
    assert!(
        repo.read(".cutwater/versions.toml")
            .contains("tool = { version = \"0.2.0\", seeded = true }\n")
    );
    assert!(repo.read(".cutwater/config.toml").ends_with(
        "[cargo]\ndiscover = true\ntag = \"{unit}-v{version}\"\n\n\
         [[unit]]\nname = \"tool\"\ntag = \"v{version}\"\n"
    ));
}

/// With every unit new, nothing is seeded, and `init` says only what it
/// created and what is new.
#[test]
fn only_new_units_seed_nothing() {
    let repo = Repo::new();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    repo.write("src/main.rs", "fn main() {}\n");
    repo.commit("crate");
    repo.cutwater()
        .args(["init", "--new", "tool"])
        .assert()
        .success()
        .stdout(
            "Created .cutwater/config.toml and .cutwater/versions.toml.\n\
             New, without a release yet: tool. Their first release is the version their files \
             declare.\n",
        );
}

#[test]
fn cargo_asks_with_the_repository_configuration() {
    let repo = Repo::new();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
    );
    repo.write("src/main.rs", "fn main() {}\n");
    // Cargo itself, which the repository's configuration points at a
    // vendored directory, answers without the network that it cannot ask.
    repo.write(
        ".cargo/config.toml",
        "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \"vendor\"\n",
    );
    repo.commit("a crate");
    let before = repo.list(".");
    repo.cutwater()
        .arg("init")
        .assert()
        .code(2)
        .stderr(contains(
            "unit `tool`: the default Cargo registry could not be asked: `cargo info \
             tool@0.2.0`: error: crates-io is replaced with non-remote-registry source dir",
        ))
        .stderr(contains(
            "  Fix the tool, the network or the configuration and run `cutwater init` again; if \
             0.2.0 is published, `--published tool` seeds it without asking.\n",
        ))
        .stderr(contains("--new").not());
    // The throwaway package cargo ran in is gone.
    assert_eq!(repo.list("."), before);
}

#[test]
fn crate_with_several_registries_is_refused() {
    let repo = Repo::new();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.2.0\"\nedition = \"2024\"\n\
         publish = [\"internal\", \"mirror\"]\n",
    );
    repo.write("src/main.rs", "fn main() {}\n");
    repo.commit("a crate");
    let registry = Registry::new(&[("tool", &["0.2.0"])]);
    repo.cutwater()
        .arg("init")
        .env("PATH", registry.path())
        .assert()
        .code(2)
        .stderr(contains(
            "Cargo.toml: crate `tool` names the registries `internal`, `mirror` in `publish`",
        ));
    assert_eq!(registry.queries(), "");
}

#[test]
fn maven_adoption_sets_producer_properties() {
    let repo = Repo::new();
    common::maven::Units {
        billing_property: "1.2.0",
        ..common::maven::Units::default()
    }
    .write(&repo);
    repo.commit("maven units");
    repo.cutwater()
        .args([
            "init", "--new", "billing", "--new", "ledger", "--new", "orders",
        ])
        .assert()
        .success()
        .stdout(contains(
            "Set the version properties of dependents to their producers' revisions in: \
             orders/pom.xml.",
        ));
    assert!(
        repo.read("orders/pom.xml")
            .contains("<billing.version>1.3.1-SNAPSHOT</billing.version>")
    );
    assert!(
        repo.read(".cutwater/config.toml")
            .contains("[maven]\ndiscover = true\n")
    );
    repo.cutwater()
        .args(["check", "--base", "HEAD"])
        .assert()
        .success();
}

#[test]
fn maven_literal_producer_version_is_refused() {
    let repo = Repo::new();
    common::maven::Units::default().write(&repo);
    repo.write(
        "orders/pom.xml",
        &common::maven::Units::default().orders_pom().replace(
            "<version>${billing.version}</version>",
            "<version>1.3.0</version>",
        ),
    );
    repo.commit("maven units");
    let before = repo.snapshot();
    repo.cutwater()
        .args([
            "init", "--new", "billing", "--new", "ledger", "--new", "orders",
        ])
        .assert()
        .code(2)
        .stderr(contains(
            "orders: orders/pom.xml: `com.acme:billing-api` of unit `billing` has the version \
             `1.3.0`; a unit refers to another through a version property",
        ));
    assert_eq!(repo.snapshot(), before);
    repo.cutwater()
        .args(["init", "--published", "billing"])
        .assert()
        .code(2)
        .stderr(contains(
            "`--published` names `billing`, whose files hold no released version",
        ));
}

#[test]
fn malformed_pom_outside_maven_units_is_ignored() {
    let repo = Repo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"tool\",\n  \"version\": \"0.1.0\"\n}\n",
    );
    repo.write("fixtures/broken/pom.xml", "<project><artifactId>");
    repo.commit("package");
    repo.cutwater()
        .args(["init", "--new", "tool"])
        .assert()
        .success();
    assert!(!repo.read(".cutwater/config.toml").contains("[maven]"));
}
