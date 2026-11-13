//! POM files as the Maven adapter reads them, parsed with roxmltree, and
//! edits of their properties that keep the rest of the file as written.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use roxmltree::{Document, Node};

use super::super::{AdapterError, EdgeKind, join, resolve_dir};

/// The file name of a POM.
pub const POM: &str = "pom.xml";

/// A POM of the repository, with what the adapter reads from it.
#[derive(Debug, Clone)]
pub struct Pom {
    /// The file relative to the repository root, with `/` separators.
    pub file: String,
    /// Its directory relative to the repository root; `.` for the root.
    pub dir: String,
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    pub parent: Option<Parent>,
    /// The properties it defines itself, in order.
    pub properties: Vec<(String, String)>,
    /// The files of the modules it aggregates, relative to the repository
    /// root; `None` for a module that leaves the repository.
    pub modules: Vec<(String, Option<String>)>,
    /// Its references to other projects, outside profiles.
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone)]
pub struct Parent {
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    /// The file `relativePath` names, `../pom.xml` by default; `None` when
    /// it is empty or leaves the repository.
    pub file: Option<String>,
}

/// A dependency, a managed dependency or a plugin.
#[derive(Debug, Clone)]
pub struct Reference {
    pub group_id: Option<String>,
    pub artifact_id: String,
    pub version: Option<String>,
    pub kind: EdgeKind,
    /// Whether the release build of the referring unit resolves it, so
    /// that the unit referred to is published first.
    pub published_first: bool,
    /// The element path of the reference in the POM, for messages and
    /// locations, such as `dependencies.com.acme:core`.
    pub keys: Vec<String>,
}

