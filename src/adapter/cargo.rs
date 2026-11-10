//! The Cargo adapter. Cargo itself, through `cargo metadata`, says which
//! packages a workspace has and where they may be published; the adapter
//! reads and writes their manifests with `toml_edit`, which keeps their
//! formatting.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use cargo_metadata::{Metadata, MetadataCommand};
use semver::{Op, Version, VersionReq};
use toml_edit::{DocumentMut, Item, TableLike, Value};

use super::{
    AcceptsRequest, Adapter, AdapterError, DiscoverRequest, DiscoveredUnit, Discovery, Edge,
    EdgeKind, FileWrite, InspectRequest, Inspection, Location, LockfileRequest, Lockfiles,
    PLAIN_OUTPUT, Publication, Publications, PublishedRequest, UnitVersion, WriteRequest, Writes,
    join, nearest_lockfiles, resolve_dir, run_tool,
};

pub struct Cargo;

/// The dependency tables of a manifest and the kind of their edges; Cargo
/// still reads the old spellings with underscores.
const DEPENDENCY_TABLES: [(&str, EdgeKind); 5] = [
    ("dependencies", EdgeKind::Runtime),
    ("build-dependencies", EdgeKind::Build),
    ("build_dependencies", EdgeKind::Build),
    ("dev-dependencies", EdgeKind::Development),
    ("dev_dependencies", EdgeKind::Development),
];

/// `cargo update --workspace` updates the lockfile's entries of the
/// workspace's own packages, as after a change of their versions, and
/// resolves other packages only when the lockfile lacks them.
const LOCKFILES: [(&str, &[&str]); 1] = [("Cargo.lock", &["cargo", "update", "--workspace"])];

impl Adapter for Cargo {
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError> {
        discover(&request.root, None)
    }

    fn inspect(&self, request: &InspectRequest) -> Result<Inspection, AdapterError> {
        let root = canonical(&request.root)?;
        let mut workspaces: Vec<Workspace> = Vec::new();
        if root.join("Cargo.toml").is_file() && !request.units.is_empty() {
            workspaces.push(Workspace::load(&root, "Cargo.toml", None)?);
        }
        for unit in &request.units {
            let covered = workspaces
                .iter()
                .any(|workspace| workspace.members.contains(&unit.path));
            if !covered {
                let manifest = join(&unit.path, "Cargo.toml");
                workspaces.push(Workspace::load(&root, &manifest, None)?);
            }
        }

        let mut units = Vec::new();
        for unit in &request.units {
            let workspace = workspaces
                .iter()
                .find(|workspace| workspace.members.contains(&unit.path))
                .ok_or_else(|| {
                    AdapterError::new(
                        join(&unit.path, "Cargo.toml"),
                        format!("unit `{}` is not a package of a Cargo workspace", unit.name),
                    )
                })?;
            let file = join(&unit.path, "Cargo.toml");
            let manifest = workspace.manifest(&file)?;
            let missing = || {
                AdapterError::new(
                    &file,
                    format!("unit `{}` needs a `package.version`", unit.name),
                )
            };
            let declared = manifest
                .get("package")
                .and_then(Item::as_table_like)
                .and_then(|package| package.get("version"))
                .ok_or_else(missing)?;
            let (version, location) = if inherits(declared) {
                let file = workspace.root_manifest.clone();
                let keys = ["workspace", "package", "version"];
                let version = string_at(workspace.manifest(&file)?, &keys).ok_or_else(|| {
                    AdapterError::new(
                        &file,
                        format!(
                            "unit `{}` inherits `workspace.package.version`, which is missing",
                            unit.name
                        ),
                    )
                })?;
                (version, Location::new(file, &keys))
            } else {
                let version = declared.as_str().ok_or_else(missing)?.to_string();
                (
                    version,
                    Location::new(file.clone(), &["package", "version"]),
                )
            };
            Version::parse(&version).map_err(|err| {
                AdapterError::new(
                    &location.file,
                    format!("`{version}` is not a semantic version: {err}"),
                )
            })?;
            units.push(UnitVersion {
                name: unit.name.clone(),
                holds_release: true,
                version,
                location,
            });
        }

        let unit_at: HashMap<&str, &str> = request
            .units
            .iter()
            .map(|unit| (unit.path.as_str(), unit.name.as_str()))
            .collect();
        let mut edges = Vec::new();
        for workspace in &workspaces {
            for member in &workspace.members {
                let file = join(member, "Cargo.toml");
                let manifest = workspace.manifest(&file)?;
                for (keys, kind, table) in dependency_tables(manifest) {
                    for (key, entry) in table.iter() {
                        let Some(dependency) =
                            workspace.dependency(member, &file, &keys, key, entry)?
                        else {
                            continue;
                        };
                        let Some(unit) = unit_at.get(dependency.dir.as_str()) else {
                            continue;
                        };
                        if dependency.dir == *member {
                            continue;
                        }
                        let kind = if kind == EdgeKind::Runtime && dependency.optional {
                            EdgeKind::Optional
                        } else {
                            kind
                        };
                        edges.push(Edge {
                            dependent_path: member.clone(),
                            dependent_unit: unit_at
                                .get(member.as_str())
                                .map(|name| name.to_string()),
                            dependency: unit.to_string(),
                            kind,
                            reference: dependency
                                .requirement
                                .clone()
                                .unwrap_or_else(|| format!("path {}", dependency.path)),
                            range: dependency.requirement,
                            follows: false,
                            location: dependency.location,
                        });
                    }
                }
            }
        }
        Ok(Inspection { units, edges })
    }

