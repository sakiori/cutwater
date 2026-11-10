//! The directories that workspace patterns select, as pnpm and npm read
//! them. Both search below the repository root without entering
//! `node_modules` or, through a wildcard, a directory whose name starts with
//! a dot; they differ in how negated patterns work and in what else they
//! leave out.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use globset::{Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use super::AdapterError;

/// The manifest names pnpm reads, in the order it prefers them.
pub const PNPM_MANIFESTS: [&str; 3] = ["package.json", "package.json5", "package.yaml"];

/// The directories with a pnpm manifest that the `packages` patterns of
/// `pnpm-workspace.yaml` select, with the name of the manifest pnpm reads
/// there. A pattern selects the directories whose manifest its glob,
/// extended by `/package.json`, matches; a pattern starting with `!`
/// excludes, also inside dot directories. No search enters
/// `bower_components` or pnpm's `modulesDir`, `modules_dir`: a relative one
/// inside any project, an absolute one where it lies.
pub fn pnpm_dirs(
    root: &Path,
    patterns: &[String],
    modules_dir: Option<&str>,
    file: &str,
) -> Result<BTreeMap<String, &'static str>, AdapterError> {
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for pattern in patterns {
        let (negated, body) = match pattern.strip_prefix('!') {
            Some(body) => (true, body),
            None => (false, pattern.as_str()),
        };
        // An absolute pattern selects nothing, and a pattern that leaves the
        // repository selects nothing that could be a unit of it.
        if body.starts_with('/') {
            continue;
        }
        let Some(body) = normalize(body) else {
            continue;
        };
        if negated {
            excluded.push(body);
        } else {
            included.push(body);
        }
    }
    let manifest_globs =
        |patterns: &[String]| glob_set(patterns.iter().map(|pattern| manifest_of(pattern)), file);
    let inclusions = manifest_globs(&included)?;
    let exclusions = manifest_globs(&excluded)?;
    let (anywhere, at) = match modules_dir {
        Some(dir) if Path::new(dir).is_absolute() => (None, below(root, Path::new(dir))),
        Some(dir) => (normalize(dir).filter(|dir| dir != "."), None),
        None => (None, None),
    };
    let skip = |dir: &str, name: &str| {
        name == "bower_components"
            || anywhere
                .as_ref()
                .is_some_and(|skip| dir == skip || dir.ends_with(&format!("/{skip}")))
            || at.as_ref().is_some_and(|skip| dir == skip)
    };
    let found = search(root, &included, &PNPM_MANIFESTS, &skip, file)?;
    Ok(found
        .into_iter()
        .filter(|(dir, _)| {
            let manifest = manifest_of(dir);
            inclusions.is_match(&manifest) && !exclusions.is_match(&manifest)
        })
        .collect())
}

/// The directories with a `package.json` that the `workspaces` patterns of
/// a root `package.json` select, as npm's map-workspaces reads them: a
/// pattern selects directories; an odd number of leading `!` makes it
/// exclude what matches it and, for a pattern ending in `/**`, the directory
/// itself; and a pattern undoes the earlier exclusions that match it.
pub fn npm_dirs(
    root: &Path,
    patterns: &[String],
    file: &str,
) -> Result<BTreeSet<String>, AdapterError> {
    let compile = |pattern: &str| -> Result<GlobMatcher, AdapterError> {
        Ok(glob(pattern, file)?.compile_matcher())
    };
    let mut included: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    for pattern in patterns {
        let bangs = pattern.len() - pattern.trim_start_matches('!').len();
        let body = pattern[bangs..]
            .trim_start_matches("./")
            .trim_start_matches('/');
        let body = body.trim_end_matches('/').to_string();
        if bangs % 2 == 1 {
            excluded.push(body);
            continue;
        }
        let mut kept = Vec::new();
        for exclusion in excluded {
            if !compile(&exclusion)?.is_match(&body) {
                kept.push(exclusion);
            }
        }
        excluded = kept;
        included.push(body);
    }
    for exclusion in &excluded {
        let matcher = compile(exclusion)?;
        included.retain(|pattern| !matcher.is_match(pattern));
    }

    let inclusions = glob_set(included.iter().cloned(), file)?;
    let exclusions = glob_set(
        excluded.iter().flat_map(|pattern| {
            let parent = pattern.strip_suffix("/**").map(str::to_string);
            std::iter::once(pattern.clone()).chain(parent)
        }),
        file,
    )?;
    let found = search(root, &included, &["package.json"], &|_, _| false, file)?;
    Ok(found
        .into_keys()
        .filter(|dir| inclusions.is_match(dir) && !exclusions.is_match(dir))
        .collect())
}

