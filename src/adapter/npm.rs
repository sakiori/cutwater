//! The npm adapter.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::CstRootNode;
use nodejs_semver::{Range, Version};
use serde::Deserialize;
use serde_json::Value;

use super::pnpm::{PNPM_WORKSPACE, PnpmWorkspace, set_catalog_entry};
use super::walk::{PNPM_MANIFESTS, npm_dirs, pnpm_dirs};
use super::{
    AcceptsRequest, Adapter, AdapterError, DiscoverRequest, DiscoveredUnit, Discovery, Edge,
    EdgeKind, FileWrite, InspectRequest, Inspection, Location, LockfileRequest, Lockfiles,
    Publication, Publications, PublishedRequest, UnitVersion, WriteRequest, Writes, join,
    nearest_lockfiles, publication, run_tool,
};

pub struct Npm;

#[derive(Deserialize)]
struct PackageJson {
    name: Option<String>,
    #[serde(default)]
    private: Value,
    workspaces: Option<Value>,
}

/// The dependency tables of a `package.json` and the kind of their edges.
const DEPENDENCY_TABLES: [(&str, EdgeKind); 4] = [
    ("dependencies", EdgeKind::Runtime),
    ("peerDependencies", EdgeKind::Peer),
    ("optionalDependencies", EdgeKind::Optional),
    ("devDependencies", EdgeKind::Development),
];

