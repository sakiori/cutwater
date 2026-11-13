use super::mvn::{
    Deployment, File as MavenFile, Holding, Isolation, Module, Options, Repository, absence,
    errors, installed_problems, metadata_versions,
};
use super::*;
use crate::adapter::{ReferenceWrite, VersionWrite};
use std::path::PathBuf;

fn write(root: &Path, file: &str, content: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn unit(name: &str, path: &str) -> UnitRef {
    UnitRef {
        name: name.to_string(),
        path: path.to_string(),
    }
}

/// A POM with the given elements inside `project`.
fn pom(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n{body}</project>\n"
    )
}

/// A unit's aggregator `artifact` with the revision `revision`, the modules
/// `modules`, and more `body`.
fn aggregator(
    artifact: &str,
    revision: &str,
    modules: &[&str],
    properties: &str,
    body: &str,
) -> String {
    let modules: String = modules
        .iter()
        .map(|module| format!("    <module>{module}</module>\n"))
        .collect();
    let modules = if modules.is_empty() {
        String::new()
    } else {
        format!("  <modules>\n{modules}  </modules>\n")
    };
    pom(&format!(
        "  <groupId>com.acme</groupId>\n  <artifactId>{artifact}</artifactId>\n  <version>${{revision}}</version>\n  <packaging>pom</packaging>\n\
         <properties>\n    <revision>{revision}</revision>\n{properties}  </properties>\n{modules}{body}"
    ))
}

/// A module of the aggregator `parent` in the directory above it.
fn module(parent: &str, artifact: &str, body: &str) -> String {
    pom(&format!(
        "  <parent>\n    <groupId>com.acme</groupId>\n    <artifactId>{parent}</artifactId>\n    <version>${{revision}}</version>\n  </parent>\n  <artifactId>{artifact}</artifactId>\n{body}"
    ))
}

fn dependency(artifact: &str, version: &str, extra: &str) -> String {
    let version = if version.is_empty() {
        String::new()
    } else {
        format!("<version>{version}</version>")
    };
    format!(
        "    <dependency><groupId>com.acme</groupId><artifactId>{artifact}</artifactId>{version}{extra}</dependency>\n"
    )
}

fn dependencies(entries: &[String]) -> String {
    format!("  <dependencies>\n{}  </dependencies>\n", entries.concat())
}

/// A repository with the units `billing` (modules `billing-api`,
/// `billing-service`), `ledger` and `orders`, which depends on both, a root
/// aggregator that is no unit, and a sample application that refers to
/// `billing-api` with a released version.
fn repository(root: &Path) -> Vec<UnitRef> {
    write(
        root,
        "pom.xml",
        &pom(
            "  <groupId>com.acme</groupId>\n  <artifactId>build</artifactId>\n  <version>1</version>\n  <packaging>pom</packaging>\n  <modules>\n    <module>billing</module>\n    <module>orders</module>\n  </modules>\n",
        ),
    );
    write(
        root,
        "billing/pom.xml",
        &aggregator(
            "billing",
            "1.3.1-SNAPSHOT",
            &["billing-api", "billing-service"],
            "",
            "",
        ),
    );
    write(
        root,
        "billing/billing-api/pom.xml",
        &module("billing", "billing-api", ""),
    );
    write(
        root,
        "billing/billing-service/pom.xml",
        &module(
            "billing",
            "billing-service",
            &dependencies(&[dependency("billing-api", "${project.version}", "")]),
        ),
    );
    write(
        root,
        "billing/billing-api/src/it/sample/pom.xml",
        &aggregator("sample", "9.9.9-SNAPSHOT", &[], "", ""),
    );
    write(
        root,
        "ledger/pom.xml",
        &aggregator("ledger", "2.1.1-SNAPSHOT", &[], "", ""),
    );
    write(
        root,
        "orders/pom.xml",
        &aggregator(
            "orders",
            "0.4.0-SNAPSHOT",
            &["orders-core"],
            "    <billing.version>1.3.1-SNAPSHOT</billing.version>\n    <ledger.version>2.1.1-SNAPSHOT</ledger.version>\n",
            "  <dependencyManagement>\n  <dependencies>\n    <dependency><groupId>com.acme</groupId><artifactId>billing-api</artifactId><version>${billing.version}</version></dependency>\n  </dependencies>\n  </dependencyManagement>\n",
        ),
    );
    write(
        root,
        "orders/orders-core/pom.xml",
        &module(
            "orders",
            "orders-core",
            &dependencies(&[
                dependency("billing-api", "", ""),
                dependency("ledger", "${ledger.version}", "<scope>test</scope>"),
            ]),
        ),
    );
    write(
        root,
        "samples/app/pom.xml",
        &pom(&format!(
            "  <groupId>com.acme</groupId>\n  <artifactId>app</artifactId>\n  <version>1</version>\n{}",
            dependencies(&[dependency("billing-api", "1.3.0", "")])
        )),
    );
    write(root, "orders/target/pom.xml", "not a POM");
    vec![
        unit("billing", "billing"),
        unit("ledger", "ledger"),
        unit("orders", "orders"),
    ]
}

/// The released versions of the repository's units.
fn released() -> Vec<VersionWrite> {
    [
        ("billing", "1.3.0"),
        ("ledger", "2.1.0"),
        ("orders", "0.3.0"),
    ]
    .iter()
    .map(|(unit, version)| VersionWrite {
        unit: unit.to_string(),
        version: version.to_string(),
    })
    .collect()
}

