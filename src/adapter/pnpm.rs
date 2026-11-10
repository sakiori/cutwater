//! `pnpm-workspace.yaml`: the packages of a pnpm workspace and its catalogs.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_saphyr::Spanned;

use super::AdapterError;

pub const PNPM_WORKSPACE: &str = "pnpm-workspace.yaml";

/// The settings of `pnpm-workspace.yaml` the adapter reads.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PnpmWorkspace {
    pub packages: Option<Vec<String>>,
    pub modules_dir: Option<String>,
    /// The default catalog.
    catalog: Option<BTreeMap<String, Spanned<String>>>,
    /// The named catalogs; `default` is the default catalog.
    catalogs: Option<BTreeMap<String, BTreeMap<String, Spanned<String>>>>,
}

/// A catalog entry: the range the catalog gives a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub range: String,
    /// The keys of the entry in `pnpm-workspace.yaml`.
    pub keys: Vec<String>,
}

impl PnpmWorkspace {
    /// The workspace file of `root`, `None` when there is none.
    pub fn load(root: &Path) -> Result<Option<PnpmWorkspace>, AdapterError> {
        if !root.join(PNPM_WORKSPACE).is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(root.join(PNPM_WORKSPACE))
            .map_err(|err| AdapterError::new(PNPM_WORKSPACE, format!("cannot read it: {err}")))?;
        Self::parse(&text).map(Some)
    }

    fn parse(text: &str) -> Result<PnpmWorkspace, AdapterError> {
        let workspace: Option<PnpmWorkspace> = serde_saphyr::from_str(text)
            .map_err(|err| AdapterError::new(PNPM_WORKSPACE, err.to_string()))?;
        Ok(workspace.unwrap_or_default())
    }

    /// The entry of `package` in the catalog that `catalog:<name>` names;
    /// `catalog:` names the default catalog.
    pub fn catalog_entry(&self, name: &str, package: &str) -> Result<CatalogEntry, String> {
        let name = if name.is_empty() { "default" } else { name };
        let found = match (name, &self.catalog) {
            ("default", Some(catalog)) => catalog
                .get(package)
                .map(|range| (range, vec!["catalog".to_string(), package.to_string()])),
            _ => self
                .catalogs
                .as_ref()
                .and_then(|catalogs| catalogs.get(name))
                .and_then(|catalog| catalog.get(package))
                .map(|range| {
                    let keys = ["catalogs", name, package];
                    (range, keys.map(str::to_string).to_vec())
                }),
        };
        let (range, keys) = found.ok_or_else(|| {
            format!("the catalog `{name}` of {PNPM_WORKSPACE} has no entry for `{package}`")
        })?;
        Ok(CatalogEntry {
            range: range.value.clone(),
            keys,
        })
    }
}

/// `text`, a `pnpm-workspace.yaml`, with the catalog entry at `keys` set to
/// `range`, keeping everything else, and the entry's quoting where the new
/// value allows it.
pub fn set_catalog_entry(text: &str, keys: &[String], range: &str) -> Result<String, AdapterError> {
    let workspace = PnpmWorkspace::parse(text)?;
    let value = match keys {
        [catalog, package] if catalog == "catalog" => workspace
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.get(package)),
        [catalogs, name, package] if catalogs == "catalogs" => workspace
            .catalogs
            .as_ref()
            .and_then(|catalogs| catalogs.get(name))
            .and_then(|catalog| catalog.get(package)),
        _ => None,
    };
    let missing = || {
        AdapterError::new(
            PNPM_WORKSPACE,
            format!("has no catalog entry `{}`", keys.join(".")),
        )
    };
    let span = value.ok_or_else(missing)?.defined.span();
    let (Some(start), Some(len)) = (span.byte_offset(), span.byte_len()) else {
        return Err(missing());
    };
    let (start, end) = (start as usize, (start + len) as usize);
    let written = &text[start..end];
    let candidates = match written.chars().next() {
        Some('\'') => vec![format!("'{}'", range.replace('\'', "''"))],
        Some('"') => vec![double_quoted(range)],
        _ => vec![range.to_string(), double_quoted(range)],
    };
    for candidate in candidates {
        let edited = format!("{}{candidate}{}", &text[..start], &text[end..]);
        let reread = PnpmWorkspace::parse(&edited)
            .ok()
            .and_then(|workspace| workspace.entry_value(keys));
        if reread.as_deref() == Some(range) {
            return Ok(edited);
        }
    }
    Err(AdapterError::new(
        PNPM_WORKSPACE,
        format!(
            "cannot write `{range}` into the catalog entry `{}`",
            keys.join(".")
        ),
    ))
}

impl PnpmWorkspace {
    fn entry_value(&self, keys: &[String]) -> Option<String> {
        let value = match keys {
            [_, package] => self.catalog.as_ref()?.get(package)?,
            [_, name, package] => self.catalogs.as_ref()?.get(name)?.get(package)?,
            _ => return None,
        };
        Some(value.value.clone())
    }
}

fn double_quoted(value: &str) -> String {
    serde_json::to_string(value).expect("a string serializes to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "packages:\n  - 'packages/*'\n\n# Shared ranges, é\ncatalog:\n  core: ^1.0.0 # the core\n  'ui': \"~2.0.0\"\n  web: '>=1.0.0'\ncatalogs:\n  legacy:\n    core: ^0.9.0\n";

    fn keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn catalog_entries() {
        let workspace = PnpmWorkspace::parse(WORKSPACE).unwrap();
        let entry = workspace.catalog_entry("", "core").unwrap();
        assert_eq!(entry.range, "^1.0.0");
        assert_eq!(entry.keys, keys(&["catalog", "core"]));
        assert_eq!(
            workspace.catalog_entry("default", "ui").unwrap().range,
            "~2.0.0"
        );
        let legacy = workspace.catalog_entry("legacy", "core").unwrap();
        assert_eq!(legacy.keys, keys(&["catalogs", "legacy", "core"]));
        let problem = workspace.catalog_entry("legacy", "ui").unwrap_err();
        assert!(problem.contains("`legacy`"), "{problem}");
        let named_default =
            PnpmWorkspace::parse("catalogs:\n  default:\n    core: ^1.0.0\n").unwrap();
        assert_eq!(
            named_default.catalog_entry("", "core").unwrap().keys,
            keys(&["catalogs", "default", "core"])
        );
    }

    #[test]
    fn editing_keeps_the_file() {
        let edited = set_catalog_entry(WORKSPACE, &keys(&["catalog", "core"]), "^2.0.0").unwrap();
        let edited = set_catalog_entry(&edited, &keys(&["catalog", "ui"]), "~3.0.0").unwrap();
        let edited = set_catalog_entry(&edited, &keys(&["catalog", "web"]), ">=2.0.0").unwrap();
        let edited =
            set_catalog_entry(&edited, &keys(&["catalogs", "legacy", "core"]), ">=1.0.0").unwrap();
        expect_test::expect![[r#"
            packages:
              - 'packages/*'

            # Shared ranges, é
            catalog:
              core: ^2.0.0 # the core
              'ui': "~3.0.0"
              web: '>=2.0.0'
            catalogs:
              legacy:
                core: ">=1.0.0"
        "#]]
        .assert_eq(&edited);
        assert!(set_catalog_entry(WORKSPACE, &keys(&["catalog", "api"]), "1.0.0").is_err());
    }
}