    fn accepts(&self, request: &AcceptsRequest) -> Result<bool, AdapterError> {
        let requirement = VersionReq::parse(&request.range).map_err(|err| {
            AdapterError::new(
                &request.file,
                format!(
                    "`{}` is not a Cargo version requirement: {err}",
                    request.range
                ),
            )
        })?;
        let version = Version::parse(&request.version).map_err(|err| {
            AdapterError::new(
                &request.file,
                format!("`{}` is not a version: {err}", request.version),
            )
        })?;
        Ok(requirement.matches(&version))
    }

    fn write(&self, request: &WriteRequest) -> Result<Writes, AdapterError> {
        let root = &request.root;
        let mut values: BTreeMap<Location, (String, Vec<&str>)> = BTreeMap::new();
        if !request.versions.is_empty() {
            let inspection = self.inspect(&InspectRequest {
                root: root.clone(),
                units: request.units.clone(),
            })?;
            for write in &request.versions {
                let unit = inspection
                    .units
                    .iter()
                    .find(|unit| unit.name == write.unit)
                    .ok_or_else(|| {
                        AdapterError::new("Cargo.toml", format!("unknown unit `{}`", write.unit))
                    })?;
                let entry = values
                    .entry(unit.location.clone())
                    .or_insert_with(|| (write.version.clone(), Vec::new()));
                if entry.0 != write.version {
                    return Err(AdapterError::new(
                        &unit.location.file,
                        format!(
                            "units {} and `{}` share their version through {}, so they cannot \
                             take the versions {} and {}; make them a fixed group",
                            entry
                                .1
                                .iter()
                                .map(|name| format!("`{name}`"))
                                .collect::<Vec<_>>()
                                .join(", "),
                            write.unit,
                            unit.location,
                            entry.0,
                            write.version
                        ),
                    ));
                }
                entry.1.push(&write.unit);
            }
        }
        for write in &request.references {
            let range = write.edge.range.as_deref().unwrap_or_default();
            values.insert(
                write.edge.location.clone(),
                (rewrite_requirement(range, &write.version), Vec::new()),
            );
        }

        let mut by_file: BTreeMap<&str, Vec<(&[String], &str)>> = BTreeMap::new();
        for (location, (value, _)) in &values {
            by_file
                .entry(&location.file)
                .or_default()
                .push((&location.keys, value));
        }
        let mut files = Vec::new();
        for (file, edits) in by_file {
            let text = std::fs::read_to_string(root.join(file))
                .map_err(|err| AdapterError::new(file, format!("cannot read it: {err}")))?;
            let mut document: DocumentMut = text
                .parse()
                .map_err(|err: toml_edit::TomlError| AdapterError::new(file, err.to_string()))?;
            for (keys, value) in edits {
                set_string(&mut document, keys, value).ok_or_else(|| {
                    AdapterError::new(file, format!("has no string `{}`", keys.join(".")))
                })?;
            }
            let content = document.to_string();
            if content != text {
                files.push(FileWrite {
                    path: file.to_string(),
                    content,
                });
            }
        }
        Ok(Writes { files })
    }

