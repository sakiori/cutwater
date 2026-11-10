//! The Cargo adapter. Cargo itself, through `cargo metadata`, says which
//! packages the workspace in the root `Cargo.toml` has and where they may be
//! published.

use std::path::{Path, PathBuf};

use cargo_metadata::MetadataCommand;

use super::{Adapter, AdapterError, DiscoverRequest, DiscoveredUnit, Discovery};

pub struct Cargo;

impl Adapter for Cargo {
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError> {
        discover(&request.root, None)
    }
}

/// The publishable members of the workspace at `root`, asking `cargo`, or
/// the cargo `CARGO` names, when `cargo` is `None`.
fn discover(root: &Path, cargo: Option<&Path>) -> Result<Discovery, AdapterError> {
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(Discovery::default());
    }
    let mut command = MetadataCommand::new();
    command
        .manifest_path(&manifest)
        .current_dir(root)
        .no_deps()
        // Discovery reads manifests only; it never needs the network.
        .other_options(vec!["--offline".to_string()]);
    if let Some(cargo) = cargo {
        command.cargo_path(cargo);
    }
    let metadata = command.exec().map_err(|err| match err {
        cargo_metadata::Error::Io(err) if err.kind() == std::io::ErrorKind::NotFound => {
            AdapterError::new(
                "Cargo.toml",
                "Cargo discovery (`cargo.discover`) runs `cargo metadata`, but cargo is not on \
                 the PATH; install cargo or turn Cargo discovery off",
            )
        }
        cargo_metadata::Error::CargoMetadata { stderr } => AdapterError::new(
            "Cargo.toml",
            format!("`cargo metadata` failed: {}", stderr.trim()),
        ),
        err => AdapterError::new("Cargo.toml", format!("`cargo metadata` failed: {err}")),
    })?;

    let root = canonical(root)?;
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
            .map(|dir| canonical(dir.as_std_path()))
            .transpose()?
            .unwrap_or_default();
        let Ok(relative) = dir.strip_prefix(&root) else {
            return Err(AdapterError::new(
                "Cargo.toml",
                format!(
                    "workspace member `{}` lies outside the repository",
                    package.name
                ),
            ));
        };
        let parts: Vec<_> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        units.push(DiscoveredUnit {
            name: package.name.to_string(),
            path: if parts.is_empty() {
                ".".to_string()
            } else {
                parts.join("/")
            },
        });
    }
    units.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.name.cmp(&b.name)));
    Ok(Discovery { units })
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
}