fn inspect(root: &Path, units: &[UnitRef]) -> Result<Inspection, AdapterError> {
    Maven.inspect(&InspectRequest {
        root: root.to_path_buf(),
        units: units.to_vec(),
        released: released(),
    })
}

fn problems(inspection: &Inspection) -> Vec<String> {
    inspection
        .problems
        .iter()
        .map(|problem| format!("{}: {}", problem.units.join(", "), problem.message))
        .collect()
}

#[test]
fn aggregators_with_a_revision_are_units() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    repository(root);
    // A module that is an aggregator with a revision of its own belongs to
    // the unit above it.
    write(
        root,
        "billing/billing-service/pom.xml",
        &aggregator("billing-service", "1.3.1-SNAPSHOT", &[], "", ""),
    );
    let units: Vec<(String, String)> = Maven
        .discover(&DiscoverRequest {
            root: root.to_path_buf(),
        })
        .unwrap()
        .units
        .into_iter()
        .map(|unit| (unit.name, unit.path))
        .collect();
    assert_eq!(
        units,
        [
            ("billing".to_string(), "billing".to_string()),
            ("ledger".to_string(), "ledger".to_string()),
            ("orders".to_string(), "orders".to_string()),
        ]
    );
}

#[test]
fn versions_and_edges() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    let inspection = inspect(root, &units).unwrap();
    let versions: Vec<String> = inspection
        .units
        .iter()
        .map(|unit| {
            format!(
                "{} {} ({}) holds release {} at {}",
                unit.name, unit.version, unit.package, unit.holds_release, unit.location
            )
        })
        .collect();
    assert_eq!(
        versions,
        [
            "billing 1.3.1-SNAPSHOT (com.acme:billing) holds release false at billing/pom.xml `properties.revision`",
            "ledger 2.1.1-SNAPSHOT (com.acme:ledger) holds release false at ledger/pom.xml `properties.revision`",
            "orders 0.4.0-SNAPSHOT (com.acme:orders) holds release false at orders/pom.xml `properties.revision`",
        ]
    );
    let edges: Vec<String> = inspection
        .edges
        .iter()
        .map(|edge| {
            format!(
                "{} ({:?}) -> {} {} `{}` {:?}{}{} at {}",
                edge.dependent_path,
                edge.dependent_unit,
                edge.dependency,
                edge.kind,
                edge.reference,
                edge.range,
                if edge.follows { ", follows" } else { "" },
                if edge.published_first {
                    ", published first"
                } else {
                    ""
                },
                edge.location
            )
        })
        .collect();
    expect_test::expect![[r#"
        [
            "orders/orders-core (Some(\"orders\")) -> billing runtime `` None, follows, published first at orders/orders-core/pom.xml `dependencies.com.acme:billing-api`",
            "orders/orders-core (Some(\"orders\")) -> ledger development `${ledger.version}` Some(\"[2.1.0]\"), follows, published first at orders/pom.xml `properties.ledger.version`",
            "orders (Some(\"orders\")) -> billing managed `${billing.version}` Some(\"[1.3.0]\"), follows at orders/pom.xml `properties.billing.version`",
            "samples/app (None) -> billing runtime `1.3.0` None, published first at samples/app/pom.xml `dependencies.com.acme:billing-api`",
        ]
    "#]]
    .assert_debug_eq(&edges);
    assert!(
        inspection.problems.is_empty(),
        "{:?}",
        problems(&inspection)
    );
}

#[test]
fn revisions_are_snapshots_of_semantic_versions() {
    for (revision, valid) in [
        ("1.3.1-SNAPSHOT", true),
        ("0.1.0-SNAPSHOT", true),
        ("1.0-SNAPSHOT", false),
        ("1.3.1", false),
        ("1.3.1-rc.1-SNAPSHOT", false),
        ("1.3.1-snapshot", false),
        ("${sha1}-SNAPSHOT", false),
    ] {
        assert_eq!(is_snapshot_revision(revision), valid, "{revision}");
    }
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    write(
        root,
        "billing/pom.xml",
        &aggregator(
            "billing",
            "1.0-SNAPSHOT",
            &["billing-api", "billing-service"],
            "",
            "",
        ),
    );
    let err = inspect(root, &units).unwrap_err();
    assert_eq!(err.file, "billing/pom.xml");
    assert!(
        err.message
            .starts_with("unit `billing` sets `revision` to `1.0-SNAPSHOT`, but a Maven unit's"),
        "{}",
        err.message
    );
}

#[test]
fn malformed_units_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    write(
        root,
        "billing/billing-api/pom.xml",
        &module("billing", "billing-api", "  <version>1.0.0</version>\n"),
    );
    let err = inspect(root, &units).unwrap_err();
    assert_eq!(err.file, "billing/billing-api/pom.xml");
    assert!(
        err.message.contains("whose modules share its version"),
        "{}",
        err.message
    );

    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    write(
        root,
        "ledger/pom.xml",
        &pom(
            "  <groupId>com.acme</groupId>\n  <artifactId>ledger</artifactId>\n  <version>2.1.0</version>\n",
        ),
    );
    let err = inspect(root, &units).unwrap_err();
    assert_eq!(err.file, "ledger/pom.xml");
    assert!(
        err.message
            .contains("unit `ledger` sets its version to `2.1.0`, but a Maven unit's aggregator"),
        "{}",
        err.message
    );
}

