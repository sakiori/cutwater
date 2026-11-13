//! The Maven adapter against real Maven: release builds in `goal`,
//! deployment in `publish` and seeding in `init`, with a `file://` deploy
//! repository, isolated settings and local repositories of each test's own.
//! Maven downloads its plugins from Maven Central. The tests are ignored by
//! default; CI's `test-maven` job runs them with the Maven and the JDK that
//! `.gitlab/scripts/install-tools.sh` pins, the Maven named by
//! `CUTWATER_TEST_MVN`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Repo;
use common::maven::{HttpRepository, Units, redeploy};
use common::publish::{GitLab, TOKEN};
use predicates::str::contains;
use serde_json::Value;

/// Maven with settings of its own: no mirror, no profile, and a local
/// repository in the test's directory.
struct Maven {
    mvn: PathBuf,
    dir: tempfile::TempDir,
}

impl Maven {
    fn new() -> Maven {
        let mvn = std::env::var_os("CUTWATER_TEST_MVN")
            .map(PathBuf::from)
            .expect("CUTWATER_TEST_MVN names no Maven; see the module's comment");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("deploy")).unwrap();
        let maven = Maven { mvn, dir };
        maven.settings("", "");
        maven
    }

    /// Writes the settings: the local repository, the mirror of Maven
    /// Central that [`plugin_cache`] serves, the further mirrors `mirrors`
    /// and the further elements `extra`.
    fn settings(&self, mirrors: &str, extra: &str) {
        std::fs::write(
            self.dir.path().join("settings.xml"),
            format!(
                "<settings xmlns=\"http://maven.apache.org/SETTINGS/1.0.0\">\n  \
                 <localRepository>{}</localRepository>\n  <mirrors>\n    <mirror><id>plugins</id>\
                 <mirrorOf>central</mirrorOf><url>file://{}</url></mirror>\n{mirrors}  </mirrors>\n\
                 {extra}</settings>\n",
                self.dir.path().join("local").display(),
                plugin_cache(&self.mvn).display()
            ),
        )
        .unwrap();
    }

    /// A directory of the test's own, for another repository.
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The deploy repository.
    fn deploy(&self) -> PathBuf {
        self.dir.path().join("deploy")
    }

    fn units(&self) -> Units {
        Units {
            deploy: format!("file://{}", self.deploy().display()),
            ..Units::default()
        }
    }

    /// The environment that runs this Maven with these settings.
    fn environment(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        let mut paths = vec![self.mvn.parent().unwrap().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        vec![
            ("PATH", std::env::join_paths(paths).unwrap()),
            (
                "MAVEN_ARGS",
                format!(
                    "--settings {}",
                    self.dir.path().join("settings.xml").display()
                )
                .into(),
            ),
            ("MAVEN_SKIP_RC", "true".into()),
        ]
    }

    /// Deploys the unit in `dir` at `version`, or the modules `projects`
    /// of it, as an earlier release did.
    fn deploy_unit(&self, dir: &Path, version: &str, projects: Option<&str>) {
        self.deploy_unit_with(dir, version, projects, &[]);
    }

    /// Deploys as [`Self::deploy_unit`] does, with the extra arguments
    /// `args`.
    fn deploy_unit_with(&self, dir: &Path, version: &str, projects: Option<&str>, args: &[&str]) {
        let local = tempfile::tempdir().unwrap();
        let mut command = Command::new(&self.mvn);
        command
            .current_dir(dir)
            .envs(self.environment())
            .args(["--batch-mode", "--no-transfer-progress"])
            .arg(format!("-Dmaven.repo.local={}", local.path().display()))
            .arg(format!("-Drevision={version}"))
            .args(args)
            .arg("deploy");
        if let Some(projects) = projects {
            command.args(["--projects", projects]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    /// The POM that the deploy repository holds of `artifact` at `version`.
    fn deployed(&self, artifact: &str, version: &str) -> Option<String> {
        std::fs::read_to_string(
            self.deploy()
                .join("com/acme")
                .join(artifact)
                .join(version)
                .join(format!("{artifact}-{version}.pom")),
        )
        .ok()
    }

    fn cutwater(&self, repo: &Repo) -> assert_cmd::Command {
        let mut command = repo.cutwater();
        command.envs(self.environment());
        command
    }
}

/// The plugins the tests run, resolved from Maven Central once into a local
/// repository kept for later runs, which the tests' settings make the mirror
/// of Maven Central, so that their builds and questions do not ask Maven
/// Central again.
fn plugin_cache(mvn: &Path) -> &'static Path {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("maven-plugins");
        // The cache is current for this Maven and these plugins only.
        let version = Command::new(mvn)
            .arg("--version")
            .env("MAVEN_SKIP_RC", "true")
            .output()
            .unwrap();
        let marker = format!(
            "{}{}",
            String::from_utf8_lossy(&version.stdout)
                .lines()
                .next()
                .unwrap_or_default(),
            common::maven::PLUGINS
        );
        let ready = cache.join(".resolved");
        if std::fs::read_to_string(&ready).ok().as_deref() == Some(marker.as_str()) {
            return cache;
        }
        // A project that runs every plugin of the tests.
        let project = tempfile::tempdir().unwrap();
        let deploy = tempfile::tempdir().unwrap();
        let write = |file: &str, content: &str| {
            let path = project.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        };
        write("settings.xml", "<settings/>\n");
        write(
            "src/main/java/warm/Up.java",
            "package warm;\npublic class Up {}\n",
        );
        write(
            "pom.xml",
            &format!(
                "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  \
                 <modelVersion>4.0.0</modelVersion>\n  <groupId>cutwater.test</groupId>\n  \
                 <artifactId>warm-up</artifactId>\n  <version>${{revision}}</version>\n  \
                 <properties>\n    <revision>1.0.0</revision>\n    \
                 <maven.compiler.release>17</maven.compiler.release>\n  </properties>\n  \
                 <distributionManagement>\n    <repository>\n      <id>deploy</id>\n      \
                 <url>file://{}</url>\n    </repository>\n  </distributionManagement>\n{}\
                 </project>\n",
                deploy.path().display(),
                common::maven::flatten(None)
            ),
        );
        let output = Command::new(mvn)
            .current_dir(project.path())
            .env_remove("MAVEN_ARGS")
            .env("MAVEN_SKIP_RC", "true")
            .args(["--batch-mode", "--no-transfer-progress"])
            .args(["--settings", "settings.xml"])
            .arg(format!("-Dmaven.repo.local={}", cache.display()))
            .arg("deploy")
            .args(
                common::maven::PLUGINS
                    .lines()
                    .map(|plugin| format!("{plugin}:help")),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        std::fs::write(&ready, marker).unwrap();
        cache
    })
}

const CONFIG: &str = "[maven]\ndiscover = true\n";

/// Every unit seeded at its last release: `billing` 1.3.0, `ledger` 2.1.0,
/// `orders` 0.4.0.
const SEEDED: &str = "[units]\nbilling = { version = \"1.3.0\", seeded = true }\n\
                      ledger = { version = \"2.1.0\", seeded = true }\n\
                      orders = { version = \"0.4.0\", seeded = true }\n";

/// A repository with the Maven units of `units` and the manifest `SEEDED`,
/// committed on `main`; returns it and the commit.
fn repository(units: &Units) -> (Repo, String) {
    let repo = Repo::with(
        CONFIG,
        &[
            (".cutwater/versions.toml", SEEDED),
            (".gitignore", "target/\n.flattened-pom.xml\n"),
        ],
    );
    units.write(&repo);
    let commit = repo.commit("maven units");
    (repo, commit)
}

/// `publish` in a pipeline of `main`, against the mock of GitLab.
fn publish(maven: &Maven, repo: &Repo, gitlab: &GitLab) -> assert_cmd::Command {
    let mut command = maven.cutwater(repo);
    command
        .args(["publish", "--json"])
        .env("GITLAB_CI", "true")
        .env("CI_COMMIT_BRANCH", "main")
        .env("CI_SERVER_URL", gitlab.url())
        .env("CI_PROJECT_ID", "7")
        .env("CUTWATER_GITLAB_TOKEN", TOKEN);
    command
}

fn states(output: &[u8]) -> Vec<(String, String)> {
    let record: Value = serde_json::from_slice(output).unwrap();
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

fn changelog(repo: &Repo, dir: &str, package: &str, version: &str) {
    repo.write(
        &format!("{dir}/CHANGELOG.md"),
        &format!("# {package}\n\n## {version}\n\n### Patch Changes\n\n- A change.\n"),
    );
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn release_build_of_a_dependent() {
    let maven = Maven::new();
    let (repo, target) = repository(&maven.units());
    // `ledger` 2.1.0 is in the deploy repository, and the release does not
    // publish it; nothing holds `billing` 1.4.0 before the release.
    maven.deploy_unit(&repo.path().join("ledger"), "2.1.0", None);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = \"1.4.0\"\nledger = { version = \"2.1.0\", seeded = true }\n\
         orders = \"0.4.1\"\n",
    );
    Units {
        billing: "1.4.1-SNAPSHOT",
        billing_property: "1.4.1-SNAPSHOT",
        orders: "0.4.2-SNAPSHOT",
        ..maven.units()
    }
    .write(&repo);
    changelog(&repo, "billing", "com.acme:billing", "1.4.0");
    changelog(&repo, "orders", "com.acme:orders", "0.4.1");
    repo.commit("Release");

    // `orders` builds against the `billing` built before it in the job, and
    // against `ledger` from the deploy repository.
    maven
        .cutwater(&repo)
        .args(["goal", "--base", &target])
        .assert()
        .success()
        .stdout(contains(
            "The maven packages of the release pass the maven adapter's checks.",
        ))
        // Maven's output shows as the builds run.
        .stderr(contains("[INFO] BUILD SUCCESS"));
    assert!(maven.deployed("billing-api", "1.4.0").is_none());

    let mut gitlab = GitLab::new();
    gitlab.no_tag("billing-v1.4.0");
    gitlab.no_tag("orders-v0.4.1");
    let (create, _) = gitlab.releases();
    let assert = publish(&maven, &repo, &gitlab).assert().success();
    create.expect(2).assert();
    assert_eq!(
        states(&assert.get_output().stdout),
        [
            ("billing".to_string(), "published".to_string()),
            ("orders".to_string(), "published".to_string()),
        ]
    );
    for artifact in ["billing", "billing-api", "billing-service"] {
        assert!(maven.deployed(artifact, "1.4.0").is_some(), "{artifact}");
    }
    let orders = maven.deployed("orders", "0.4.1").unwrap();
    assert!(
        orders.contains("<artifactId>billing-api</artifactId>\n      <version>1.4.0</version>"),
        "{orders}"
    );
    assert!(
        orders.contains("<artifactId>ledger</artifactId>\n      <version>2.1.0</version>"),
        "{orders}"
    );
    assert!(!orders.contains("SNAPSHOT"), "{orders}");
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn interrupted_deployment_completed_by_a_retry() {
    let maven = Maven::new();
    let (repo, _) = repository(&maven.units());
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = \"1.4.0\"\nledger = { version = \"2.1.0\", seeded = true }\n\
         orders = { version = \"0.4.0\", seeded = true }\n",
    );
    Units {
        billing: "1.4.1-SNAPSHOT",
        billing_property: "1.4.1-SNAPSHOT",
        ..maven.units()
    }
    .write(&repo);
    changelog(&repo, "billing", "com.acme:billing", "1.4.0");
    repo.commit("Release");
    // An earlier `publish` deployed two of the three modules and failed.
    maven.deploy_unit(
        &repo.path().join("billing"),
        "1.4.0",
        Some("com.acme:billing,com.acme:billing-api"),
    );
    assert!(maven.deployed("billing-service", "1.4.0").is_none());
    let earlier = maven
        .deploy()
        .join("com/acme/billing-api/1.4.0/billing-api-1.4.0.pom");
    let mut text = std::fs::read_to_string(&earlier).unwrap();
    text.push_str("<!-- deployed by the earlier run -->\n");
    redeploy(&earlier, &text);

    let mut gitlab = GitLab::new();
    gitlab.no_tag("billing-v1.4.0");
    let (create, releases) = gitlab.releases();
    let assert = publish(&maven, &repo, &gitlab).assert().success();
    create.expect(1).assert();
    assert_eq!(releases.json()[0]["tag_name"], "billing-v1.4.0");
    assert_eq!(
        states(&assert.get_output().stdout),
        [("billing".to_string(), "published".to_string())]
    );
    assert!(maven.deployed("billing-service", "1.4.0").is_some());
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), text);
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn property_left_in_a_deployed_pom_fails_goal() {
    let maven = Maven::new();
    let units = Units {
        orders_flatten: Some("resolveCiFriendliesOnly"),
        ..maven.units()
    };
    let (repo, target) = repository(&units);
    maven.deploy_unit(&repo.path().join("billing"), "1.3.0", None);
    maven.deploy_unit(&repo.path().join("ledger"), "2.1.0", None);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = { version = \"1.3.0\", seeded = true }\n\
         ledger = { version = \"2.1.0\", seeded = true }\norders = \"0.4.1\"\n",
    );
    Units {
        orders: "0.4.2-SNAPSHOT",
        ..units
    }
    .write(&repo);
    repo.commit("Release");
    maven
        .cutwater(&repo)
        .args(["goal", "--base", &target])
        .assert()
        .code(1)
        .stderr(contains(
            "orders: module `com.acme:orders` would deploy a POM in which the dependency \
             `com.acme:billing-api` has the version `${billing.version}`",
        ));
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn init_seeds_the_highest_final_version_of_the_deploy_repository() {
    let maven = Maven::new();
    let units = Units {
        billing: "1.4.0-SNAPSHOT",
        billing_property: "1.4.0-SNAPSHOT",
        ..maven.units()
    };
    let repo = Repo::with(CONFIG, &[(".gitignore", "target/\n.flattened-pom.xml\n")]);
    repo.remove(".cutwater/config.toml");
    units.write(&repo);
    repo.commit("maven units");
    for version in ["1.2.0", "1.3.0", "1.4.0-rc.1"] {
        maven.deploy_unit(&repo.path().join("billing"), version, None);
    }
    // An empty or unreachable repository looks the same, so a unit without
    // a release is declared new.
    maven
        .cutwater(&repo)
        .arg("init")
        .assert()
        .code(2)
        .stderr(contains(
            "unit `ledger`: the Maven repository `deploy` at file://",
        ))
        .stderr(contains(
            "holds no final version of it. Pass `--new ledger` if it was never released.",
        ));
    assert!(!repo.path().join(".cutwater/config.toml").exists());
    maven
        .cutwater(&repo)
        .args(["init", "--new", "ledger", "--new", "orders"])
        .assert()
        .success()
        .stdout(contains("billing 1.3.0"))
        .stdout(contains("New, without a release yet: ledger, orders."));
    assert!(
        repo.read(".cutwater/versions.toml")
            .contains("billing = { version = \"1.3.0\", seeded = true }")
    );
    repo.commit("adopt cutwater");
    maven
        .cutwater(&repo)
        .args(["check", "--base", "HEAD"])
        .assert()
        .success();
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn init_refuses_a_revision_behind_the_deploy_repository() {
    let maven = Maven::new();
    let units = Units {
        billing: "1.3.0-SNAPSHOT",
        billing_property: "1.3.0-SNAPSHOT",
        ..maven.units()
    };
    let repo = Repo::with(CONFIG, &[(".gitignore", "target/\n.flattened-pom.xml\n")]);
    repo.remove(".cutwater/config.toml");
    units.write(&repo);
    repo.commit("maven units");
    maven.deploy_unit(&repo.path().join("billing"), "1.3.0", None);
    maven
        .cutwater(&repo)
        .arg("init")
        .assert()
        .code(2)
        .stderr(contains(
            "unit `billing`: the Maven repository `deploy` at file://",
        ))
        .stderr(contains(
            "holds 1.3.0, the release that 1.3.0-SNAPSHOT in billing/pom.xml \
             `properties.revision` leads to; set it to the snapshot of a later version, such as \
             1.3.1-SNAPSHOT",
        ));
    assert!(!repo.path().join(".cutwater/config.toml").exists());
}

/// A repository with `units`, whose release commit releases `billing`
/// 1.4.0 alone.
fn billing_release(units: &Units) -> Repo {
    let (repo, _) = repository(units);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = \"1.4.0\"\nledger = { version = \"2.1.0\", seeded = true }\n\
         orders = { version = \"0.4.0\", seeded = true }\n",
    );
    Units {
        billing: "1.4.1-SNAPSHOT",
        billing_property: "1.4.1-SNAPSHOT",
        deploy: units.deploy.clone(),
        orders_flatten: units.orders_flatten,
        billing_api_packaging: units.billing_api_packaging,
        billing_api_signature: units.billing_api_signature,
        reproducible: units.reproducible,
        ..Units::default()
    }
    .write(&repo);
    changelog(&repo, "billing", "com.acme:billing", "1.4.0");
    repo.commit("Release");
    repo
}

/// `publish` of `billing` 1.4.0 that tags it.
fn publish_billing(maven: &Maven, repo: &Repo) -> assert_cmd::assert::Assert {
    let mut gitlab = GitLab::new();
    gitlab.no_tag("billing-v1.4.0");
    let (create, releases) = gitlab.releases();
    let assert = publish(maven, repo, &gitlab).assert();
    let assert = assert.success();
    create.expect(1).assert();
    assert_eq!(releases.json()[0]["tag_name"], "billing-v1.4.0");
    assert
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn bom_import_of_a_co_released_producer() {
    let maven = Maven::new();
    let (repo, target) = repository(&maven.units());
    maven.deploy_unit(&repo.path().join("ledger"), "2.1.0", None);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = \"1.4.0\"\nledger = { version = \"2.1.0\", seeded = true }\n\
         orders = \"0.4.1\"\n",
    );
    let units = Units {
        billing: "1.4.1-SNAPSHOT",
        billing_property: "1.4.1-SNAPSHOT",
        orders: "0.4.2-SNAPSHOT",
        ..maven.units()
    };
    units.write(&repo);
    // `orders` imports `billing`'s aggregator as a BOM, which only the
    // release build of `billing` before it installs.
    let orders = units.orders_pom().replace(
        "  <dependencies>\n",
        "  <dependencyManagement>\n    <dependencies>\n      <dependency>\n        \
         <groupId>com.acme</groupId>\n        <artifactId>billing</artifactId>\n        \
         <version>${billing.version}</version>\n        <type>pom</type>\n        \
         <scope>import</scope>\n      </dependency>\n    </dependencies>\n  \
         </dependencyManagement>\n  <dependencies>\n",
    );
    repo.write("orders/pom.xml", &orders);
    changelog(&repo, "billing", "com.acme:billing", "1.4.0");
    changelog(&repo, "orders", "com.acme:orders", "0.4.1");
    repo.commit("Release");
    maven
        .cutwater(&repo)
        .args(["goal", "--base", &target])
        .assert()
        .success();
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn repository_of_a_settings_profile_does_not_answer_for_the_deploy_repository() {
    let maven = Maven::new();
    let other = maven.dir("other");
    maven.settings(
        "",
        &format!(
            "  <profiles><profile><id>corp</id><repositories><repository><id>other</id>\
         <url>file://{}</url></repository></repositories></profile></profiles>\n  \
         <activeProfiles><activeProfile>corp</activeProfile></activeProfiles>\n",
            other.display()
        ),
    );
    let repo = billing_release(&maven.units());
    // `billing` 1.4.0 is in `other`, which the settings add to every build,
    // and not in the deploy repository.
    maven.deploy_unit_with(
        &repo.path().join("billing"),
        "1.4.0",
        None,
        &[&format!(
            "-DaltDeploymentRepository=other::file://{}",
            other.display()
        )],
    );
    publish_billing(&maven, &repo);
    for artifact in ["billing", "billing-api", "billing-service"] {
        assert!(maven.deployed(artifact, "1.4.0").is_some(), "{artifact}");
    }
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn mirror_of_the_deploy_repository_does_not_answer_for_it() {
    let maven = Maven::new();
    let mirror = maven.dir("mirror");
    maven.settings(
        &format!(
            "    <mirror><id>corp</id><mirrorOf>deploy</mirrorOf><url>file://{}</url></mirror>\n",
            mirror.display()
        ),
        "",
    );
    let repo = billing_release(&maven.units());
    maven.deploy_unit_with(
        &repo.path().join("billing"),
        "1.4.0",
        None,
        &[&format!(
            "-DaltDeploymentRepository=corp::file://{}",
            mirror.display()
        )],
    );
    publish_billing(&maven, &repo);
    for artifact in ["billing", "billing-api", "billing-service"] {
        assert!(maven.deployed(artifact, "1.4.0").is_some(), "{artifact}");
    }
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn module_with_its_pom_but_without_its_jar_is_completed() {
    let maven = Maven::new();
    let units = Units {
        billing_api_packaging: "jar",
        ..maven.units()
    };
    let repo = billing_release(&units);
    maven.deploy_unit(&repo.path().join("billing"), "1.4.0", None);
    // An earlier deployment stopped after the POM of `billing-api`.
    let version = maven.deploy().join("com/acme/billing-api/1.4.0");
    for entry in std::fs::read_dir(&version).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy().contains(".jar") {
            std::fs::remove_file(path).unwrap();
        }
    }

    let assert = publish_billing(&maven, &repo);
    assert_eq!(
        states(&assert.get_output().stdout),
        [("billing".to_string(), "published".to_string())]
    );
    assert!(version.join("billing-api-1.4.0.jar").is_file());
    assert!(version.join("billing-api-1.4.0.jar.sha1").is_file());
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn module_partly_deployed_by_another_build_is_not_completed() {
    let maven = Maven::new();
    let units = Units {
        billing_api_packaging: "jar",
        ..maven.units()
    };
    let repo = billing_release(&units);
    maven.deploy_unit(&repo.path().join("billing"), "1.4.0", None);
    // An earlier, different build deployed the POM of `billing-api` and
    // stopped before its JAR.
    let version = maven.deploy().join("com/acme/billing-api/1.4.0");
    for entry in std::fs::read_dir(&version).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy().contains(".jar") {
            std::fs::remove_file(path).unwrap();
        }
    }
    let pom = version.join("billing-api-1.4.0.pom");
    let mut text = std::fs::read_to_string(&pom).unwrap();
    text.push_str("<!-- built by another run -->\n");
    redeploy(&pom, &text);

    let mut gitlab = GitLab::new();
    gitlab.no_tag("billing-v1.4.0");
    let writes = gitlab.no_writes();
    publish(&maven, &repo, &gitlab)
        .assert()
        .code(2)
        .stderr(contains(
            "module `com.acme:billing-api` holds billing-api-1.4.0.pom, which differ from this \
             build's; the build is not reproducible",
        ))
        .stderr(contains("project.build.outputTimestamp"));
    assert!(!version.join("billing-api-1.4.0.jar").exists());
    for mock in writes {
        mock.assert();
    }
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn repository_that_refuses_replacements_gets_only_the_missing_files() {
    let maven = Maven::new();
    let http = HttpRepository::start();
    let units = Units {
        billing_api_packaging: "jar",
        deploy: http.url(),
        ..Units::default()
    };
    let repo = billing_release(&units);
    maven.deploy_unit(&repo.path().join("billing"), "1.4.0", None);
    // An earlier deployment stopped after the POM of `billing-api`.
    let version = http.path().join("com/acme/billing-api/1.4.0");
    for name in [
        "billing-api-1.4.0.jar",
        "billing-api-1.4.0.jar.sha1",
        "billing-api-1.4.0.jar.md5",
    ] {
        std::fs::remove_file(version.join(name)).unwrap();
    }
    // The aggregator's POM has lost only its SHA-1.
    std::fs::remove_file(
        http.path()
            .join("com/acme/billing/1.4.0/billing-1.4.0.pom.sha1"),
    )
    .unwrap();
    let before = http.uploads().len();

    let assert = publish_billing(&maven, &repo);
    assert_eq!(
        states(&assert.get_output().stdout),
        [("billing".to_string(), "published".to_string())]
    );
    let uploads: Vec<String> = http.uploads()[before..]
        .iter()
        .filter(|path| !path.contains("maven-metadata"))
        .cloned()
        .collect();
    assert_eq!(
        uploads,
        [
            "com/acme/billing/1.4.0/billing-1.4.0.pom.sha1",
            "com/acme/billing-api/1.4.0/billing-api-1.4.0.jar",
            "com/acme/billing-api/1.4.0/billing-api-1.4.0.jar.sha1",
            "com/acme/billing-api/1.4.0/billing-api-1.4.0.jar.md5",
        ]
    );
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn failed_deployment_is_not_taken_for_a_present_one() {
    let maven = Maven::new();
    let http = HttpRepository::start();
    let units = Units {
        billing_api_packaging: "jar",
        deploy: http.url(),
        ..Units::default()
    };
    let repo = billing_release(&units);
    // The repository refuses the JAR, with words that once read as
    // "already published", and has none.
    http.refuse(
        "billing-api-1.4.0.jar",
        400,
        "Repository does not allow updating assets, the asset already exists",
    );
    let mut gitlab = GitLab::new();
    gitlab.no_tag("billing-v1.4.0");
    let writes = gitlab.no_writes();
    publish(&maven, &repo, &gitlab)
        .assert()
        .code(2)
        .stderr(contains(
            "deploying billing-api-1.4.0.jar of `com.acme:billing-api` failed",
        ))
        .stderr(contains(
            "module `com.acme:billing-api` has nothing but lacks billing-api-1.4.0.jar, \
             billing-api-1.4.0.pom",
        ));
    for mock in writes {
        mock.assert();
    }
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn publication_asks_once_per_round() {
    let maven = Maven::new();
    let repo = billing_release(&maven.units());
    // A Maven that logs each invocation before it runs.
    let bin = maven.dir("bin");
    let log = maven.dir.path().join("invocations");
    let shim = bin.join("mvn");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            maven.mvn.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut gitlab = GitLab::new();
    gitlab.no_tag("billing-v1.4.0");
    let (create, _) = gitlab.releases();
    let mut command = publish(&maven, &repo, &gitlab);
    let path = std::env::join_paths(
        [bin.clone()]
            .into_iter()
            .chain(std::env::split_paths(&maven.environment()[0].1)),
    )
    .unwrap();
    command.env("PATH", path).assert().success();
    create.expect(1).assert();
    let invocations = std::fs::read_to_string(&log).unwrap();
    let count = |goal: &str| {
        invocations
            .lines()
            .filter(|line| line.contains(goal))
            .count()
    };
    // One effective POM and one build for the run; one question per round:
    // the registries, before and after the uploads, and the wait for the
    // listing; one upload per file.
    assert_eq!(count(":effective-pom"), 1, "{invocations}");
    assert_eq!(count(" install"), 1, "{invocations}");
    assert_eq!(count("--fail-at-end validate"), 4, "{invocations}");
    assert_eq!(count(":get"), 0, "{invocations}");
    assert_eq!(count(":deploy-file"), 3, "{invocations}");
}

/// A repository of the settings, as corporate builds reach Maven Central
/// through a mirror, which holds the POM `pom` of `org.example.platform`
/// as `artifact` 1.
fn platform_repository(maven: &Maven, artifact: &str) {
    let dir = maven.dir("platform");
    let version = dir.join("org/example/platform").join(artifact).join("1");
    std::fs::create_dir_all(&version).unwrap();
    std::fs::write(
        version.join(format!("{artifact}-1.pom")),
        format!(
            "<project xmlns=\"http://maven.apache.org/POM/4.0.0\"><modelVersion>4.0.0</modelVersion>\
             <groupId>org.example.platform</groupId><artifactId>{artifact}</artifactId>\
             <version>1</version><packaging>pom</packaging></project>\n"
        ),
    )
    .unwrap();
    maven.settings(
        "",
        &format!(
            "  <profiles><profile><id>corp</id><repositories><repository><id>platform</id>\
             <url>file://{}</url></repository></repositories></profile></profiles>\n  \
             <activeProfiles><activeProfile>corp</activeProfile></activeProfiles>\n",
            dir.display()
        ),
    );
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn signed_module_needs_no_checksums_of_its_signatures() {
    let maven = Maven::new();
    let http = HttpRepository::start();
    let units = Units {
        billing_api_packaging: "jar",
        billing_api_signature: Some("signature of the JAR"),
        deploy: http.url(),
        ..Units::default()
    };
    let repo = billing_release(&units);
    let assert = publish_billing(&maven, &repo);
    assert_eq!(
        states(&assert.get_output().stdout),
        [("billing".to_string(), "published".to_string())]
    );
    let uploads = http.uploads();
    assert!(
        uploads.contains(&"com/acme/billing-api/1.4.0/billing-api-1.4.0.jar.asc".to_string()),
        "{uploads:?}"
    );
    assert!(
        !uploads.iter().any(|path| path.contains(".asc.")),
        "{uploads:?}"
    );
    // Each file the repository holds is downloaded once in the run.
    let downloads: Vec<String> = http
        .downloads()
        .into_iter()
        .filter(|path| !path.contains("maven-metadata"))
        .collect();
    let mut once = downloads.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), downloads.len(), "{downloads:?}");
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn signature_made_again_completes_a_reproducible_module() {
    let maven = Maven::new();
    let http = HttpRepository::start();
    let units = Units {
        billing_api_packaging: "jar",
        billing_api_signature: Some("signature of the first run"),
        reproducible: true,
        deploy: http.url(),
        ..Units::default()
    };
    let repo = billing_release(&units);
    maven.deploy_unit(&repo.path().join("billing"), "1.4.0", None);
    // The first run stopped before the POM of `billing-api`; the retry signs
    // the same JAR again, with another signature.
    let version = http.path().join("com/acme/billing-api/1.4.0");
    for suffix in ["", ".sha1", ".md5"] {
        std::fs::remove_file(version.join(format!("billing-api-1.4.0.pom{suffix}"))).unwrap();
    }
    repo.write(
        "billing/billing-api/signature.asc",
        "signature of the retry",
    );
    let before = http.uploads().len();

    let assert = publish_billing(&maven, &repo);
    assert_eq!(
        states(&assert.get_output().stdout),
        [("billing".to_string(), "published".to_string())]
    );
    let uploads: Vec<String> = http.uploads()[before..]
        .iter()
        .filter(|path| !path.contains("maven-metadata"))
        .cloned()
        .collect();
    assert_eq!(
        uploads,
        [
            "com/acme/billing-api/1.4.0/billing-api-1.4.0.pom",
            "com/acme/billing-api/1.4.0/billing-api-1.4.0.pom.sha1",
            "com/acme/billing-api/1.4.0/billing-api-1.4.0.pom.md5",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(version.join("billing-api-1.4.0.jar.asc")).unwrap(),
        "signature of the first run"
    );
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn deployed_pom_importing_an_external_bom() {
    let maven = Maven::new();
    let http = HttpRepository::start();
    platform_repository(&maven, "platform-bom");
    let units = Units {
        deploy: http.url(),
        ..Units::default()
    };
    let (repo, _) = repository(&units);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = { version = \"1.3.0\", seeded = true }\nledger = \"2.2.0\"\n\
         orders = { version = \"0.4.0\", seeded = true }\n",
    );
    let next = Units {
        ledger: "2.2.1-SNAPSHOT",
        ledger_property: "2.2.1-SNAPSHOT",
        deploy: http.url(),
        ..Units::default()
    };
    next.write(&repo);
    // The deployed POM keeps the import, which the deploy repository cannot
    // resolve.
    repo.write(
        "ledger/pom.xml",
        &next.ledger_pom().replace(
            "  <build>\n",
            "  <dependencyManagement>\n    <dependencies>\n      <dependency>\n        \
             <groupId>org.example.platform</groupId>\n        <artifactId>platform-bom</artifactId>\n        \
             <version>1</version>\n        <type>pom</type>\n        <scope>import</scope>\n      \
             </dependency>\n    </dependencies>\n  </dependencyManagement>\n  <build>\n",
        ),
    );
    changelog(&repo, "ledger", "com.acme:ledger", "2.2.0");
    repo.commit("Release");
    for state in ["published", "present"] {
        let mut gitlab = GitLab::new();
        gitlab.no_tag("ledger-v2.2.0");
        let (create, _) = gitlab.releases();
        let assert = publish(&maven, &repo, &gitlab).assert().success();
        create.expect(1).assert();
        assert_eq!(
            states(&assert.get_output().stdout),
            [("ledger".to_string(), state.to_string())]
        );
    }
    assert!(
        std::fs::read_to_string(http.path().join("com/acme/ledger/2.2.0/ledger-2.2.0.pom"))
            .unwrap()
            .contains("platform-bom")
    );
}

#[test]
#[ignore = "needs Maven, a JDK and Maven Central; see the module's comment"]
fn module_with_an_external_parent_kept_in_its_deployed_pom() {
    let maven = Maven::new();
    let http = HttpRepository::start();
    platform_repository(&maven, "corporate-parent");
    let units = Units {
        deploy: http.url(),
        ..Units::default()
    };
    let (repo, _) = repository(&units);
    repo.write(
        ".cutwater/versions.toml",
        "[units]\nbilling = \"1.4.0\"\nledger = { version = \"2.1.0\", seeded = true }\n\
         orders = { version = \"0.4.0\", seeded = true }\n",
    );
    let next = Units {
        billing: "1.4.1-SNAPSHOT",
        billing_property: "1.4.1-SNAPSHOT",
        deploy: http.url(),
        ..Units::default()
    };
    next.write(&repo);
    // `billing` inherits from a corporate parent, which its flatten mode
    // keeps in the deployed POMs and the deploy repository does not hold.
    repo.write(
        "billing/pom.xml",
        &next
            .billing_pom()
            .replace(
                "  <groupId>com.acme</groupId>\n  <artifactId>billing</artifactId>\n",
                "  <parent>\n    <groupId>org.example.platform</groupId>\n    \
                 <artifactId>corporate-parent</artifactId>\n    <version>1</version>\n    \
                 <relativePath/>\n  </parent>\n  <groupId>com.acme</groupId>\n  \
                 <artifactId>billing</artifactId>\n",
            )
            .replace(
                "<updatePomFile>true</updatePomFile>",
                "<updatePomFile>true</updatePomFile>\n          \
                 <flattenMode>resolveCiFriendliesOnly</flattenMode>",
            ),
    );
    changelog(&repo, "billing", "com.acme:billing", "1.4.0");
    repo.commit("Release");
    let assert = publish_billing(&maven, &repo);
    assert_eq!(
        states(&assert.get_output().stdout),
        [("billing".to_string(), "published".to_string())]
    );
    assert!(
        std::fs::read_to_string(http.path().join("com/acme/billing/1.4.0/billing-1.4.0.pom"))
            .unwrap()
            .contains("corporate-parent")
    );
    for artifact in ["billing-api", "billing-service"] {
        assert!(
            http.path()
                .join(format!("com/acme/{artifact}/1.4.0/{artifact}-1.4.0.pom"))
                .is_file(),
            "{artifact}"
        );
    }
}