    fn lockfiles(&self, request: &LockfileRequest) -> Result<Lockfiles, AdapterError> {
        Ok(nearest_lockfiles(request, &LOCKFILES))
    }

    /// Asks with `cargo info`, from a throwaway package that is its own
    /// workspace, in a temporary directory under the repository root: cargo
    /// then reads the repository's configuration with its own precedence,
    /// credential providers and source replacement, without joining the
    /// repository's workspace or describing a package of it.
    fn published(&self, request: &PublishedRequest) -> Result<Publications, AdapterError> {
        let root = &request.root;
        let mut packages = Vec::new();
        for query in &request.queries {
            let file = join(&query.unit.path, "Cargo.toml");
            let metadata = metadata(root, &root.join(&file), None)?;
            let package = metadata
                .workspace_packages()
                .into_iter()
                .find(|package| {
                    package
                        .manifest_path
                        .parent()
                        .and_then(|dir| relative(root, dir.as_std_path(), &file).ok())
                        .is_some_and(|dir| dir == query.unit.path)
                })
                .ok_or_else(|| {
                    AdapterError::new(
                        &file,
                        format!("is not the package of `{}`", query.unit.name),
                    )
                })?;
            // `cargo publish` takes the only registry `publish` names, or
            // the default one, and asks for `--registry` when it names
            // several.
            let registry = match package.publish.as_deref() {
                None => None,
                Some([registry]) => Some(registry.clone()),
                Some(registries) => {
                    return Err(AdapterError::new(
                        &file,
                        format!(
                            "crate `{}` names the registries {} in `publish`, so it is unclear \
                             which one releases it; name one",
                            package.name,
                            registries
                                .iter()
                                .map(|registry| format!("`{registry}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                }
            };
            packages.push((query, file, package.name.to_string(), registry));
        }

        // Dropping the directory removes it, with the lockfile cargo
        // writes there, also when a query fails.
        let scratch = throwaway_package(root)?;
        let mut units = Vec::new();
        for (query, file, name, registry) in packages {
            let described = match &registry {
                Some(registry) => format!("the Cargo registry `{registry}`"),
                None => "the default Cargo registry".to_string(),
            };
            let info = |spec: &str| -> Result<Answer, AdapterError> {
                let mut command = std::process::Command::new("cargo");
                command.arg("info").arg(spec).current_dir(scratch.path());
                if let Some(registry) = &registry {
                    command.args(["--registry", registry]);
                }
                let output = run_tool(&mut command, "cargo", &file)?;
                let stderr = String::from_utf8_lossy(&output.stderr);
                Ok(if output.status.success() {
                    Answer::Listed(String::from_utf8_lossy(&output.stdout).into_owned())
                } else if stderr.contains(&format!("could not find `{spec}`")) {
                    Answer::NotListed
                } else {
                    Answer::Unknown(format!("`cargo info {spec}`: {}", stderr.trim()))
                })
            };
            let mut answer = Publication {
                unit: query.unit.name.clone(),
                registry: described,
                published: false,
                highest: None,
                problem: None,
            };
            match info(&format!("{name}@{}", query.version))? {
                Answer::Listed(_) => answer.published = true,
                Answer::Unknown(problem) => answer.problem = Some(problem),
                Answer::NotListed => match info(&name)? {
                    Answer::Listed(output) => {
                        answer.highest = output.lines().find_map(|line| {
                            line.strip_prefix("version: ")
                                .and_then(|rest| rest.split_whitespace().next())
                                .map(str::to_string)
                        });
                    }
                    Answer::NotListed => {}
                    Answer::Unknown(problem) => answer.problem = Some(problem),
                },
            }
            units.push(answer);
        }
        scratch.close().map_err(|err| {
            AdapterError::new(
                "Cargo.toml",
                format!("cannot remove the throwaway package of the registry queries: {err}"),
            )
        })?;
        Ok(Publications { units })
    }
}

/// What `cargo info` says about a crate or a version of it.
enum Answer {
    Listed(String),
    NotListed,
    /// Cargo could not tell, as when it is offline or denied access.
    Unknown(String),
}

/// A temporary directory under the repository root with a package that is
/// its own workspace, for cargo to run in.
fn throwaway_package(root: &Path) -> Result<tempfile::TempDir, AdapterError> {
    let error = |err: std::io::Error| {
        AdapterError::new(
            "Cargo.toml",
            format!("cannot create a throwaway package for the registry queries: {err}"),
        )
    };
    let scratch = tempfile::Builder::new()
        .prefix(".cutwater-registry-")
        .tempdir_in(root)
        .map_err(error)?;
    std::fs::create_dir(scratch.path().join("src")).map_err(error)?;
    std::fs::write(scratch.path().join("src/lib.rs"), "").map_err(error)?;
    std::fs::write(
        scratch.path().join("Cargo.toml"),
        "[package]\nname = \"cutwater-registry-query\"\nversion = \"0.0.0\"\n\
         edition = \"2021\"\npublish = false\n\n[workspace]\n",
    )
    .map_err(error)?;
    Ok(scratch)
}

impl Location {
    fn new(file: String, keys: &[&str]) -> Location {
        Location {
            file,
            keys: keys.iter().map(ToString::to_string).collect(),
        }
    }
}

/// A Cargo workspace: its root manifest and its members, with their
/// manifests parsed.
struct Workspace {
    /// The root manifest relative to the repository root.
    root_manifest: String,
    /// The members' directories relative to the repository root.
    members: Vec<String>,
    manifests: HashMap<String, DocumentMut>,
}

/// A path dependency's target and requirement.
struct PathDependency {
    path: String,
    /// The directory the path names, relative to the repository root.
    dir: String,
    requirement: Option<String>,
    optional: bool,
    location: Location,
}

impl Workspace {
    /// The workspace of the manifest `manifest`, relative to `root`, as
    /// `cargo metadata`, or the cargo at `cargo`, reports it.
    fn load(root: &Path, manifest: &str, cargo: Option<&Path>) -> Result<Workspace, AdapterError> {
        let metadata = metadata(root, &root.join(manifest), cargo)?;
        let root_manifest = relative(
            root,
            &metadata.workspace_root.as_std_path().join("Cargo.toml"),
            manifest,
        )?;
        let mut members = Vec::new();
        for package in metadata.workspace_packages() {
            let dir = package
                .manifest_path
                .parent()
                .map(|dir| dir.as_std_path().to_path_buf())
                .unwrap_or_default();
            members.push(relative(root, &dir, manifest)?);
        }
        let mut manifests = HashMap::new();
        for file in members
            .iter()
            .map(|member| join(member, "Cargo.toml"))
            .chain([root_manifest.clone()])
        {
            if manifests.contains_key(&file) {
                continue;
            }
            let text = std::fs::read_to_string(root.join(&file))
                .map_err(|err| AdapterError::new(&file, format!("cannot read it: {err}")))?;
            let document: DocumentMut = text
                .parse()
                .map_err(|err: toml_edit::TomlError| AdapterError::new(&file, err.to_string()))?;
            manifests.insert(file, document);
        }
        Ok(Workspace {
            root_manifest,
            members,
            manifests,
        })
    }

    fn manifest(&self, file: &str) -> Result<&DocumentMut, AdapterError> {
        self.manifests
            .get(file)
            .ok_or_else(|| AdapterError::new(file, "is not a manifest of the workspace"))
    }

    /// The entry `key` of the dependency table at `keys` of the member at
    /// `member`, when it is a path dependency, inherited from the
    /// workspace or not.
    fn dependency(
        &self,
        member: &str,
        file: &str,
        keys: &[String],
        key: &str,
        entry: &Item,
    ) -> Result<Option<PathDependency>, AdapterError> {
        let Some(table) = entry.as_table_like() else {
            return Ok(None);
        };
        let optional = table
            .get("optional")
            .and_then(Item::as_bool)
            .unwrap_or_default();
        let (table, base, file, mut keys) = if table.get("workspace").and_then(Item::as_bool)
            == Some(true)
        {
            let inherited = string_keys(&["workspace", "dependencies", key]);
            let Some(table) = item_at(self.manifest(&self.root_manifest)?.as_item(), &inherited)
                .and_then(Item::as_table_like)
            else {
                return Ok(None);
            };
            let base = self
                .root_manifest
                .rsplit_once('/')
                .map_or(".", |(dir, _)| dir);
            (table, base, self.root_manifest.as_str(), inherited)
        } else {
            let mut keys = keys.to_vec();
            keys.push(key.to_string());
            (table, member, file, keys)
        };
        let Some(path) = table.get("path").and_then(Item::as_str) else {
            return Ok(None);
        };
        let Some(dir) = resolve_dir(base, path) else {
            return Ok(None);
        };
        let requirement = table
            .get("version")
            .and_then(Item::as_str)
            .map(str::to_string);
        if let Some(requirement) = &requirement {
            VersionReq::parse(requirement).map_err(|err| {
                AdapterError::new(
                    file,
                    format!(
                        "`{}.version` `{requirement}` is not a version requirement: {err}",
                        keys.join(".")
                    ),
                )
            })?;
        }
        keys.push("version".to_string());
        Ok(Some(PathDependency {
            path: path.to_string(),
            dir,
            requirement,
            optional,
            location: Location {
                file: file.to_string(),
                keys,
            },
        }))
    }
}

/// The dependency tables of a manifest, top-level and target-specific, with
/// their keys and the kind of their edges.
fn dependency_tables(manifest: &DocumentMut) -> Vec<(Vec<String>, EdgeKind, &dyn TableLike)> {
    fn add<'a>(
        tables: &mut Vec<(Vec<String>, EdgeKind, &'a dyn TableLike)>,
        prefix: &[String],
        parent: &'a dyn TableLike,
    ) {
        for (name, kind) in DEPENDENCY_TABLES {
            if let Some(table) = parent.get(name).and_then(Item::as_table_like) {
                let mut keys = prefix.to_vec();
                keys.push(name.to_string());
                tables.push((keys, kind, table));
            }
        }
    }
    let mut tables = Vec::new();
    add(&mut tables, &[], manifest.as_table());
    if let Some(targets) = manifest.get("target").and_then(Item::as_table_like) {
        for (cfg, target) in targets.iter() {
            if let Some(target) = target.as_table_like() {
                add(
                    &mut tables,
                    &["target".to_string(), cfg.to_string()],
                    target,
                );
            }
        }
    }
    tables
}

/// Whether a manifest value is `{ workspace = true }`.
fn inherits(item: &Item) -> bool {
    item.as_table_like()
        .and_then(|table| table.get("workspace"))
        .and_then(Item::as_bool)
        == Some(true)
}

fn string_keys(keys: &[&str]) -> Vec<String> {
    keys.iter().map(ToString::to_string).collect()
}

fn item_at<'a>(item: &'a Item, keys: &[String]) -> Option<&'a Item> {
    keys.iter()
        .try_fold(item, |item, key| item.as_table_like()?.get(key))
}

fn string_at(document: &DocumentMut, keys: &[&str]) -> Option<String> {
    item_at(document.as_item(), &string_keys(keys))?
        .as_str()
        .map(str::to_string)
}

/// Replaces the string at `keys` with `value`, keeping its surroundings;
/// `None` when there is no string there.
fn set_string(document: &mut DocumentMut, keys: &[String], value: &str) -> Option<()> {
    let item = keys.iter().try_fold(document.as_item_mut(), |item, key| {
        item.as_table_like_mut()?.get_mut(key)
    })?;
    let old = item.as_value_mut()?;
    old.as_str()?;
    let decor = old.decor().clone();
    let mut new = Value::from(value);
    *new.decor_mut() = decor;
    *old = new;
    Some(())
}

/// A requirement like `requirement` that matches `version`, keeping its
/// operator: `^`, `~`, `=`, `>`, `>=` and a bare requirement stay, one
/// bounded below and above becomes `>=<version>, <next major>`, and any
/// other becomes the bare version.
fn rewrite_requirement(requirement: &str, version: &str) -> String {
    let comparators = VersionReq::parse(requirement)
        .map(|requirement| requirement.comparators)
        .unwrap_or_default();
    let candidate = match comparators.as_slice() {
        [single] if single.op != Op::Wildcard => {
            let operator: String = requirement
                .trim()
                .chars()
                .take_while(|c| "^~=<>".contains(*c))
                .collect();
            format!("{operator}{version}")
        }
        [low, high]
            if matches!(low.op, Op::Greater | Op::GreaterEq)
                && matches!(high.op, Op::Less | Op::LessEq) =>
        {
            let next_major = Version::parse(version)
                .map(|version| version.major + 1)
                .unwrap_or_default();
            format!(">={version}, <{next_major}.0.0")
        }
        _ => version.to_string(),
    };
    let matches = VersionReq::parse(&candidate)
        .ok()
        .zip(Version::parse(version).ok())
        .is_some_and(|(requirement, version)| requirement.matches(&version));
    if matches {
        candidate
    } else {
        version.to_string()
    }
}

/// The publishable members of the workspace at `root`, asking `cargo`, or
/// the cargo `CARGO` names, when `cargo` is `None`.
fn discover(root: &Path, cargo: Option<&Path>) -> Result<Discovery, AdapterError> {
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(Discovery::default());
    }
    let metadata = metadata(root, &manifest, cargo)?;
    let mut units = Vec::new();
    for package in metadata.workspace_packages() {
        // Cargo reports `publish = false` as an empty list of registries.
        if package
            .publish
            .as_ref()
            .is_some_and(|registries| registries.is_empty())
        {
            continue;
        }
        let dir = package
            .manifest_path
            .parent()
            .map(|dir| dir.as_std_path().to_path_buf())
            .unwrap_or_default();
        units.push(DiscoveredUnit {
            name: package.name.to_string(),
            path: relative(root, &dir, "Cargo.toml")?,
        });
    }
    units.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.name.cmp(&b.name)));
    Ok(Discovery { units })
}

/// `cargo metadata` of the workspace of `manifest`, run in `root`.
fn metadata(root: &Path, manifest: &Path, cargo: Option<&Path>) -> Result<Metadata, AdapterError> {
    let file = manifest
        .strip_prefix(root)
        .map(|file| file.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| "Cargo.toml".to_string());
    let mut command = MetadataCommand::new();
    command
        .manifest_path(manifest)
        .current_dir(root)
        .no_deps()
        // Reading manifests never needs the network.
        .other_options(vec!["--offline".to_string()]);
    for (key, value) in PLAIN_OUTPUT {
        command.env(key, value);
    }
    if let Some(cargo) = cargo {
        command.cargo_path(cargo);
    }
    command.exec().map_err(|err| match err {
        cargo_metadata::Error::Io(err) if err.kind() == std::io::ErrorKind::NotFound => {
            AdapterError::new(
                file,
                "Cargo units need `cargo metadata`, but cargo is not on the PATH; install cargo \
                 or turn Cargo discovery off",
            )
        }
        cargo_metadata::Error::CargoMetadata { stderr } => {
            AdapterError::new(file, format!("`cargo metadata` failed: {}", stderr.trim()))
        }
        err => AdapterError::new(file, format!("`cargo metadata` failed: {err}")),
    })
}

/// `path` relative to `root`, with `/` separators; `.` for the root.
fn relative(root: &Path, path: &Path, file: &str) -> Result<String, AdapterError> {
    let root = canonical(root)?;
    let path = canonical(path)?;
    let Ok(relative) = path.strip_prefix(&root) else {
        return Err(AdapterError::new(
            file,
            format!(
                "the workspace reaches {}, outside the repository",
                path.display()
            ),
        ));
    };
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    Ok(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

fn canonical(path: &Path) -> Result<PathBuf, AdapterError> {
    std::fs::canonicalize(path).map_err(|err| {
        AdapterError::new(
            "Cargo.toml",
            format!("cannot resolve {}: {err}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{ReferenceWrite, UnitRef, VersionWrite};

    fn write(root: &Path, file: &str, content: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A crate in `dir` with the extra manifest lines `extra`.
    fn package(root: &Path, dir: &str, name: &str, extra: &str) {
        let prefix = if dir == "." {
            String::new()
        } else {
            format!("{dir}/")
        };
        write(
            root,
            &format!("{prefix}Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{extra}"
            ),
        );
        write(root, &format!("{prefix}src/lib.rs"), "");
    }

    fn discover(root: &Path) -> Vec<(String, String)> {
        super::discover(root, None)
            .unwrap()
            .units
            .into_iter()
            .map(|unit| (unit.name, unit.path))
            .collect()
    }

    fn units(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, path)| (name.to_string(), path.to_string()))
            .collect()
    }

    #[test]
    fn workspace_members_except_unpublished_ones() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\", \"tools/xtask\"]\nexclude = [\"crates/scratch\"]\n\
             [workspace.package]\npublish = false\n",
        );
        package(root, "crates/core", "core", "");
        package(
            root,
            "crates/billing",
            "billing",
            "publish = [\"internal\"]\n",
        );
        package(
            root,
            "crates/inherits",
            "inherits",
            "publish.workspace = true\n",
        );
        package(root, "crates/scratch", "scratch", "[workspace]\n");
        write(root, "crates/README.md", "not a crate");
        package(root, "tools/xtask", "xtask", "publish = false\n");
        assert_eq!(
            discover(root),
            units(&[("billing", "crates/billing"), ("core", "crates/core")])
        );
    }

    #[test]
    fn root_package_and_its_path_dependencies_are_members() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        package(
            root,
            ".",
            "app",
            "[dependencies]\ncore = { path = \"libs/core\" }\n[workspace]\n",
        );
        package(root, "libs/core", "core", "");
        assert_eq!(
            discover(root),
            units(&[("app", "."), ("core", "libs/core")])
        );
    }

    #[test]
    fn single_crate() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        package(root, ".", "billing", "");
        package(root, "examples/demo", "demo", "");
        assert_eq!(discover(root), units(&[("billing", ".")]));
    }

    #[test]
    fn cargo_errors_name_the_manifest() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(root, "Cargo.toml", "[workspace]\nmembers = [\"core\"]\n");
        let err = super::discover(root, None).unwrap_err();
        assert_eq!(err.file, "Cargo.toml");
        assert!(
            err.message.contains("`cargo metadata` failed"),
            "{}",
            err.message
        );
    }

    #[test]
    fn missing_cargo_is_named() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        package(root, ".", "billing", "");
        let err = super::discover(root, Some(&root.join("no-cargo-here"))).unwrap_err();
        assert!(
            err.message.contains("cargo is not on the PATH"),
            "{}",
            err.message
        );
    }