#[test]
fn references_that_break_the_rules() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    write(
        root,
        "orders/pom.xml",
        &aggregator(
            "orders",
            "0.4.0-SNAPSHOT",
            &["orders-core"],
            "    <billing.version>1.3.0</billing.version>\n    <ledger.version>2.1.1-SNAPSHOT</ledger.version>\n",
            "",
        ),
    );
    write(
        root,
        "orders/orders-core/pom.xml",
        &module(
            "orders",
            "orders-core",
            &dependencies(&[
                dependency("billing-api", "${billing.version}", ""),
                dependency("billing-service", "1.3.0", ""),
                dependency("ledger", "${ledger.release}", ""),
            ]),
        ),
    );
    write(
        root,
        "ledger/pom.xml",
        &aggregator(
            "ledger",
            "2.1.1-SNAPSHOT",
            &[],
            "    <shared.version>2.1.1-SNAPSHOT</shared.version>\n",
            &format!(
                "  <parent><groupId>com.acme</groupId><artifactId>billing-api</artifactId><version>1.3.0</version><relativePath>../billing/billing-api</relativePath></parent>\n{}",
                dependencies(&[dependency("billing-service", "${shared.version}", "")])
            ),
        ),
    );
    write(
        root,
        "billing/billing-api/pom.xml",
        &module(
            "billing",
            "billing-api",
            &dependencies(&[dependency("orders-core", "${shared.version}", "")]),
        ),
    );
    write(
        root,
        "billing/pom.xml",
        &aggregator(
            "billing",
            "1.3.1-SNAPSHOT",
            &["billing-api", "billing-service"],
            "    <shared.version>2.1.1-SNAPSHOT</shared.version>\n",
            "",
        ),
    );
    let inspection = inspect(root, &units).unwrap();
    expect_test::expect![[r#"
        [
            "billing: billing/pom.xml: the property `shared.version` is `2.1.1-SNAPSHOT`, but it refers to unit `orders`, whose revision is `0.4.0-SNAPSHOT`; keep it equal to the revision, as `cutwater version` does",
            "ledger: ledger/pom.xml: unit `ledger` inherits from `com.acme:billing-api` of unit `billing`, but Maven 3 takes no property for a parent's version; a unit inherits only from its own POMs or from POMs outside the repository",
            "ledger: ledger/pom.xml: the property `shared.version` is `2.1.1-SNAPSHOT`, but it refers to unit `billing`, whose revision is `1.3.1-SNAPSHOT`; keep it equal to the revision, as `cutwater version` does",
            "orders: orders/pom.xml: the property `billing.version` is `1.3.0`, but it refers to unit `billing`, whose revision is `1.3.1-SNAPSHOT`; keep it equal to the revision, as `cutwater version` does",
            "orders: orders/orders-core/pom.xml: `com.acme:billing-service` of unit `billing` has the version `1.3.0`; a unit refers to another through a version property, such as `${billing.version}`, kept equal to its revision",
            "orders: orders/orders-core/pom.xml: `com.acme:ledger` of unit `ledger` has the version `${ledger.release}`, but neither this POM nor its parents in the repository define `ledger.release`",
        ]
    "#]]
    .assert_debug_eq(&problems(&inspection));

    // Adoption fixes what a property value fixes, and refuses the rest.
    let adoption = Maven
        .adopt(&InspectRequest {
            root: root.to_path_buf(),
            units: units.clone(),
            released: Vec::new(),
        })
        .unwrap();
    let files: Vec<&str> = adoption
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        files,
        ["billing/pom.xml", "ledger/pom.xml", "orders/pom.xml"]
    );
    assert!(
        adoption.files[2]
            .content
            .contains("<billing.version>1.3.1-SNAPSHOT</billing.version>")
    );
    assert_eq!(adoption.problems.len(), 3);
}

#[test]
fn one_property_for_two_units_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    write(
        root,
        "orders/orders-core/pom.xml",
        &module(
            "orders",
            "orders-core",
            &dependencies(&[
                dependency("billing-api", "${billing.version}", ""),
                dependency("ledger", "${billing.version}", ""),
            ]),
        ),
    );
    let messages = problems(&inspect(root, &units).unwrap());
    assert!(
        messages.iter().any(|message| message
            .contains("the property `billing.version` refers to the units `billing`, `ledger`")),
        "{messages:?}"
    );
}