impl Pom {
    /// Parses the POM `file`, relative to the repository root, whose text is
    /// `text`.
    pub fn parse(file: &str, text: &str) -> Result<Pom, AdapterError> {
        let document = Document::parse(text)
            .map_err(|err| AdapterError::new(file, format!("is not a POM: {err}")))?;
        let project = document.root_element();
        if project.tag_name().name() != "project" {
            return Err(AdapterError::new(
                file,
                "is not a POM: its root element is not `project`",
            ));
        }
        let dir = file
            .rsplit_once('/')
            .map_or(".".to_string(), |(dir, _)| dir.to_string());
        let parent = child(project, "parent").map(|parent| {
            let relative =
                child(parent, "relativePath").map(|path| text_of(path).unwrap_or_default());
            let file = match relative.as_deref() {
                None => resolve_dir(&dir, "..").map(|dir| join(&dir, POM)),
                Some("") => None,
                Some(path) => resolve_dir(&dir, path).map(|resolved| {
                    if resolved.ends_with(".xml") {
                        resolved
                    } else {
                        join(&resolved, POM)
                    }
                }),
            };
            Parent {
                group_id: field(parent, "groupId"),
                artifact_id: field(parent, "artifactId"),
                version: field(parent, "version"),
                file,
            }
        });
        let properties = child(project, "properties")
            .map(|properties| {
                elements(properties)
                    .map(|property| {
                        (
                            property.tag_name().name().to_string(),
                            text_of(property).unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let modules = child(project, "modules")
            .map(|modules| {
                named(modules, "module")
                    .filter_map(text_of)
                    .map(|module| {
                        let file = resolve_dir(&dir, &module).map(|path| {
                            if path.ends_with(".xml") {
                                path
                            } else {
                                join(&path, POM)
                            }
                        });
                        (module, file)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut references = Vec::new();
        let dependencies = |node: Node, keys: &[&str], references: &mut Vec<Reference>| {
            for dependency in named(node, "dependency") {
                if let Some(reference) = dependency_reference(dependency, keys) {
                    references.push(reference);
                }
            }
        };
        let managed_dependencies = |node: Node, references: &mut Vec<Reference>| {
            let keys = ["dependencyManagement", "dependencies"];
            for dependency in named(node, "dependency") {
                if let Some(mut reference) = dependency_reference(dependency, &keys) {
                    // An imported BOM shapes the dependent's model and its
                    // consumers'; any other entry only manages a version for
                    // them, which nothing needs to build.
                    if field(dependency, "scope").as_deref() != Some("import") {
                        reference.kind = EdgeKind::Managed;
                        reference.published_first = false;
                    }
                    references.push(reference);
                }
            }
        };
        if let Some(node) = child(project, "dependencies") {
            dependencies(node, &["dependencies"], &mut references);
        }
        if let Some(node) =
            child(project, "dependencyManagement").and_then(|node| child(node, "dependencies"))
        {
            managed_dependencies(node, &mut references);
        }
        if let Some(build) = child(project, "build") {
            let plugins = [
                (child(build, "plugins"), vec!["build", "plugins"]),
                (
                    child(build, "pluginManagement").and_then(|node| child(node, "plugins")),
                    vec!["build", "pluginManagement", "plugins"],
                ),
            ];
            for (node, keys) in plugins {
                let Some(node) = node else { continue };
                for plugin in named(node, "plugin") {
                    let Some(artifact_id) = field(plugin, "artifactId") else {
                        continue;
                    };
                    let group_id = field(plugin, "groupId")
                        .or_else(|| Some("org.apache.maven.plugins".to_string()));
                    let mut plugin_keys: Vec<String> =
                        keys.iter().map(ToString::to_string).collect();
                    plugin_keys.push(coordinates(group_id.as_deref(), &artifact_id));
                    if let Some(dependencies) = child(plugin, "dependencies") {
                        for dependency in named(dependencies, "dependency") {
                            if let Some(mut reference) = dependency_reference(dependency, &[]) {
                                let mut keys = plugin_keys.clone();
                                keys.push("dependencies".to_string());
                                keys.append(&mut reference.keys);
                                reference.keys = keys;
                                reference.kind = EdgeKind::Development;
                                references.push(reference);
                            }
                        }
                    }
                    references.push(Reference {
                        group_id,
                        artifact_id,
                        version: field(plugin, "version"),
                        kind: EdgeKind::Development,
                        published_first: true,
                        keys: plugin_keys,
                    });
                }
            }
        }

        Ok(Pom {
            file: file.to_string(),
            dir,
            group_id: field(project, "groupId"),
            artifact_id: field(project, "artifactId"),
            version: field(project, "version"),
            parent,
            properties,
            modules,
            references,
        })
    }

    /// The value of the property `name` the POM defines itself.
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The POM's groupId, its own or its parent's.
    pub fn effective_group_id(&self) -> Option<&str> {
        self.group_id
            .as_deref()
            .or_else(|| self.parent.as_ref()?.group_id.as_deref())
    }
}

/// `groupId:artifactId`, or the artifactId alone without a groupId.
pub fn coordinates(group_id: Option<&str>, artifact_id: &str) -> String {
    match group_id {
        Some(group_id) => format!("{group_id}:{artifact_id}"),
        None => artifact_id.to_string(),
    }
}

/// The reference of a `dependency` element below `keys`, and its kind by
/// whether the dependent's consumers resolve it: a `compile`, `runtime` or
/// `import` dependency is a runtime edge, and a `test`, `provided` or
/// `system` dependency, or an optional one, none of which Maven passes on,
/// a development edge. The release build resolves every one.
fn dependency_reference(dependency: Node, keys: &[&str]) -> Option<Reference> {
    let artifact_id = field(dependency, "artifactId")?;
    let group_id = field(dependency, "groupId");
    let optional = field(dependency, "optional").as_deref() == Some("true");
    let kind = match field(dependency, "scope").as_deref() {
        Some("test" | "provided" | "system") => EdgeKind::Development,
        _ if optional => EdgeKind::Development,
        _ => EdgeKind::Runtime,
    };
    let mut path: Vec<String> = keys.iter().map(ToString::to_string).collect();
    path.push(coordinates(group_id.as_deref(), &artifact_id));
    Some(Reference {
        group_id,
        artifact_id,
        version: field(dependency, "version"),
        kind,
        published_first: true,
        keys: path,
    })
}

fn elements<'a, 'input>(node: Node<'a, 'input>) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children().filter(Node::is_element)
}

pub(super) fn named<'a, 'input: 'a>(
    node: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    elements(node).filter(move |child| child.tag_name().name() == name)
}

pub(super) fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    elements(node).find(|child| child.tag_name().name() == name)
}

/// The element's text, trimmed, as Maven reads it.
pub(super) fn text_of(node: Node) -> Option<String> {
    let text: String = node
        .children()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .collect();
    Some(text.trim().to_string())
}

pub(super) fn field(node: Node, name: &str) -> Option<String> {
    child(node, name).and_then(text_of)
}

/// The POMs of the repository at `root`, by file: every `pom.xml` that git
/// does not ignore, outside hidden directories and outside the `src` and
/// `target` directories of a Maven project, whose test resources and
/// build output may hold POMs of their own.
pub fn scan(root: &Path) -> Result<Scan, AdapterError> {
    let mut scan = Scan::default();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .require_git(false)
        .filter_entry(|entry| {
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            let name = entry.file_name();
            let project_dir = entry
                .path()
                .parent()
                .is_some_and(|parent| parent.join(POM).is_file());
            !(is_dir && project_dir && (name == "src" || name == "target"))
        })
        .build();
    for entry in walker {
        let entry = entry
            .map_err(|err| AdapterError::new(POM, format!("cannot search for POMs: {err}")))?;
        if entry.file_name() != POM || !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let file = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        match read(root, &file) {
            Ok(pom) => {
                scan.poms.insert(file, pom);
            }
            Err(err) => {
                scan.broken.insert(file, err);
            }
        }
    }
    Ok(scan)
}

/// The POMs of the repository, by file, and those that cannot be read,
/// which fail a command only where a unit needs them.
#[derive(Debug, Default)]
pub struct Scan {
    pub poms: BTreeMap<String, Pom>,
    pub broken: BTreeMap<String, AdapterError>,
}

/// Reads and parses the POM `file`, relative to `root`.
pub fn read(root: &Path, file: &str) -> Result<Pom, AdapterError> {
    let text = std::fs::read_to_string(root.join(file))
        .map_err(|err| AdapterError::new(file, format!("cannot read it: {err}")))?;
    Pom::parse(file, &text)
}

/// The text of the POM `file` with the properties it defines itself set to
/// new values, everything else kept as written. Fails naming a property the
/// POM does not define.
pub fn set_properties(
    file: &str,
    text: &str,
    values: &BTreeMap<String, String>,
) -> Result<String, AdapterError> {
    let document = Document::parse(text)
        .map_err(|err| AdapterError::new(file, format!("is not a POM: {err}")))?;
    let properties = child(document.root_element(), "properties");
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for (name, value) in values {
        let element = properties
            .and_then(|properties| named(properties, name).last())
            .ok_or_else(|| {
                AdapterError::new(file, format!("defines no property `{name}` to set"))
            })?;
        let value = escape(value);
        let mut children = element.children();
        let edit = match (children.next(), children.next()) {
            (Some(only), None) if only.is_text() => (only.range(), value),
            (None, _) => (element.range(), format!("<{name}>{value}</{name}>")),
            _ => {
                return Err(AdapterError::new(
                    file,
                    format!("the property `{name}` holds more than text, so it is not set"),
                ));
            }
        };
        edits.push(edit);
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut text = text.to_string();
    for (range, value) in edits {
        text.replace_range(range, &value);
    }
    Ok(text)
}

/// `value` as XML character data.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDERS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>com.acme</groupId>
    <artifactId>platform</artifactId>
    <version>7</version>
  </parent>
  <artifactId>orders</artifactId>
  <version>${revision}</version>
  <packaging>pom</packaging>
  <properties>
    <revision>0.4.1-SNAPSHOT</revision>
    <!-- the producers -->
    <billing.version>
      1.3.1-SNAPSHOT
    </billing.version>
    <ledger.version/>
  </properties>
  <modules>
    <module>orders-api</module>
    <module>../shared/orders-model.xml</module>
  </modules>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>com.acme</groupId>
        <artifactId>billing-bom</artifactId>
        <version>${billing.version}</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency>
      <groupId>com.acme</groupId>
      <artifactId>billing-api</artifactId>
    </dependency>
    <dependency>
      <groupId>com.acme</groupId>
      <artifactId>ledger</artifactId>
      <version>${ledger.version}</version>
      <scope>test</scope>
    </dependency>
    <dependency>
      <groupId>com.acme</groupId>
      <artifactId>audit</artifactId>
      <version>2.0.0</version>
      <optional>true</optional>
    </dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <artifactId>maven-surefire-plugin</artifactId>
        <dependencies>
          <dependency>
            <groupId>com.acme</groupId>
            <artifactId>test-kit</artifactId>
            <version>${ledger.version}</version>
          </dependency>
        </dependencies>
      </plugin>
    </plugins>
  </build>
  <profiles>
    <profile>
      <dependencies>
        <dependency>
          <groupId>com.acme</groupId>
          <artifactId>hidden</artifactId>
        </dependency>
      </dependencies>
    </profile>
  </profiles>
</project>
"#;

    #[test]
    fn reads_what_the_adapter_needs() {
        let pom = Pom::parse("services/orders/pom.xml", ORDERS).unwrap();
        assert_eq!(pom.dir, "services/orders");
        assert_eq!(pom.effective_group_id(), Some("com.acme"));
        assert_eq!(pom.version.as_deref(), Some("${revision}"));
        assert_eq!(
            pom.parent.as_ref().unwrap().file.as_deref(),
            Some("services/pom.xml")
        );
        assert_eq!(pom.property("billing.version"), Some("1.3.1-SNAPSHOT"));
        assert_eq!(pom.property("ledger.version"), Some(""));
        assert_eq!(
            pom.modules,
            [
                (
                    "orders-api".to_string(),
                    Some("services/orders/orders-api/pom.xml".to_string())
                ),
                (
                    "../shared/orders-model.xml".to_string(),
                    Some("services/shared/orders-model.xml".to_string())
                ),
            ]
        );
        let references: Vec<String> = pom
            .references
            .iter()
            .map(|reference| {
                format!(
                    "{} {:?} {} at {}",
                    reference.artifact_id,
                    reference.version,
                    reference.kind,
                    reference.keys.join(".")
                )
            })
            .collect();
        expect_test::expect![[r#"
            [
                "billing-api None runtime at dependencies.com.acme:billing-api",
                "ledger Some(\"${ledger.version}\") development at dependencies.com.acme:ledger",
                "audit Some(\"2.0.0\") development at dependencies.com.acme:audit",
                "billing-bom Some(\"${billing.version}\") runtime at dependencyManagement.dependencies.com.acme:billing-bom",
                "test-kit Some(\"${ledger.version}\") development at build.plugins.org.apache.maven.plugins:maven-surefire-plugin.dependencies.com.acme:test-kit",
                "maven-surefire-plugin None development at build.plugins.org.apache.maven.plugins:maven-surefire-plugin",
            ]
        "#]]
        .assert_debug_eq(&references);
    }

    #[test]
    fn properties_are_set_in_place() {
        let values = BTreeMap::from([
            ("billing.version".to_string(), "1.4.1-SNAPSHOT".to_string()),
            ("ledger.version".to_string(), "2.1.1-SNAPSHOT".to_string()),
            ("revision".to_string(), "0.4.2-SNAPSHOT".to_string()),
        ]);
        let text = set_properties("orders/pom.xml", ORDERS, &values).unwrap();
        let start = text.find("<properties>").unwrap();
        let end = text.find("</properties>").unwrap();
        expect_test::expect![[r#"
            <properties>
                <revision>0.4.2-SNAPSHOT</revision>
                <!-- the producers -->
                <billing.version>1.4.1-SNAPSHOT</billing.version>
                <ledger.version>2.1.1-SNAPSHOT</ledger.version>
              "#]]
        .assert_eq(&text[start..end]);
        assert_eq!(text[..start], ORDERS[..start]);
        assert_eq!(text[end..], ORDERS[ORDERS.find("</properties>").unwrap()..]);

        let missing = BTreeMap::from([("other.version".to_string(), "1.0.0".to_string())]);
        let err = set_properties("orders/pom.xml", ORDERS, &missing).unwrap_err();
        assert_eq!(err.file, "orders/pom.xml");
        assert!(err.message.contains("`other.version`"), "{}", err.message);
    }

    #[test]
    fn malformed_pom_names_the_file() {
        let err = Pom::parse("a/pom.xml", "<project><artifactId>a</project>").unwrap_err();
        assert_eq!(err.file, "a/pom.xml");
        let err = Pom::parse("b/pom.xml", "<settings/>").unwrap_err();
        assert!(err.message.contains("not `project`"), "{}", err.message);
    }
}