/// The directories below `root` that hold one of `manifests`, as far as the
/// `patterns` can reach, with the first of `manifests` each holds.
/// Directories `skip` names, by their path relative to the root and their
/// name, are not searched.
fn search(
    root: &Path,
    patterns: &[String],
    manifests: &[&'static str],
    skip: &dyn Fn(&str, &str) -> bool,
    file: &str,
) -> Result<BTreeMap<String, &'static str>, AdapterError> {
    let mut found = BTreeMap::new();
    for pattern in patterns {
        let prefix = literal_prefix(pattern);
        let start = if prefix.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&prefix)
        };
        if !start.is_dir() {
            continue;
        }
        let mut walk = WalkDir::new(&start);
        if pattern == "." {
            walk = walk.max_depth(0);
        } else if !pattern.contains("**") {
            let literal = prefix.split('/').filter(|part| !part.is_empty()).count();
            walk = walk.max_depth(pattern.split('/').count() - literal);
        }
        let entries = walk.into_iter().filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if !entry.file_type().is_dir() {
                return false;
            }
            let name = entry.file_name().to_string_lossy();
            if name.starts_with('.') || name == "node_modules" {
                return false;
            }
            !relative(root, entry.path()).is_some_and(|dir| skip(&dir, &name))
        });
        for entry in entries {
            let entry = entry.map_err(|err| {
                AdapterError::new(file, format!("cannot search for packages: {err}"))
            })?;
            if !entry.file_type().is_dir() {
                continue;
            }
            let Some(manifest) = manifests
                .iter()
                .find(|manifest| entry.path().join(manifest).is_file())
            else {
                continue;
            };
            if let Some(dir) = relative(root, entry.path()) {
                found.insert(dir, *manifest);
            }
        }
    }
    Ok(found)
}

fn manifest_of(dir: &str) -> String {
    if dir == "." {
        "package.json".to_string()
    } else {
        format!("{dir}/package.json")
    }
}

fn glob(pattern: &str, file: &str) -> Result<Glob, AdapterError> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|err| {
            AdapterError::new(
                file,
                format!("invalid workspace pattern `{pattern}`: {err}"),
            )
        })
}

fn glob_set(
    patterns: impl IntoIterator<Item = String>,
    file: &str,
) -> Result<GlobSet, AdapterError> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        set.add(glob(&pattern, file)?);
    }
    set.build()
        .map_err(|err| AdapterError::new(file, format!("invalid workspace patterns: {err}")))
}