#[test]
fn releases_write_the_next_snapshots() {
    for (version, snapshot) in [
        ("1.4.0", "1.4.1-SNAPSHOT"),
        ("2.0.0", "2.0.1-SNAPSHOT"),
        ("1.5.0-beta.0", "1.5.0-SNAPSHOT"),
        ("0.1.0", "0.1.1-SNAPSHOT"),
    ] {
        assert_eq!(
            next_snapshot(version, "pom.xml").unwrap(),
            snapshot,
            "{version}"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    let writes = Maven
        .write(&WriteRequest {
            root: root.to_path_buf(),
            units,
            versions: vec![VersionWrite {
                unit: "billing".to_string(),
                version: "1.4.0".to_string(),
            }],
            references: Vec::new(),
        })
        .unwrap();
    let text: String = writes
        .files
        .iter()
        .map(|file| {
            let properties = &file.content[file.content.find("<properties>").unwrap()
                ..file.content.find("</properties>").unwrap()];
            format!("--- {}\n{}\n", file.path, properties.trim_end())
        })
        .collect();
    expect_test::expect![[r#"
        --- billing/pom.xml
        <properties>
            <revision>1.4.1-SNAPSHOT</revision>
        --- orders/pom.xml
        <properties>
            <revision>0.4.0-SNAPSHOT</revision>
            <billing.version>1.4.1-SNAPSHOT</billing.version>
            <ledger.version>2.1.1-SNAPSHOT</ledger.version>
    "#]]
    .assert_eq(&text);

    // A reference to rewrite takes the snapshot after its version too.
    let inspection = inspect(root, &repository(root)).unwrap();
    let edge = inspection
        .edges
        .into_iter()
        .find(|edge| edge.dependency == "ledger")
        .unwrap();
    let writes = Maven
        .write(&WriteRequest {
            root: root.to_path_buf(),
            units: repository(root),
            versions: Vec::new(),
            references: vec![ReferenceWrite {
                edge,
                version: "3.0.0".to_string(),
            }],
        })
        .unwrap();
    assert!(
        writes.files[0]
            .content
            .contains("<ledger.version>3.0.1-SNAPSHOT</ledger.version>")
    );
}

#[test]
fn units_build_in_dependency_order_with_released_versions() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    let analysis = Analysis::of(root, &units, &[]).unwrap();
    let released = |pairs: &[(&str, &str)]| -> Vec<VersionWrite> {
        pairs
            .iter()
            .map(|(unit, version)| VersionWrite {
                unit: unit.to_string(),
                version: version.to_string(),
            })
            .collect()
    };
    assert_eq!(
        analysis
            .build_properties(
                "orders",
                "0.4.0",
                &released(&[
                    ("billing", "1.4.0"),
                    ("ledger", "2.1.0"),
                    ("orders", "0.3.0")
                ]),
                true,
            )
            .unwrap(),
        [
            ("billing.version".to_string(), "1.4.0".to_string()),
            ("ledger.version".to_string(), "2.1.0".to_string()),
            ("revision".to_string(), "0.4.0".to_string()),
        ]
    );
    let only_billing = released(&[("billing", "1.4.0")]);
    let missing = analysis
        .build_properties("orders", "0.4.0", &only_billing, true)
        .unwrap_err();
    assert!(
        missing.message.contains("unit `ledger`"),
        "{}",
        missing.message
    );
    assert_eq!(
        analysis
            .build_properties("orders", "0.4.0", &only_billing, false)
            .unwrap()
            .len(),
        2
    );

    // Every reference orders publication, test dependencies included,
    // since the release build resolves them.
    let config = crate::config::Config::parse("[maven]\ndiscover = true\n").unwrap();
    let units = crate::units::Units::resolve(root, &config).unwrap();
    let edges = analysis.edges;
    let order = crate::graph::order(&units, &edges).unwrap();
    let position = |name: &str| order.iter().position(|unit| unit == name).unwrap();
    assert!(position("billing") < position("orders"));
    assert!(position("ledger") < position("orders"));
}

#[test]
fn references_accept_the_exact_version_they_name() {
    let accepts = |range: &str| {
        Maven.accepts(&AcceptsRequest {
            range: range.to_string(),
            version: "2.0.0".to_string(),
            file: "pom.xml".to_string(),
        })
    };
    assert!(accepts("[2.0.0]").unwrap());
    assert!(accepts("2.0.0").unwrap());
    assert!(!accepts("[1.3.0]").unwrap());
    assert!(!accepts("[2.0.0-beta.0]").unwrap());
    assert!(accepts("[1.0,2.0)").is_err());
}

#[test]
fn highest_final_version() {
    let versions = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        highest_final(&versions(&[
            "1.2.0",
            "1.10.0",
            "2.0.0-rc.1",
            "1.9",
            "1.3.0"
        ])),
        Some("1.10.0".to_string())
    );
    assert_eq!(highest_final(&versions(&["2.0.0-rc.1"])), None);
    assert_eq!(
        metadata_versions(
            "<metadata><groupId>com.acme</groupId><versioning><release>1.3.0</release>\
             <versions><version>1.2.0</version><version> 1.3.0 </version></versions></versioning></metadata>"
        )
        .unwrap(),
        ["1.2.0", "1.3.0"]
    );
}

#[test]
fn effective_deployment() {
    let effective = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Generated by Maven Help Plugin -->
<projects>
  <!-- Effective POM for project 'com.acme:billing:pom:1.4.0' -->
  <project xmlns="http://maven.apache.org/POM/4.0.0">
    <groupId>com.acme</groupId>
    <artifactId>billing</artifactId>
    <version>1.4.0</version>
    <distributionManagement>
      <repository><id>releases</id><url>https://repo.example/releases</url></repository>
      <snapshotRepository><id>snapshots</id><url>https://repo.example/snapshots</url></snapshotRepository>
    </distributionManagement>
  </project>
  <project xmlns="http://maven.apache.org/POM/4.0.0">
    <groupId>com.acme</groupId>
    <artifactId>billing-api</artifactId>
    <properties><altReleaseDeploymentRepository>other::default::https://other.example/maven</altReleaseDeploymentRepository></properties>
    <distributionManagement>
      <repository><id>releases</id><url>https://repo.example/releases</url></repository>
    </distributionManagement>
  </project>
  <project xmlns="http://maven.apache.org/POM/4.0.0">
    <groupId>com.acme</groupId>
    <artifactId>billing-it</artifactId>
    <properties><maven.deploy.skip>true</maven.deploy.skip></properties>
  </project>
  <project xmlns="http://maven.apache.org/POM/4.0.0">
    <groupId>com.acme</groupId>
    <artifactId>billing-docs</artifactId>
    <build><plugins><plugin><artifactId>maven-deploy-plugin</artifactId>
      <executions><execution><id>default-deploy</id><goals><goal>deploy</goal></goals>
      <configuration><skip>true</skip></configuration></execution></executions>
    </plugin></plugins></build>
  </project>
</projects>
"#;
    let deployment = Deployment::parse(effective, "billing").unwrap();
    let modules: Vec<String> = deployment
        .modules
        .iter()
        .map(|module| format!("{} -> {}", module.artifact_id, module.repository.id))
        .collect();
    assert_eq!(modules, ["billing -> releases", "billing-api -> other"]);
    assert_eq!(
        deployment.describe(),
        "the Maven repository `releases` at https://repo.example/releases and the Maven \
         repository `other` at https://other.example/maven"
    );

    let single = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <groupId>com.acme</groupId><artifactId>ledger</artifactId></project>"#;
    let problem = Deployment::parse(single, "ledger").unwrap_err();
    assert!(
        problem.contains("module `com.acme:ledger` of unit `ledger` names no repository"),
        "{problem}"
    );
}

#[test]
fn deployed_poms_carry_released_versions() {
    let installed = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <groupId>com.acme</groupId>
  <artifactId>orders-core</artifactId>
  <version>0.4.0</version>
  <parent><groupId>com.acme</groupId><artifactId>orders</artifactId><version>0.4.0</version></parent>
  <dependencies>
    <dependency><groupId>com.acme</groupId><artifactId>billing-api</artifactId><version>${billing.version}</version></dependency>
    <dependency><groupId>com.acme</groupId><artifactId>ledger</artifactId><version>2.1.0</version></dependency>
  </dependencies>
  <dependencyManagement><dependencies>
    <dependency><groupId>com.acme</groupId><artifactId>audit</artifactId><version>1.0.1-SNAPSHOT</version></dependency>
  </dependencies></dependencyManagement>
  <profiles><profile><id>extra</id><dependencies>
    <dependency><groupId>com.acme</groupId><artifactId>ledger</artifactId><version>${ledger.version}</version></dependency>
  </dependencies></profile></profiles>
</project>"#;
    expect_test::expect![[r#"
        [
            "module `com.acme:orders-core` would deploy a POM in which the dependency `com.acme:billing-api` has the version `${billing.version}`, an unresolved property; configure the flatten plugin to interpolate `dependencies` and `dependencyManagement`",
            "module `com.acme:orders-core` would deploy a POM in which the managed dependency `com.acme:audit` has the snapshot version `1.0.1-SNAPSHOT`",
            "module `com.acme:orders-core` would deploy a POM in which the dependency `com.acme:ledger` of the profile `extra` has the version `${ledger.version}`, an unresolved property; configure the flatten plugin to interpolate `dependencies` and `dependencyManagement`",
        ]
    "#]]
    .assert_debug_eq(&installed_problems("com.acme:orders-core", installed));
    let module = Module {
        group_id: "com.acme".to_string(),
        artifact_id: "orders-core".to_string(),
        repository: Repository {
            id: "releases".to_string(),
            url: "https://repo.example/releases".to_string(),
        },
    };
    let local = tempfile::tempdir().unwrap();
    assert_eq!(
        module.check_installed(local.path(), "0.4.0"),
        ["the release build installed no POM of module `com.acme:orders-core` at 0.4.0"]
    );
}

#[test]
fn answers_of_one_round_of_questions() {
    let answers = |pairs: &[(&str, bool)]| {
        pairs
            .iter()
            .map(|(notation, saved)| (notation.to_string(), *saved))
            .collect::<Vec<_>>()
    };
    assert_eq!(absence("[INFO] BUILD FAILURE\n", &[]), Ok(()));
    let missing = "[ERROR] Failed to execute goal org.apache.maven.plugins:maven-dependency-plugin:\
                   3.8.1:list-classes (ask) on project file-1: Couldn't download artifact: The \
                   following artifacts could not be resolved: com.acme:billing-api:jar:sources:1.4.0 \
                   (absent): Could not find artifact com.acme:billing-api:jar:sources:1.4.0 in \
                   releases (https://repo.example/releases)\n\
                   [ERROR] Failed to execute goal on project file-0: Couldn't download artifact: \
                   zip END header not found\n";
    let sources = |pom_saved| {
        answers(&[
            ("com.acme:billing-api:pom:1.4.0", pom_saved),
            ("com.acme:billing-api:jar:sources:1.4.0", false),
        ])
    };
    assert_eq!(absence(missing, &sources(true)), Ok(()));
    let cached = "[ERROR] com.acme:billing-api:jar:1.4.0 was not found in \
                  https://repo.example/releases during a previous attempt.\n";
    assert_eq!(
        absence(
            cached,
            &answers(&[("com.acme:billing-api:jar:1.4.0", false)])
        ),
        Ok(())
    );
    // A file Maven saved nothing of, but did not name as not found, is no
    // answer: a POM it could not read names the other files too.
    assert!(absence(missing, &sources(false)).is_err());
    let descriptor = "[ERROR] Failed to read artifact descriptor for com.acme:billing-api:jar:1.4.0: \
                      The following artifacts could not be resolved: org.example:parent:pom:9 \
                      (absent): Could not find artifact org.example:parent:pom:9 in releases\n";
    assert!(
        absence(
            descriptor,
            &answers(&[("com.acme:billing-api:jar:1.4.0", false)])
        )
        .is_err()
    );
    // The goal fails for every saved file that is no JAR; the errors leave
    // that out and keep the cause.
    let denied = "[ERROR] Failed to execute goal on project file-0: Couldn't download artifact: \
                  zip END header not found\n\
                  [ERROR] Failed to execute goal on project file-1: Couldn't download artifact: \
                  Could not transfer artifact com.acme:billing-api:jar:1.4.0 from/to releases: \
                  status code: 401\n\
                  [ERROR] \n[ERROR] To see the full stack trace of the errors, re-run Maven with the -e switch.\n";
    assert_eq!(
        absence(
            denied,
            &answers(&[
                ("com.acme:billing-api:pom:1.4.0", true),
                ("com.acme:billing-api:jar:1.4.0", false),
            ])
        ),
        Err(
            "Failed to execute goal on project file-1: Couldn't download artifact: Could not \
             transfer artifact com.acme:billing-api:jar:1.4.0 from/to releases: status code: 401"
                .to_string()
        )
    );
    assert_eq!(errors("[INFO] one\n[INFO] two\n"), "[INFO] one\n[INFO] two");
}

#[test]
fn versions_of_maven() {
    use super::mvn::{maven_version, supported};
    for (output, version) in [
        (
            "Apache Maven 3.9.16 (2bdd9fdd)\nMaven home: /m\n",
            Some("3.9.16"),
        ),
        ("\nApache Maven 3.8.8 (4c87b05d)\n", Some("3.8.8")),
        ("Apache Maven 4.0.0-rc-4 (bed0f8c6)\n", Some("4.0.0-rc-4")),
        ("mvn: command not found\n", None),
    ] {
        assert_eq!(
            maven_version(output).ok().map(|v| v.to_string()).as_deref(),
            version,
            "{output}"
        );
    }
    let supported = |version: &str| supported(&Version::parse(version).unwrap());
    assert!(supported("3.9.0").is_ok());
    assert!(supported("3.9.16").is_ok());
    assert_eq!(
        supported("3.8.8").unwrap_err(),
        "Maven units need Maven 3.9 or a later Maven 3, but the `mvn` on the PATH is Maven 3.8.8"
    );
    assert!(supported("4.0.0").is_err());
}

#[test]
fn files_a_repository_holds_of_a_signed_module() {
    let module = Module {
        group_id: "com.acme".to_string(),
        artifact_id: "billing-api".to_string(),
        repository: Repository {
            id: "releases".to_string(),
            url: "https://repo.example/releases".to_string(),
        },
    };
    let file = |name: &str| MavenFile::parse(name, "billing-api", "1.4.0").unwrap();
    let held = tempfile::tempdir().unwrap();
    let built = tempfile::tempdir().unwrap();
    let dir = "com/acme/billing-api/1.4.0";
    let save = |root: &Path, name: &str, content: &str| {
        write(root, &format!("{dir}/{name}"), content);
        Some(root.join(dir).join(name))
    };
    for (name, content) in [
        ("billing-api-1.4.0.pom", "pom"),
        ("billing-api-1.4.0.jar", "jar"),
        ("billing-api-1.4.0.jar.asc", "signature of this build"),
        ("billing-api-1.4.0-sources.jar", "sources"),
        (
            "billing-api-1.4.0-sources.jar.asc",
            "signature of the sources",
        ),
    ] {
        write(built.path(), &format!("{dir}/{name}"), content);
    }
    let answers = vec![
        (
            file("billing-api-1.4.0.pom"),
            save(held.path(), "billing-api-1.4.0.pom", "pom"),
        ),
        (
            file("billing-api-1.4.0.jar"),
            save(held.path(), "billing-api-1.4.0.jar", "jar"),
        ),
        // A signature made earlier, of the same JAR.
        (
            file("billing-api-1.4.0.jar.asc"),
            save(
                held.path(),
                "billing-api-1.4.0.jar.asc",
                "signature of an earlier run",
            ),
        ),
        (file("billing-api-1.4.0-sources.jar"), None),
        (
            file("billing-api-1.4.0-sources.jar.asc"),
            save(
                held.path(),
                "billing-api-1.4.0-sources.jar.asc",
                "signature of other sources",
            ),
        ),
    ];
    // The SHA-1 of "pom", as repositories keep it, and a wrong one of the
    // JAR, which Maven saves when its checksum policy only warns.
    write(
        held.path(),
        &format!("{dir}/billing-api-1.4.0.pom.sha1"),
        "ACB4A94F3C944150FB89F07D87B019E224C73A27  billing-api-1.4.0.pom\n",
    );
    write(
        held.path(),
        &format!("{dir}/billing-api-1.4.0.jar.sha1"),
        "0",
    );
    let holding = super::mvn::holding(&module, "1.4.0", answers, Some(built.path())).unwrap();
    let names = |files: &[MavenFile]| files.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    // Signatures need no SHA-1 and differ whenever they are made again.
    assert!(holding.unchecked.is_empty(), "{:?}", holding.unchecked);
    assert!(holding.differing.is_empty(), "{:?}", holding.differing);
    assert_eq!(names(&holding.corrupt), ["billing-api-1.4.0.jar"]);
    assert_eq!(names(&holding.missing), ["billing-api-1.4.0-sources.jar"]);
    // Nothing confirms that a signature of a missing file signs this build's.
    assert_eq!(
        names(&holding.unconfirmed),
        ["billing-api-1.4.0-sources.jar.asc"]
    );
    assert_eq!(
        file("billing-api-1.4.0.jar.asc").signed(),
        Some("billing-api-1.4.0.jar")
    );
    assert_eq!(
        file("billing-api-1.4.0.jar.sigstore.json").signed(),
        Some("billing-api-1.4.0.jar")
    );
    assert_eq!(file("billing-api-1.4.0.jar").signed(), None);
}

#[test]
fn options_of_the_build_directory() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    write(
        root,
        "java/.mvn/maven.config",
        "--settings=my dir/settings.xml\n-Dsettings.security=.mvn/security.xml\n",
    );
    write(root, "java/billing/pom.xml", "<project/>");
    let dir = root.join("java");
    // Maven reads the nearest `.mvn` from the build's directory upward, takes
    // each of its lines as one argument, and resolves paths against the
    // build's directory.
    assert_eq!(
        Options::of(&dir, ""),
        Options {
            user: Some(dir.join("my dir/settings.xml")),
            global: None,
            security: Some(dir.join(".mvn/security.xml")),
        }
    );
    let nested = root.join("java/billing");
    assert_eq!(
        Options::of(&nested, "").user,
        Some(nested.join("my dir/settings.xml"))
    );
    // The command line comes first.
    assert_eq!(
        Options::of(&dir, "-s ci.xml").user,
        Some(dir.join("ci.xml"))
    );
    assert_eq!(Options::of(root, ""), Options::default());

    // `|` parts the command line from the lines of `maven.config`.
    let parse = |arguments: &[&str]| {
        let at = arguments
            .iter()
            .position(|a| *a == "|")
            .unwrap_or(arguments.len());
        let strings = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        let config = arguments.get(at + 1..).unwrap_or_default();
        let options = Options::parse(
            Path::new("/d"),
            &strings(&arguments[..at]),
            &strings(config),
        );
        let show = |path: Option<PathBuf>| path.map(|path| path.display().to_string());
        (
            show(options.user),
            show(options.global),
            show(options.security),
        )
    };
    let some = |path: &str| Some(path.to_string());
    for (arguments, expected) in [
        (&["-s", "s.xml"][..], (some("/d/s.xml"), None, None)),
        (&["-ss.xml"], (some("/d/s.xml"), None, None)),
        (&["--settings", "s.xml"], (some("/d/s.xml"), None, None)),
        (&["--settings=s.xml"], (some("/d/s.xml"), None, None)),
        (&["-gs", "g.xml"], (None, some("/d/g.xml"), None)),
        (&["-gsg.xml"], (None, some("/d/g.xml"), None)),
        (&["--global-settings=g.xml"], (None, some("/d/g.xml"), None)),
        (
            &["-D", "settings.security=k.xml"],
            (None, None, some("/d/k.xml")),
        ),
        (
            &["-Dsettings.security=/k.xml"],
            (None, None, some("/k.xml")),
        ),
        (
            &["--define=settings.security=k.xml"],
            (None, None, some("/d/k.xml")),
        ),
        // Settings files: the first counts, the command line's first.
        (
            &["-DskipTests", "-s", "a.xml", "-s", "b.xml"],
            (some("/d/a.xml"), None, None),
        ),
        (
            &["-sa.xml", "|", "-s", "b.xml"],
            (some("/d/a.xml"), None, None),
        ),
        (
            &["|", "-gs", "a.xml", "-gsb.xml"],
            (None, some("/d/a.xml"), None),
        ),
        // A property: the last counts, the command line's over the config's.
        (
            &["-Dsettings.security=a.xml", "-Dsettings.security=b.xml"],
            (None, None, some("/d/b.xml")),
        ),
        (
            &[
                "-Dsettings.security=a.xml",
                "|",
                "-Dsettings.security=b.xml",
            ],
            (None, None, some("/d/a.xml")),
        ),
        (
            &[
                "|",
                "-Dsettings.security=a.xml",
                "--define=settings.security=b.xml",
            ],
            (None, None, some("/d/b.xml")),
        ),
        (&["-U", "-ntp"], (None, None, None)),
    ] {
        assert_eq!(parse(arguments), expected, "{arguments:?}");
    }
}

#[test]
fn module_versions_come_from_the_unit() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    // A parent outside the unit would give the module its own version.
    write(
        root,
        "billing/billing-api/pom.xml",
        &pom(
            "  <parent>\n    <groupId>com.acme</groupId>\n    <artifactId>build</artifactId>\n    <version>1</version>\n    <relativePath>../../pom.xml</relativePath>\n  </parent>\n  <artifactId>billing-api</artifactId>\n",
        ),
    );
    let err = inspect(root, &units).unwrap_err();
    assert_eq!(err.file, "billing/billing-api/pom.xml");
    assert!(
        err.message.contains(
            "is a module of unit `billing`, but takes its version from its parent \
             `com.acme:build`, outside the unit"
        ),
        "{}",
        err.message
    );
    write(
        root,
        "billing/billing-api/pom.xml",
        &pom(
            "  <parent>\n    <groupId>com.acme</groupId>\n    <artifactId>build</artifactId>\n    <version>1</version>\n    <relativePath>../../pom.xml</relativePath>\n  </parent>\n  <artifactId>billing-api</artifactId>\n  <version>${revision}</version>\n",
        ),
    );
    inspect(root, &units).unwrap();
}

#[test]
fn unreadable_poms_fail_only_the_units_they_belong_to() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let units = repository(root);
    write(root, "fixtures/broken/pom.xml", "<project><artifactId>");
    let discovered = Maven
        .discover(&DiscoverRequest {
            root: root.to_path_buf(),
        })
        .unwrap();
    assert_eq!(discovered.units.len(), 3);
    inspect(root, &units).unwrap();
    write(root, "billing/billing-api/pom.xml", "<project><artifactId>");
    let err = inspect(root, &units).unwrap_err();
    assert_eq!(err.file, "billing/billing-api/pom.xml");
    assert!(err.message.contains("is not a POM"), "{}", err.message);
}

#[test]
fn file_names_of_a_release() {
    let parse = |name: &str| {
        MavenFile::parse(name, "billing-api", "1.4.0").map(|file| (file.classifier, file.extension))
    };
    assert_eq!(
        parse("billing-api-1.4.0.pom"),
        Some((None, "pom".to_string()))
    );
    assert_eq!(
        parse("billing-api-1.4.0.jar"),
        Some((None, "jar".to_string()))
    );
    assert_eq!(
        parse("billing-api-1.4.0-sources.jar"),
        Some((Some("sources".to_string()), "jar".to_string()))
    );
    assert_eq!(
        parse("billing-api-1.4.0-dist.tar.gz"),
        Some((Some("dist".to_string()), "tar.gz".to_string()))
    );
    assert_eq!(
        parse("billing-api-1.4.0.jar.asc"),
        Some((None, "jar.asc".to_string()))
    );
    assert_eq!(parse("billing-api-1.4.01.jar"), None);
    assert_eq!(parse("other-1.4.0.jar"), None);

    let module = Module {
        group_id: "com.acme".to_string(),
        artifact_id: "billing-api".to_string(),
        repository: Repository {
            id: "releases".to_string(),
            url: "https://repo.example/releases".to_string(),
        },
    };
    let local = tempfile::tempdir().unwrap();
    for name in [
        "billing-api-1.4.0.pom",
        "billing-api-1.4.0.jar",
        "billing-api-1.4.0.jar.sha1",
        "billing-api-1.4.0-sources.jar",
        "_remote.repositories",
        "maven-metadata-local.xml",
    ] {
        write(
            local.path(),
            &format!("com/acme/billing-api/1.4.0/{name}"),
            "",
        );
    }
    let installed = module.installed(local.path(), "1.4.0");
    let names: Vec<&str> = installed.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "billing-api-1.4.0-sources.jar",
            "billing-api-1.4.0.jar",
            "billing-api-1.4.0.pom"
        ]
    );
    let holding = Holding {
        module: &module,
        present: installed[2..].to_vec(),
        missing: installed[1..2].to_vec(),
        unchecked: vec![(installed[2].clone(), "0a1b".to_string())],
        corrupt: Vec::new(),
        differing: Vec::new(),
        unconfirmed: Vec::new(),
    };
    assert!(!holding.complete());
    assert_eq!(
        holding.describe(),
        "module `com.acme:billing-api` has billing-api-1.4.0.pom but lacks \
         billing-api-1.4.0.jar and the SHA-1 of billing-api-1.4.0.pom"
    );
    let corrupt = Holding {
        present: installed.to_vec(),
        missing: Vec::new(),
        unchecked: Vec::new(),
        corrupt: installed[1..2].to_vec(),
        ..holding
    };
    assert!(!corrupt.complete());
    assert!(
        corrupt
            .describe()
            .ends_with("but lacks a SHA-1 that matches billing-api-1.4.0.jar"),
        "{}",
        corrupt.describe()
    );
}

