//! The npm adapter.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::walk::{PNPM_MANIFESTS, npm_dirs, pnpm_dirs};
use super::{Adapter, AdapterError, DiscoverRequest, DiscoveredUnit, Discovery};

pub struct Npm;

const PNPM_WORKSPACE: &str = "pnpm-workspace.yaml";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PnpmWorkspace {
    packages: Option<Vec<String>>,
    modules_dir: Option<String>,
}

#[derive(Deserialize)]
struct PackageJson {
    name: Option<String>,
    #[serde(default)]
    private: Value,
    workspaces: Option<Value>,
}

impl Adapter for Npm {
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError> {
        let root = &request.root;
        let mut dirs = BTreeSet::new();
        if root.join(PNPM_WORKSPACE).is_file() {
            let text = read(root, PNPM_WORKSPACE)?;
            let workspace: Option<PnpmWorkspace> = serde_saphyr::from_str(&text)
                .map_err(|err| AdapterError::new(PNPM_WORKSPACE, err.to_string()))?;
            let (packages, modules_dir) = workspace
                .map(|workspace| (workspace.packages, workspace.modules_dir))
                .unwrap_or_default();
            // Without `packages`, pnpm takes every package below the root.
            let patterns = packages.unwrap_or_else(|| vec![".".to_string(), "**".to_string()]);
            // Older pnpm reads the setting from .npmrc only; newer pnpm prefers
            // pnpm-workspace.yaml.
            let modules_dir = match modules_dir {
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
                    let file = if dir == "." {
                        manifest.to_string()
                    } else {
                        format!("{dir}/{manifest}")
                    };
                    return Err(AdapterError::new(
                        file,
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

        let mut units = Vec::new();
        for dir in dirs {
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

fn manifest_path(dir: &str) -> String {
    if dir == "." {
        "package.json".to_string()
    } else {
        format!("{dir}/package.json")
    }
}

fn read(root: &Path, file: &str) -> Result<String, AdapterError> {
    std::fs::read_to_string(root.join(file))
        .map_err(|err| AdapterError::new(file, format!("cannot read it: {err}")))
}

fn package_json(root: &Path, dir: &str) -> Result<PackageJson, AdapterError> {
    let file = manifest_path(dir);
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
}
