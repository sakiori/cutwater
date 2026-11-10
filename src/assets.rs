//! Release assets: the files a unit's asset patterns match, which CI built
//! before `publish`, with the names and checksums under which they go to
//! the project's generic package registry.

use std::path::Path;

use anyhow::{Context, Result, bail};
use globset::GlobBuilder;
use sha2::{Digest, Sha256};

use crate::units::Unit;

/// A file of a unit's release assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The file relative to the repository root, with `/` separators.
    pub path: String,
    /// The file's name, under which it is uploaded.
    pub name: String,
    /// The SHA-256 checksum of its content, in lowercase hex.
    pub sha256: String,
}

/// The files of the repository at `root` that the unit's asset patterns
/// match. Every pattern must match a file, every file must have a name the
/// generic package registry accepts, and no two files may share a name.
pub fn resolve(root: &Path, unit: &Unit) -> Result<Vec<Asset>> {
    let mut assets: Vec<Asset> = Vec::new();
    for pattern in &unit.assets {
        let matcher = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .with_context(|| format!("the asset pattern `{pattern}` of unit `{}`", unit.name))?
            .compile_matcher();
        let base = literal_prefix(pattern);
        let mut matched = false;
        let start = root.join(&base);
        if start.exists() {
            for entry in walkdir::WalkDir::new(&start).sort_by_file_name() {
                let entry = entry.with_context(|| format!("cannot list {}", start.display()))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .expect("the walk stays below the root")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if !matcher.is_match(&relative) {
                    continue;
                }
                matched = true;
                if assets.iter().any(|asset| asset.path == relative) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if !is_file_name(&name) {
                    bail!(
                        "the release asset {relative} of unit `{}` has a name the generic package \
                         registry refuses: it accepts letters, digits, `.`, `_`, `-`, `+`, `~` and \
                         `@`, with no `~` or `@` at either end",
                        unit.name
                    );
                }
                if let Some(other) = assets.iter().find(|asset| asset.name == name) {
                    bail!(
                        "the release assets {} and {relative} of unit `{}` have the same name, \
                         which the generic package registry keeps only once",
                        other.path,
                        unit.name
                    );
                }
                let content = std::fs::read(entry.path())
                    .with_context(|| format!("cannot read the release asset {relative}"))?;
                assets.push(Asset {
                    path: relative,
                    name,
                    sha256: sha256(&content),
                });
            }
        }
        if !matched {
            bail!(
                "the asset pattern `{pattern}` of unit `{}` matches no file; build the release \
                 assets before `publish`",
                unit.name
            );
        }
    }
    Ok(assets)
}

/// The SHA-256 checksum of `content`, in lowercase hex.
pub fn sha256(content: &[u8]) -> String {
    Sha256::digest(content)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The directories of a pattern before its first component with a glob
/// character, where its matches lie.
fn literal_prefix(pattern: &str) -> String {
    let parts: Vec<&str> = pattern.split('/').collect();
    let literal: Vec<&str> = parts[..parts.len() - 1]
        .iter()
        .take_while(|part| !part.contains(['*', '?', '[', '{', '\\']))
        .copied()
        .collect();
    literal.join("/")
}

/// Whether the generic package registry accepts `name` as a file name.
fn is_file_name(name: &str) -> bool {
    let allowed =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '~' | '@');
    let edge = |c: Option<char>| c.is_some_and(|c| c != '~' && c != '@');
    name.chars().all(allowed) && edge(name.chars().next()) && edge(name.chars().last())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_the_registry_accepts() {
        for name in ["cli-1.0.0-x86_64.tar.gz", "a+b~c@d", "x"] {
            assert!(is_file_name(name), "{name}");
        }
        for name in ["", "~a", "a@", "with space", "ü.tar"] {
            assert!(!is_file_name(name), "{name}");
        }
    }

    #[test]
    fn literal_prefixes() {
        assert_eq!(literal_prefix("dist/*.tar.gz"), "dist");
        assert_eq!(literal_prefix("target/dist/**/*.zip"), "target/dist");
        assert_eq!(literal_prefix("*.zip"), "");
        assert_eq!(literal_prefix("out/{a,b}/x.zip"), "out");
    }

    #[test]
    fn checksum() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