/// The lockfiles of npm and pnpm, with the commands that refresh them
/// without installing anything or running scripts. In CI pnpm would refuse
/// to change its lockfile without `--no-frozen-lockfile`.
const LOCKFILES: [(&str, &[&str]); 3] = [
    (
        "pnpm-lock.yaml",
        &[
            "pnpm",
            "install",
            "--lockfile-only",
            "--no-frozen-lockfile",
            "--ignore-scripts",
        ],
    ),
    (
        "package-lock.json",
        &[
            "npm",
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
    ),
    (
        "npm-shrinkwrap.json",
        &[
            "npm",
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
    ),
];

impl Adapter for Npm {
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError> {
        let root = &request.root;
        let mut units = Vec::new();
        for dir in workspace_dirs(root)? {
            let manifest = package_json(root, &dir)?;
            // npm publishes neither a private package nor one without a name.
            if truthy(&manifest.private) {
                continue;
            }
            if let Some(name) = manifest.name {
                units.push(DiscoveredUnit { name, path: dir });
            }
        }
        Ok(Discovery { units })
    }

    fn inspect(&self, request: &InspectRequest) -> Result<Inspection, AdapterError> {
        let root = &request.root;
        let mut dirs = workspace_dirs(root)?;
        dirs.extend(request.units.iter().map(|unit| unit.path.clone()));
        let mut manifests = BTreeMap::new();
        for dir in dirs {
            let file = join(&dir, "package.json");
            let manifest: Value = serde_json::from_str(&read(root, &file)?)
                .map_err(|err| AdapterError::new(&file, err.to_string()))?;
            manifests.insert(dir, manifest);
        }

        let mut units = Vec::new();
        // The units by package name, with their versions.
        let mut by_package: HashMap<&str, (&str, &str, Version)> = HashMap::new();
        for unit in &request.units {
            let file = join(&unit.path, "package.json");
            let manifest = &manifests[&unit.path];
            let field = |key: &str| {
                manifest.get(key).and_then(Value::as_str).ok_or_else(|| {
                    AdapterError::new(
                        &file,
                        format!(
                            "unit `{}` needs a `{key}` string in its package.json",
                            unit.name
                        ),
                    )
                })
            };
            let package = field("name")?;
            let version = field("version")?;
            let parsed = strict_version(version).ok_or_else(|| {
                AdapterError::new(
                    &file,
                    format!("`version` `{version}` is not a semantic version"),
                )
            })?;
            if let Some((other, _, _)) =
                by_package.insert(package, (&unit.name, &unit.path, parsed))
            {
                return Err(AdapterError::new(
                    &file,
                    format!(
                        "units `{other}` and `{}` are both the package `{package}`",
                        unit.name
                    ),
                ));
            }
            units.push(UnitVersion {
                name: unit.name.clone(),
                holds_release: true,
                version: version.to_string(),
                location: Location {
                    file,
                    keys: vec!["version".to_string()],
                },
            });
        }

        let catalogs = PnpmWorkspace::load(root)?;
        let unit_at: HashMap<&str, &str> = request
            .units
            .iter()
            .map(|unit| (unit.path.as_str(), unit.name.as_str()))
            .collect();
        let mut edges = Vec::new();
        for (dir, manifest) in &manifests {
            let file = join(dir, "package.json");
            for (table, kind) in DEPENDENCY_TABLES {
                let Some(dependencies) = manifest.get(table).and_then(Value::as_object) else {
                    continue;
                };
                for (package, reference) in dependencies {
                    let Some((unit, unit_dir, version)) = by_package.get(package.as_str()) else {
                        continue;
                    };
                    let Some(reference) = reference.as_str() else {
                        return Err(AdapterError::new(
                            &file,
                            format!("`{table}.{package}` is not a string"),
                        ));
                    };
                    if unit_dir == dir {
                        continue;
                    }
                    let location = Location {
                        file: file.clone(),
                        keys: vec![table.to_string(), package.clone()],
                    };
                    let Some(resolved) =
                        resolve(reference, version, location, package, catalogs.as_ref()).map_err(
                            |problem| {
                                AdapterError::new(&file, format!("`{table}.{package}`: {problem}"))
                            },
                        )?
                    else {
                        continue;
                    };
                    edges.push(Edge {
                        dependent_path: dir.clone(),
                        dependent_unit: unit_at.get(dir.as_str()).map(|name| name.to_string()),
                        dependency: unit.to_string(),
                        kind,
                        reference: reference.to_string(),
                        range: Some(resolved.range),
                        follows: resolved.follows,
                        location: resolved.location,
                    });
                }
            }
        }
        Ok(Inspection { units, edges })
    }

    fn accepts(&self, request: &AcceptsRequest) -> Result<bool, AdapterError> {
        let range = Range::parse(&request.range).map_err(|err| {
            AdapterError::new(
                "",
                format!("`{}` is not an npm range: {err}", request.range),
            )
        })?;
        let version = Version::parse(&request.version).map_err(|err| {
            AdapterError::new(
                &request.file,
                format!("`{}` is not a version: {err}", request.version),
            )
        })?;
        Ok(range.satisfies(&version))
    }

    fn write(&self, request: &WriteRequest) -> Result<Writes, AdapterError> {
        let root = &request.root;
        // The new value of each location, by file.
        let mut edits: BTreeMap<String, BTreeMap<Vec<String>, String>> = BTreeMap::new();
        for write in &request.versions {
            let unit = request
                .units
                .iter()
                .find(|unit| unit.name == write.unit)
                .ok_or_else(|| {
                    AdapterError::new("package.json", format!("unknown unit `{}`", write.unit))
                })?;
            edits
                .entry(join(&unit.path, "package.json"))
                .or_default()
                .insert(vec!["version".to_string()], write.version.clone());
        }
        for write in &request.references {
            let edge = &write.edge;
            let range = edge.range.as_deref().unwrap_or_default();
            let new_range = rewrite_range(range, &write.version);
            // A catalog entry holds the range itself; a package.json entry
            // the reference, which may carry the `workspace:` protocol.
            let value = if edge.location.file == PNPM_WORKSPACE {
                new_range
            } else if edge.reference.starts_with("workspace:") {
                format!("workspace:{new_range}")
            } else {
                new_range
            };
            edits
                .entry(edge.location.file.clone())
                .or_default()
                .insert(edge.location.keys.clone(), value);
        }

        let mut files = Vec::new();
        for (file, values) in edits {
            let text = read(root, &file)?;
            let content = if file == PNPM_WORKSPACE {
                let mut content = text.clone();
                for (keys, value) in &values {
                    content = set_catalog_entry(&content, keys, value)?;
                }
                content
            } else {
                set_json_strings(&file, &text, &values)?
            };
            if content != text {
                files.push(FileWrite {
                    path: file,
                    content,
                });
            }
        }
        Ok(Writes { files })
    }

    fn lockfiles(&self, request: &LockfileRequest) -> Result<Lockfiles, AdapterError> {
        Ok(nearest_lockfiles(request, &LOCKFILES))
    }

    fn published(&self, request: &PublishedRequest) -> Result<Publications, AdapterError> {
        let root = &request.root;
        let mut units = Vec::new();
        for query in &request.queries {
            let file = join(&query.unit.path, "package.json");
            let manifest: Value = serde_json::from_str(&read(root, &file)?)
                .map_err(|err| AdapterError::new(&file, err.to_string()))?;
            let name = manifest
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| AdapterError::new(&file, "has no `name` string"))?;
            let registry = manifest
                .pointer("/publishConfig/registry")
                .and_then(Value::as_str);
            let mut command = std::process::Command::new("npm");
            command
                .args(["view", name, "versions", "--json"])
                .current_dir(root);
            if let Some(registry) = registry {
                command.arg(format!("--registry={registry}"));
                // npm takes a scoped package's registry from the scope's
                // setting, which outranks `--registry`.
                if let Some((scope, _)) = name.split_once('/').filter(|_| name.starts_with('@')) {
                    command.arg(format!("--{scope}:registry={registry}"));
                }
            }
            let output = run_tool(&mut command, "npm", &file)?;
            let registry = match registry {
                Some(registry) => format!("the npm registry {registry}"),
                None => "the npm registry the environment configures".to_string(),
            };
            units.push(match npm_versions(&output) {
                Ok(versions) => publication(&query.unit.name, &query.version, registry, &versions),
                Err(problem) => Publication {
                    problem: Some(format!("`npm view {name} versions`: {problem}")),
                    ..publication(&query.unit.name, &query.version, registry, &[])
                },
            });
        }
        Ok(Publications { units })
    }
}

/// The versions `npm view <name> versions --json` reports: a list, or a
/// single version as a string; none when npm answers E404, as it does for
/// a package the registry does not hold.
fn npm_versions(output: &std::process::Output) -> Result<Vec<String>, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    if !output.status.success() {
        let code = value
            .as_ref()
            .and_then(|value| value.pointer("/error/code"))
            .and_then(Value::as_str);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if code == Some("E404") || stderr.contains("code E404") {
            return Ok(Vec::new());
        }
        let summary = value
            .as_ref()
            .and_then(|value| value.pointer("/error/summary"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| stderr.trim().to_string());
        return Err(summary);
    }
    match value {
        Some(Value::Array(versions)) => Ok(versions
            .into_iter()
            .filter_map(|version| version.as_str().map(str::to_string))
            .collect()),
        Some(Value::String(version)) => Ok(vec![version]),
        // npm prints nothing for a package without versions.
        None if stdout.trim().is_empty() => Ok(Vec::new()),
        _ => Err(format!("unexpected output: {}", stdout.trim())),
    }
}

/// A reference resolved to the range the dependent's last release accepts.
struct Resolved {
    range: String,
    follows: bool,
    location: Location,
}

/// The range that `reference`, a dependent's reference to `package`, whose
/// current version is `version`, gives the published dependent: pnpm turns
/// `workspace:*` into the exact version and `workspace:^` and `workspace:~`
/// into a caret or tilde range of it, and takes `catalog:` ranges from
/// `pnpm-workspace.yaml`. `None` for a reference that is no range, such as
/// a tag, an alias, a path or a URL.
fn resolve(
    reference: &str,
    version: &Version,
    location: Location,
    package: &str,
    catalogs: Option<&PnpmWorkspace>,
) -> Result<Option<Resolved>, String> {
    if let Some(rest) = reference.strip_prefix("workspace:") {
        let (range, follows) = match rest {
            "*" => (version.to_string(), true),
            "^" | "~" => (format!("{rest}{version}"), true),
            range if Range::parse(range).is_ok() => (range.to_string(), false),
            // A relative path to the package or an alias: pnpm publishes
            // the exact version.
            _ => (version.to_string(), true),
        };
        return Ok(Some(Resolved {
            range,
            follows,
            location,
        }));
    }
    if let Some(name) = reference.strip_prefix("catalog:") {
        let Some(workspace) = catalogs else {
            return Err(format!(
                "`{reference}` refers to a pnpm catalog, but there is no {PNPM_WORKSPACE}"
            ));
        };
        let entry = workspace.catalog_entry(name.trim(), package)?;
        if Range::parse(&entry.range).is_err() {
            return Err(format!(
                "the catalog range `{}` of `{package}` is not an npm range",
                entry.range
            ));
        }
        return Ok(Some(Resolved {
            range: entry.range,
            follows: false,
            location: Location {
                file: PNPM_WORKSPACE.to_string(),
                keys: entry.keys,
            },
        }));
    }
    Ok(Range::parse(reference).is_ok().then(|| Resolved {
        range: reference.to_string(),
        follows: false,
        location,
    }))
}

/// A range like `range` that accepts `version`, keeping its operator as
/// Changesets does: `^`, `~`, `>=`, `<=` and `>` stay, a range bounded
/// below and above becomes `>=<version> <next major>`, and any other range
/// becomes the exact version.
fn rewrite_range(range: &str, version: &str) -> String {
    let range = range.trim();
    let tokens: Vec<&str> = range.split_whitespace().collect();
    let bounded = matches!(tokens.as_slice(), [low, high]
        if low.starts_with('>') && high.starts_with('<') && Range::parse(range).is_ok());
    let candidate = if bounded {
        let parsed = Version::parse(version).ok();
        let next_major = parsed
            .map(|version| version.major() + 1)
            .unwrap_or_default();
        format!(">={version} <{next_major}.0.0")
    } else {
        let operator = ["^", "~", ">=", "<=", ">"]
            .into_iter()
            .find(|operator| range.starts_with(operator))
            .unwrap_or_default();
        format!("{operator}{version}")
    };
    let accepted = Range::parse(&candidate)
        .ok()
        .zip(Version::parse(version).ok())
        .is_some_and(|(range, version)| range.satisfies(&version));
    if accepted {
        candidate
    } else {
        version.to_string()
    }
}

/// A version as npm accepts it in `package.json`'s `version`, which is
/// strict semantic versioning.
fn strict_version(version: &str) -> Option<Version> {
    semver::Version::parse(version).ok()?;
    Version::parse(version).ok()
}

/// `text`, a `package.json`, with the string values at each path of keys
/// replaced, keeping everything else.
fn set_json_strings(
    file: &str,
    text: &str,
    values: &BTreeMap<Vec<String>, String>,
) -> Result<String, AdapterError> {
    let options = ParseOptions {
        allow_comments: false,
        allow_loose_object_property_names: false,
        allow_trailing_commas: false,
        ..ParseOptions::default()
    };
    let root = CstRootNode::parse(text, &options)
        .map_err(|err| AdapterError::new(file, err.to_string()))?;
    for (keys, value) in values {
        let missing = || AdapterError::new(file, format!("has no string `{}`", keys.join(".")));
        let (last, parents) = keys.split_last().ok_or_else(missing)?;
        let mut object = root.object_value().ok_or_else(missing)?;
        for key in parents {
            object = object.object_value(key).ok_or_else(missing)?;
        }
        let literal = object
            .get(last)
            .and_then(|property| property.value())
            .and_then(|value| value.as_string_lit())
            .ok_or_else(missing)?;
        literal.set_raw_value(serde_json::to_string(value).expect("a string serializes"));
    }
    Ok(root.to_string())
}

/// The directories of the packages of the repository's workspace: a pnpm
/// workspace, an npm workspace or a single package at the root.
fn workspace_dirs(root: &Path) -> Result<BTreeSet<String>, AdapterError> {
    for (lockfile, manager) in [
        ("yarn.lock", "Yarn"),
        ("bun.lock", "Bun"),
        ("bun.lockb", "Bun"),
    ] {
        if root.join(lockfile).is_file() {
            return Err(AdapterError::new(
                lockfile,
                format!(
                    "the workspace is managed with {manager}, which cutwater does not support; \
                     it supports npm and pnpm"
                ),
            ));
        }
    }
    let mut dirs = BTreeSet::new();
    if let Some(workspace) = PnpmWorkspace::load(root)? {
        // Without `packages`, pnpm takes every package below the root.
        let patterns = workspace
            .packages
            .unwrap_or_else(|| vec![".".to_string(), "**".to_string()]);
        // Older pnpm reads the setting from .npmrc only; newer pnpm prefers
        // pnpm-workspace.yaml.
        let modules_dir = match workspace.modules_dir {
            Some(dir) => Some(dir),
            None => npmrc_modules_dir(root)?,
        };
        let mut found = pnpm_dirs(root, &patterns, modules_dir.as_deref(), PNPM_WORKSPACE)?;
        // pnpm always counts the root's package as a workspace project.
        if let Some(manifest) = PNPM_MANIFESTS
            .into_iter()
            .find(|manifest| root.join(manifest).is_file())
        {
            found.insert(".".to_string(), manifest);
        }
        for (dir, manifest) in found {
            if manifest != "package.json" {
                return Err(AdapterError::new(
                    join(&dir, manifest),
                    format!(
                        "pnpm reads this package's manifest from {manifest}, but cutwater \
                         reads only package.json; convert it to package.json or exclude the \
                         package from the workspace"
                    ),
                ));
            }
            dirs.insert(dir);
        }
    } else if root.join("package.json").is_file() {
        let manifest = package_json(root, ".")?;
        if let Some(workspaces) = manifest.workspaces {
            dirs = npm_dirs(root, &workspace_patterns(workspaces)?, "package.json")?;
        }
        dirs.insert(".".to_string());
    }
    Ok(dirs)
}

/// The `modules-dir` setting of the root `.npmrc`.
fn npmrc_modules_dir(root: &Path) -> Result<Option<String>, AdapterError> {
    const NPMRC: &str = ".npmrc";
    if !root.join(NPMRC).is_file() {
        return Ok(None);
    }
    let text = read(root, NPMRC)?;
    let mut ini = configparser::ini::Ini::new_cs();
    // Settings before any section belong to npm's top level.
    ini.set_default_section("");
    ini.read(text)
        .map_err(|err| AdapterError::new(NPMRC, err))?;
    // npm's ini reader unquotes a quoted value.
    Ok(ini.get("", "modules-dir").map(|value| {
        let unquoted = ['"', '\''].into_iter().find_map(|quote| {
            value
                .strip_prefix(quote)
                .and_then(|rest| rest.strip_suffix(quote))
        });
        unquoted.unwrap_or(&value).to_string()
    }))
}

fn read(root: &Path, file: &str) -> Result<String, AdapterError> {
    std::fs::read_to_string(root.join(file))
        .map_err(|err| AdapterError::new(file, format!("cannot read it: {err}")))
}

fn package_json(root: &Path, dir: &str) -> Result<PackageJson, AdapterError> {
    let file = join(dir, "package.json");
    let text = read(root, &file)?;
    serde_json::from_str(&text).map_err(|err| AdapterError::new(file, err.to_string()))
}

/// npm's `workspaces`: a list of patterns, or Yarn's form, an object whose
/// `packages` holds them.
fn workspace_patterns(workspaces: Value) -> Result<Vec<String>, AdapterError> {
    let list = match workspaces {
        Value::Object(mut object) => object.remove("packages").unwrap_or(Value::Null),
        list => list,
    };
    serde_json::from_value(list).map_err(|_| {
        AdapterError::new(
            "package.json",
            "`workspaces` must be a list of patterns, or an object whose `packages` is one",
        )
    })
}

/// JavaScript's truthiness, which npm applies to `private`.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
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

    fn discover(root: &Path) -> Vec<(String, String)> {
        Npm.discover(&DiscoverRequest {
            root: root.to_path_buf(),
        })
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
    fn pnpm_workspace_publishable_packages() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "package.json",
            r#"{"name": "monorepo", "private": true}"#,
        );
        write(
            root,
            "pnpm-workspace.yaml",
            "packages:\n  - 'packages/*'\ncatalog:\n  react: ^19.0.0\n",
        );
        write(
            root,
            "packages/web/package.json",
            r#"{"name": "@acme/web"}"#,
        );
        write(
            root,
            "packages/api/package.json",
            r#"{"name": "@acme/api"}"#,
        );
        write(
            root,
            "packages/core/package.json",
            r#"{"name": "@acme/core"}"#,
        );
        write(
            root,
            "packages/scripts/package.json",
            r#"{"name": "@acme/scripts", "private": true}"#,
        );
        assert_eq!(
            discover(root),
            units(&[
                ("@acme/api", "packages/api"),
                ("@acme/core", "packages/core"),
                ("@acme/web", "packages/web"),
            ])
        );
    }

    #[test]
    fn pnpm_workspace_without_packages_takes_every_package() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(root, "package.json", r#"{"name": "tool"}"#);
        write(root, "pnpm-workspace.yaml", "");
        write(root, "libs/a/package.json", r#"{"name": "a"}"#);
        write(
            root,
            "libs/a/node_modules/dep/package.json",
            r#"{"name": "dep"}"#,
        );
        assert_eq!(discover(root), units(&[("tool", "."), ("a", "libs/a")]));
    }

    #[test]
    fn npm_workspaces() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "package.json",
            r#"{"name": "root", "private": true, "workspaces": ["packages/*", "tools/cli"]}"#,
        );
        write(root, "packages/core/package.json", r#"{"name": "core"}"#);
        write(
            root,
            "packages/unnamed/package.json",
            r#"{"version": "1.0.0"}"#,
        );
        write(root, "tools/cli/package.json", r#"{"name": "cli"}"#);
        write(root, "tools/other/package.json", r#"{"name": "other"}"#);
        assert_eq!(
            discover(root),
            units(&[("core", "packages/core"), ("cli", "tools/cli")])
        );
    }

    #[test]
    fn yarn_style_workspaces_object() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "package.json",
            r#"{"private": true, "workspaces": {"packages": ["packages/*"]}}"#,
        );
        write(root, "packages/core/package.json", r#"{"name": "core"}"#);
        assert_eq!(discover(root), units(&[("core", "packages/core")]));
    }

    #[test]
    fn single_package() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "package.json",
            r#"{"name": "billing", "version": "1.0.0"}"#,
        );
        write(root, "examples/demo/package.json", r#"{"name": "demo"}"#);
        assert_eq!(discover(root), units(&[("billing", ".")]));
    }

    #[test]
    fn pnpm_manifest_other_than_package_json_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(root, "package.json", r#"{"private": true}"#);
        write(root, "pnpm-workspace.yaml", "packages: ['packages/**']\n");
        write(root, ".npmrc", "; settings\nmodules-dir = \"deps\"\n");
        write(root, "packages/core/package.json", r#"{"name": "core"}"#);
        write(root, "packages/core/deps/dep/package.yaml", "name: dep\n");
        write(root, "packages/ui/package.yaml", "name: ui\n");
        let err = Npm
            .discover(&DiscoverRequest {
                root: root.to_path_buf(),
            })
            .unwrap_err();
        assert_eq!(err.file, "packages/ui/package.yaml");
        assert!(
            err.message.contains("reads only package.json"),
            "{}",
            err.message
        );
    }

    #[test]
    fn npm_view_output() {
        use std::os::unix::process::ExitStatusExt;
        let output = |code: i32, stdout: &str, stderr: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        };
        assert_eq!(
            npm_versions(&output(0, "[\"1.0.0\", \"1.1.0\"]\n", "")),
            Ok(vec!["1.0.0".to_string(), "1.1.0".to_string()])
        );
        assert_eq!(
            npm_versions(&output(0, "\"1.0.0\"\n", "")),
            Ok(vec!["1.0.0".to_string()])
        );
        let not_found = "{\"error\": {\"code\": \"E404\", \"summary\": \"Not Found\"}}";
        assert_eq!(
            npm_versions(&output(1, not_found, "npm error code E404\n")),
            Ok(Vec::new())
        );
        let denied = "{\"error\": {\"code\": \"E401\", \"summary\": \"Unable to authenticate\"}}";
        assert_eq!(
            npm_versions(&output(1, denied, "")),
            Err("Unable to authenticate".to_string())
        );
    }

    #[test]
    fn other_package_managers_are_refused() {
        for lockfile in ["yarn.lock", "bun.lock", "bun.lockb"] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            write(
                root,
                "package.json",
                r#"{"name": "tool", "version": "1.0.0"}"#,
            );
            write(root, lockfile, "");
            let err = Npm
                .discover(&DiscoverRequest {
                    root: root.to_path_buf(),
                })
                .unwrap_err();
            assert_eq!(err.file, lockfile);
            assert!(err.message.contains("does not support"), "{}", err.message);
        }
    }

    #[test]
    fn invalid_manifest_names_the_file() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(root, "package.json", r#"{"workspaces": ["packages/*"]}"#);
        write(root, "packages/core/package.json", "{");
        let err = Npm
            .discover(&DiscoverRequest {
                root: root.to_path_buf(),
            })
            .unwrap_err();
        assert_eq!(err.file, "packages/core/package.json");
    }

    fn unit(name: &str, path: &str) -> UnitRef {
        UnitRef {
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    /// A pnpm workspace whose `api` uses `core` in every way.
    fn workspace(root: &Path) -> Vec<UnitRef> {
        write(
            root,
            "package.json",
            r#"{"name": "monorepo", "private": true, "devDependencies": {"core": "workspace:*"}}"#,
        );
        write(
            root,
            "pnpm-workspace.yaml",
            "packages:\n  - packages/*\ncatalog:\n  core: ^1.0.0\ncatalogs:\n  next:\n    core: '>=1.2.0 <2.0.0'\n",
        );
        write(
            root,
            "packages/core/package.json",
            r#"{"name": "core", "version": "1.2.0"}"#,
        );
        write(
            root,
            "packages/api/package.json",
            "{\n\t\"name\": \"api\",\n\t\"version\": \"0.3.0\",\n\t\"dependencies\": {\"core\": \"workspace:^\", \"left-pad\": \"^1.0.0\"},\n\t\"peerDependencies\": {\"core\": \"^1.0.0\"},\n\t\"optionalDependencies\": {\"core\": \"catalog:\"},\n\t\"devDependencies\": {\"core\": \"catalog:next\"}\n}\n",
        );
        write(
            root,
            "packages/app/package.json",
            r#"{"name": "app", "private": true, "dependencies": {"core": "workspace:~1.2.0", "api": "link:../api"}}"#,
        );
        vec![unit("api", "packages/api"), unit("core", "packages/core")]
    }

    fn inspect(root: &Path, units: Vec<UnitRef>) -> Inspection {
        Npm.inspect(&InspectRequest {
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
                "api 0.3.0 at packages/api/package.json `version`",
                "core 1.2.0 at packages/core/package.json `version`",
            ]
        );
        let edges: Vec<String> = inspection
            .edges
            .iter()
            .map(|edge| {
                format!(
                    "{} ({:?}) -> {} {}: {} = {:?}{} at {}",
                    edge.dependent_path,
                    edge.dependent_unit,
                    edge.dependency,
                    edge.kind,
                    edge.reference,
                    edge.range,
                    if edge.follows { ", follows" } else { "" },
                    edge.location
                )
            })
            .collect();
        expect_test::expect![[r#"
            [
                ". (None) -> core development: workspace:* = Some(\"1.2.0\"), follows at package.json `devDependencies.core`",
                "packages/api (Some(\"api\")) -> core runtime: workspace:^ = Some(\"^1.2.0\"), follows at packages/api/package.json `dependencies.core`",
                "packages/api (Some(\"api\")) -> core peer: ^1.0.0 = Some(\"^1.0.0\") at packages/api/package.json `peerDependencies.core`",
                "packages/api (Some(\"api\")) -> core optional: catalog: = Some(\"^1.0.0\") at pnpm-workspace.yaml `catalog.core`",
                "packages/api (Some(\"api\")) -> core development: catalog:next = Some(\">=1.2.0 <2.0.0\") at pnpm-workspace.yaml `catalogs.next.core`",
                "packages/app (None) -> core runtime: workspace:~1.2.0 = Some(\"~1.2.0\") at packages/app/package.json `dependencies.core`",
            ]
        "#]].assert_debug_eq(&edges);
    }

    #[test]
    fn unknown_catalog_entry_names_the_file() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let units = workspace(root);
        write(
            root,
            "packages/app/package.json",
            r#"{"name": "app", "private": true, "dependencies": {"core": "catalog:old"}}"#,
        );
        let err = Npm
            .inspect(&InspectRequest {
                root: root.to_path_buf(),
                units,
            })
            .unwrap_err();
        assert_eq!(err.file, "packages/app/package.json");
        assert!(
            err.message.contains("`dependencies.core`") && err.message.contains("`old`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn unit_without_a_version() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(root, "package.json", r#"{"name": "tool"}"#);
        let err = Npm
            .inspect(&InspectRequest {
                root: root.to_path_buf(),
                units: vec![unit("tool", ".")],
            })
            .unwrap_err();
        assert!(
            err.message.contains("needs a `version` string"),
            "{}",
            err.message
        );
    }

    #[test]
    fn writes_keep_the_files() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let units = workspace(root);
        let inspection = inspect(root, units.clone());
        let references = inspection
            .edges
            .into_iter()
            .filter(|edge| !edge.follows)
            .map(|edge| ReferenceWrite {
                edge,
                version: "2.0.0".to_string(),
            })
            .collect();
        let writes = Npm
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
            .map(|file| format!("--- {}\n{}\n", file.path, file.content.trim_end()))
            .collect();
        expect_test::expect![[r#"
            --- packages/api/package.json
            {
            	"name": "api",
            	"version": "0.3.1",
            	"dependencies": {"core": "workspace:^", "left-pad": "^1.0.0"},
            	"peerDependencies": {"core": "^2.0.0"},
            	"optionalDependencies": {"core": "catalog:"},
            	"devDependencies": {"core": "catalog:next"}
            }
            --- packages/app/package.json
            {"name": "app", "private": true, "dependencies": {"core": "workspace:~2.0.0", "api": "link:../api"}}
            --- packages/core/package.json
            {"name": "core", "version": "2.0.0"}
            --- pnpm-workspace.yaml
            packages:
              - packages/*
            catalog:
              core: ^2.0.0
            catalogs:
              next:
                core: '>=2.0.0 <3.0.0'
        "#]].assert_eq(&text);
    }

    #[test]
    fn ranges() {
        let accepts = |range: &str, version: &str| {
            Npm.accepts(&AcceptsRequest {
                range: range.to_string(),
                version: version.to_string(),
                file: "package.json".to_string(),
            })
            .unwrap()
        };
        assert!(!accepts("^1.2.0", "2.0.0"));
        assert!(accepts("^1.2.0", "1.9.0"));
        // npm accepts a prerelease only where the range names one of the
        // same version.
        assert!(!accepts("^1.2.0", "1.5.0-beta.0"));
        assert!(!accepts("*", "2.0.0-beta.0"));
        assert!(accepts("^1.5.0-beta.0", "1.5.0-beta.1"));
        assert!(!accepts("^1.5.0-beta.0", "1.6.0-beta.0"));
        assert!(accepts("1.x || >=2.5.0-rc.1", "2.5.0-rc.2"));
        for (range, rewritten) in [
            ("^1.2.0", "^2.0.0"),
            ("~1.2.0", "~2.0.0"),
            (">=1.2.0", ">=2.0.0"),
            ("1.2.0", "2.0.0"),
            ("1.x", "2.0.0"),
            ("<2.0.0", "2.0.0"),
            (">=1.0.0 <2.0.0", ">=2.0.0 <3.0.0"),
            ("^1.0.0 || ^0.9.0", "^2.0.0"),
        ] {
            assert_eq!(rewrite_range(range, "2.0.0"), rewritten, "{range}");
        }
        assert_eq!(rewrite_range("*", "2.0.0-beta.0"), "2.0.0-beta.0");
    }

    #[test]
    fn lockfiles_at_the_workspace_root() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let units = workspace(root);
        let lockfiles = |units: Vec<UnitRef>| {
            Npm.lockfiles(&LockfileRequest {
                root: root.to_path_buf(),
                units,
            })
            .unwrap()
            .lockfiles
        };
        assert!(lockfiles(units.clone()).is_empty());
        write(root, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        write(root, "packages/api/package-lock.json", "{}");
        let found: Vec<String> = lockfiles(units)
            .iter()
            .map(|lockfile| {
                format!(
                    "{} in {}: {}",
                    lockfile.path, lockfile.dir, lockfile.command[0]
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                "packages/api/package-lock.json in packages/api: npm",
                "pnpm-lock.yaml in .: pnpm"
            ]
        );
    }
}