/// The pattern without `./`, `a/..` and a trailing `/`, `.` for the root,
/// or `None` when it leaves the repository.
fn normalize(pattern: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in pattern.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// The leading components of a pattern that hold no glob syntax.
fn literal_prefix(pattern: &str) -> String {
    if pattern == "." {
        return String::new();
    }
    pattern
        .split('/')
        .take_while(|part| !part.contains(['*', '?', '[', '{']))
        .collect::<Vec<_>>()
        .join("/")
}

/// An absolute `path` relative to `root`, when it lies below it.
fn below(root: &Path, path: &Path) -> Option<String> {
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    relative(root, path)
        .or_else(|| relative(&canonical(root)?, &canonical(path)?))
        .filter(|dir| dir != ".")
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dirs: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for dir in dirs {
            let (dir, manifest) = dir.split_once(':').unwrap_or((dir, "package.json"));
            let dir = root.path().join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(manifest), "{}").unwrap();
        }
        root
    }

    fn strings(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(ToString::to_string).collect()
    }

    fn pnpm(root: &Path, patterns: &[&str], modules_dir: Option<&str>) -> Vec<String> {
        pnpm_dirs(root, &strings(patterns), modules_dir, "pnpm-workspace.yaml")
            .unwrap()
            .into_keys()
            .collect()
    }

    fn npm(root: &Path, patterns: &[&str]) -> Vec<String> {
        npm_dirs(root, &strings(patterns), "package.json")
            .unwrap()
            .into_iter()
            .collect()
    }

    #[test]
    fn single_star_selects_one_level_and_exclusions_apply() {
        let root = tree(&[
            "packages/a",
            "packages/b",
            "packages/a/nested",
            "packages/legacy",
            "other/c",
        ]);
        let patterns = ["packages/*", "!packages/legacy"];
        assert_eq!(
            pnpm(root.path(), &patterns, None),
            ["packages/a", "packages/b"]
        );
        assert_eq!(npm(root.path(), &patterns), ["packages/a", "packages/b"]);
    }

    #[test]
    fn double_star_skips_dependencies_and_dot_directories() {
        let root = tree(&[
            "apps/web",
            "apps/web/node_modules/dep",
            "apps/.cache/tool",
            "apps/deep/er/api",
        ]);
        let expected = ["apps/deep/er/api", "apps/web"];
        assert_eq!(pnpm(root.path(), &["./apps/**/"], None), expected);
        assert_eq!(npm(root.path(), &["./apps/**/"]), expected);
    }

    #[test]
    fn pnpm_exclusion_with_double_star_covers_the_directory_itself() {
        let root = tree(&["packages/a", "packages/test", "packages/test/fixture"]);
        assert_eq!(
            pnpm(root.path(), &["packages/**", "!**/test/**"], None),
            ["packages/a"]
        );
    }

    #[test]
    fn pnpm_skips_its_modules_directory() {
        let root = tree(&[
            "packages/a",
            "packages/a/deps/dep",
            "deps/other",
            "packages/b/bower_components/c",
        ]);
        assert_eq!(pnpm(root.path(), &["**"], Some("deps")), ["packages/a"]);
        let absolute = root.path().join("packages/a/deps");
        assert_eq!(
            pnpm(root.path(), &["**"], absolute.to_str()),
            ["deps/other", "packages/a"]
        );
        assert_eq!(
            npm(root.path(), &["**"]),
            [
                "deps/other",
                "packages/a",
                "packages/a/deps/dep",
                "packages/b/bower_components/c"
            ]
        );
    }

    #[test]
    fn pnpm_reports_the_manifest_it_reads() {
        let root = tree(&[
            "packages/a",
            "packages/b:package.yaml",
            "packages/c:package.json5",
        ]);
        let found = pnpm_dirs(root.path(), &strings(&["packages/*"]), None, "x").unwrap();
        assert_eq!(found["packages/a"], "package.json");
        assert_eq!(found["packages/b"], "package.yaml");
        assert_eq!(found["packages/c"], "package.json5");
    }

    #[test]
    fn npm_later_patterns_undo_earlier_exclusions() {
        let root = tree(&["packages/a", "packages/b", "packages/b/a", "packages/c"]);
        assert_eq!(
            npm(
                root.path(),
                &["packages/**", "!packages/b/**", "packages/b/a"]
            ),
            ["packages/a", "packages/b", "packages/b/a", "packages/c"]
        );
        assert_eq!(
            npm(root.path(), &["packages/*", "!packages/b", "!!packages/c"]),
            ["packages/a", "packages/c"]
        );
    }

    #[test]
    fn literal_patterns_name_dot_directories_and_the_root() {
        let root = tree(&[".", ".config/tool"]);
        assert_eq!(
            pnpm(root.path(), &[".", ".config/tool"], None),
            [".", ".config/tool"]
        );
    }
}