    fn unit(name: &str, path: &str) -> UnitRef {
        UnitRef {
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    /// A workspace whose crates inherit its version, and whose `api` uses
    /// `core` in every way.
    fn workspace(root: &Path) -> Vec<UnitRef> {
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\n\
             version = \"1.2.0\" # shared\nedition = \"2024\"\n\n[workspace.dependencies]\n\
             core = { path = \"crates/core\", version = \"1.2\" }\n",
        );
        package(root, "crates/core", "core", "");
        write(
            root,
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        write(
            root,
            "crates/api/Cargo.toml",
            "[package]\nname = \"api\"\nversion = \"0.3.0\"\nedition = \"2024\"\n\n\
             [dependencies]\ncore = { workspace = true, optional = true }\n\n\
             [build-dependencies]\nbuild-core = { package = \"core\", path = \"../core\", version = \"^1.2.0\" }\n\n\
             [target.'cfg(unix)'.dependencies.core]\npath = \"../core\"\nversion = \">=1.0, <2.0\"\n\n\
             [dev-dependencies]\ncore = { path = \"../core\" }\n",
        );
        write(root, "crates/api/src/lib.rs", "");
        package(
            root,
            "crates/tool",
            "tool",
            "publish = false\n[dependencies]\ncore = { path = \"../core\", version = \"~1.2\" }\n",
        );
        vec![unit("api", "crates/api"), unit("core", "crates/core")]
    }

    fn inspect(root: &Path, units: Vec<UnitRef>) -> Inspection {
        Cargo
            .inspect(&InspectRequest {
                root: root.to_path_buf(),
                units,
            })
            .unwrap()
    }

    #[test]
    fn versions_and_edges() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let inspection = inspect(root, workspace(root));
        let versions: Vec<String> = inspection
            .units
            .iter()
            .map(|unit| format!("{} {} at {}", unit.name, unit.version, unit.location))
            .collect();
        assert_eq!(
            versions,
            [
                "api 0.3.0 at crates/api/Cargo.toml `package.version`",
                "core 1.2.0 at Cargo.toml `workspace.package.version`",
            ]
        );
        let edges: Vec<String> = inspection
            .edges
            .iter()
            .map(|edge| {
                format!(
                    "{} ({:?}) -> {} {}: {:?} at {}",
                    edge.dependent_path,
                    edge.dependent_unit,
                    edge.dependency,
                    edge.kind,
                    edge.range,
                    edge.location
                )
            })
            .collect();
        expect_test::expect![[r#"
            [
                "crates/api (Some(\"api\")) -> core optional: Some(\"1.2\") at Cargo.toml `workspace.dependencies.core.version`",
                "crates/api (Some(\"api\")) -> core build: Some(\"^1.2.0\") at crates/api/Cargo.toml `build-dependencies.build-core.version`",
                "crates/api (Some(\"api\")) -> core development: None at crates/api/Cargo.toml `dev-dependencies.core.version`",
                "crates/api (Some(\"api\")) -> core runtime: Some(\">=1.0, <2.0\") at crates/api/Cargo.toml `target.cfg(unix).dependencies.core.version`",
                "crates/tool (None) -> core runtime: Some(\"~1.2\") at crates/tool/Cargo.toml `dependencies.core.version`",
            ]
        "#]].assert_debug_eq(&edges);
    }

    #[test]
    fn writes_keep_the_manifests() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let units = workspace(root);
        let inspection = inspect(root, units.clone());
        let references = inspection
            .edges
            .into_iter()
            .filter(|edge| edge.range.is_some())
            .map(|edge| ReferenceWrite {
                edge,
                version: "2.0.0".to_string(),
            })
            .collect();
        let writes = Cargo
            .write(&WriteRequest {
                root: root.to_path_buf(),
                units,
                versions: vec![
                    VersionWrite {
                        unit: "core".to_string(),
                        version: "2.0.0".to_string(),
                    },
                    VersionWrite {
                        unit: "api".to_string(),
                        version: "0.3.1".to_string(),
                    },
                ],
                references,
            })
            .unwrap();
        let text: String = writes
            .files
            .iter()
            .map(|file| format!("--- {}\n{}", file.path, file.content))
            .collect();
        expect_test::expect![[r#"
            --- Cargo.toml
            [workspace]
            members = ["crates/*"]
            resolver = "3"

            [workspace.package]
            version = "2.0.0" # shared
            edition = "2024"

            [workspace.dependencies]
            core = { path = "crates/core", version = "2.0.0" }
            --- crates/api/Cargo.toml
            [package]
            name = "api"
            version = "0.3.1"
            edition = "2024"

            [dependencies]
            core = { workspace = true, optional = true }

            [build-dependencies]
            build-core = { package = "core", path = "../core", version = "^2.0.0" }

            [target.'cfg(unix)'.dependencies.core]
            path = "../core"
            version = ">=2.0.0, <3.0.0"

            [dev-dependencies]
            core = { path = "../core" }
            --- crates/tool/Cargo.toml
            [package]
            name = "tool"
            version = "0.1.0"
            edition = "2024"
            publish = false
            [dependencies]
            core = { path = "../core", version = "~2.0.0" }
        "#]]
        .assert_eq(&text);
    }

    #[test]
    fn units_sharing_a_version_need_the_same_one() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        workspace(root);
        write(
            root,
            "crates/api/Cargo.toml",
            "[package]\nname = \"api\"\nversion.workspace = true\nedition = \"2024\"\n",
        );
        let err = Cargo
            .write(&WriteRequest {
                root: root.to_path_buf(),
                units: vec![unit("api", "crates/api"), unit("core", "crates/core")],
                versions: vec![
                    VersionWrite {
                        unit: "core".to_string(),
                        version: "2.0.0".to_string(),
                    },
                    VersionWrite {
                        unit: "api".to_string(),
                        version: "1.2.1".to_string(),
                    },
                ],
                references: Vec::new(),
            })
            .unwrap_err();
        assert!(
            err.message
                .contains("share their version through Cargo.toml `workspace.package.version`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn requirements() {
        let accepts = |range: &str, version: &str| {
            Cargo
                .accepts(&AcceptsRequest {
                    range: range.to_string(),
                    version: version.to_string(),
                    file: "Cargo.toml".to_string(),
                })
                .unwrap()
        };
        assert!(accepts("1.2", "1.9.0"));
        assert!(!accepts("1.2", "2.0.0"));
        assert!(!accepts("1.2", "1.5.0-beta.0"));
        assert!(accepts("^1.5.0-beta.0", "1.5.0-beta.1"));
        for (requirement, rewritten) in [
            ("1.2", "2.0.0"),
            ("^1.2", "^2.0.0"),
            ("~1.2", "~2.0.0"),
            ("=1.2.0", "=2.0.0"),
            (">=1.0, <2.0", ">=2.0.0, <3.0.0"),
            ("1.*", "2.0.0"),
            ("<2.0", "2.0.0"),
        ] {
            assert_eq!(
                rewrite_requirement(requirement, "2.0.0"),
                rewritten,
                "{requirement}"
            );
        }
    }
}