#[test]
fn settings_that_reach_only_the_deploy_repository() {
    let settings = r#"<?xml version="1.0"?>
<settings xmlns="http://maven.apache.org/SETTINGS/1.0.0">
  <servers>
    <server>
      <id>releases</id>
      <username>${env.DEPLOY_USER}</username>
      <password>{COQLCE6DU6GtcS5P=}</password>
    </server>
    <server><id>other</id><password>secret</password></server>
    <server><id>group</id><username>reader</username><password>${env.READER}</password></server>
  </servers>
  <proxies><proxy><id>corp</id><host>proxy.example</host><port>8080</port></proxy></proxies>
  <mirrors>
    <mirror><id>group</id><mirrorOf>*</mirrorOf><url>https://mirror.example/group</url></mirror>
  </mirrors>
  <profiles><profile><id>extra</id><repositories><repository><id>extra</id><url>https://extra.example</url></repository></repositories></profile></profiles>
  <activeProfiles><activeProfile>extra</activeProfile></activeProfiles>
</settings>
"#;
    let isolation = Isolation::parse(settings, "releases").unwrap();
    expect_test::expect![[r#"
        <?xml version="1.0" encoding="UTF-8"?>
        <settings xmlns="http://maven.apache.org/SETTINGS/1.0.0">
          <servers>
            <server>
              <id>releases</id>
              <username>${env.DEPLOY_USER}</username>
              <password>{COQLCE6DU6GtcS5P=}</password>
            </server>
            <server><id>group</id><username>reader</username><password>${env.READER}</password></server>
          </servers>
          <proxies>
            <proxy><id>corp</id><host>proxy.example</host><port>8080</port></proxy>
          </proxies>
          <mirrors>
            <mirror><id>group</id><mirrorOf>!releases,*</mirrorOf><url>https://mirror.example/group</url></mirror>
          </mirrors>
        </settings>
    "#]]
    .assert_eq(&isolation.render());
    assert!(Isolation::parse("<settings>", "releases").is_err());
}

#[test]
fn plugins_the_tests_resolve_ahead() {
    // The Maven tests resolve these plugins once into a cache, which they
    // renew when this list changes.
    let listed = include_str!("../../../tests/fixtures/maven-plugins.txt");
    for plugin in super::mvn::PLUGINS {
        assert!(listed.lines().any(|line| line == plugin), "{plugin}");
    }
}
