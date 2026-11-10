//! The repository's settings, `.cutwater/config.toml`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use toml::Spanned;

use crate::adapter::AdapterKind;

/// The configuration file, relative to the repository root.
pub const CONFIG_FILE: &str = ".cutwater/config.toml";

/// The settings of cutwater for one repository, in `.cutwater/config.toml`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(title = "cutwater configuration")]
pub struct Config {
    /// The branch that releases are made from and that merge requests target.
    #[serde(default = "default_branch")]
    pub default_branch: String,
    /// The branch that holds the release merge request's commit. A merge
    /// request from this branch of the same project is exempt from the
    /// changeset rule of `check`.
    #[serde(default = "release_branch")]
    pub release_branch: String,
    /// The discovery of npm packages: pnpm workspaces (`pnpm-workspace.yaml`),
    /// npm workspaces (`workspaces` in `package.json`) or a single
    /// `package.json`, at the repository root.
    #[serde(default)]
    pub npm: DiscoverySettings,
    /// The discovery of Cargo crates: the members of the workspace in the
    /// root `Cargo.toml`, or the single crate it declares, as `cargo metadata`
    /// reports them, so cargo must be on the `PATH`.
    #[serde(default)]
    pub cargo: DiscoverySettings,
    /// Units listed by hand. A listed unit overrides the discovered unit of
    /// the same name; it needs `path` and `adapter` only when no adapter
    /// discovers it.
    #[serde(default, rename = "unit")]
    pub units: Vec<UnitSettings>,
    /// Fixed groups: units that always share one version and one tag.
    #[serde(default, rename = "group")]
    pub groups: Vec<GroupSettings>,
    #[serde(skip)]
    #[schemars(skip)]
    lines: LineIndex,
}

fn default_branch() -> String {
    "main".to_string()
}

fn release_branch() -> String {
    "cutwater/release".to_string()
}

/// How an adapter discovers units.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DiscoverySettings {
    /// Whether the adapter's publishable packages are units. Private npm
    /// packages and crates with `publish = false` never are.
    #[serde(default)]
    pub discover: bool,
    /// Names of discovered packages that are not units.
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub exclude: Vec<Spanned<String>>,
}

/// A unit: what is versioned and released as one.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct UnitSettings {
    /// The unit's name, unique in the repository: for a discovered unit, its
    /// package's name.
    #[schemars(with = "String")]
    pub name: Spanned<String>,
    /// The unit's directory, relative to the repository root.
    pub path: Option<String>,
    /// The ecosystem adapter that versions and publishes the unit.
    pub adapter: Option<AdapterKind>,
    /// Patterns of the files the unit ships, relative to its path, in the
    /// syntax of `.gitignore`: a pattern naming a directory covers everything
    /// under it, a pattern starting with `!` excludes, and the last pattern
    /// that applies to a file decides. Without patterns, or with exclusions
    /// only, every file under the path is shipped. Whatever the patterns
    /// say, a unit never ships the files of units nested under it, nor
    /// `.changeset/` or `.cutwater/`.
    pub shipped: Option<Vec<String>>,
    /// The template of the unit's release tags, with the placeholders
    /// `{unit}` and `{version}`. Default: `v{version}` when the repository
    /// has one unit, `{unit}-v{version}` otherwise. A member of a fixed group
    /// takes the group's tags and sets none.
    pub tag: Option<String>,
}

/// A fixed group: two or more units that always share one version.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GroupSettings {
    /// The group's name.
    #[schemars(with = "String")]
    pub name: Spanned<String>,
    /// The names of the group's units; a unit belongs to at most one group.
    pub units: Vec<String>,
    /// The template of the group's release tags, with the placeholders
    /// `{group}` and `{version}`. Default: `{group}-v{version}`.
    pub tag: Option<String>,
}

impl Config {
    /// Reads the configuration of the repository at `root`.
    pub fn load(root: &Path) -> Result<Config> {
        let path = root.join(CONFIG_FILE);
        if !path.is_file() {
            bail!(
                "{CONFIG_FILE} is missing, so this repository is not set up for cutwater; \
                 run `cutwater init` to set it up"
            );
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("cannot read {CONFIG_FILE}"))?;
        Self::parse(&text)
    }

    /// Parses and checks the text of a configuration file.
    pub fn parse(text: &str) -> Result<Config> {
        let mut config: Config =
            toml::from_str(text).map_err(|err| anyhow::anyhow!("{CONFIG_FILE}: {err}"))?;
        config.lines = LineIndex::new(text);
        for (setting, branch) in [
            ("default-branch", &config.default_branch),
            ("release-branch", &config.release_branch),
        ] {
            if branch.trim().is_empty() {
                bail!("{CONFIG_FILE}: `{setting}` is empty; name a branch");
            }
        }
        if config.default_branch == config.release_branch {
            bail!(
                "{CONFIG_FILE}: `release-branch` and `default-branch` are both `{}`; \
                 the release merge request needs a branch of its own",
                config.release_branch
            );
        }
        for kind in AdapterKind::ALL {
            let settings = config.discovery(kind);
            if !settings.discover && !settings.exclude.is_empty() {
                bail!(
                    "{CONFIG_FILE}, line {}: `{kind}.exclude` excludes discovered units, \
                     but `{kind}.discover` is off; turn discovery on or remove the exclusions",
                    config.line(&settings.exclude[0])
                );
            }
        }
        Ok(config)
    }

    pub fn discovery(&self, kind: AdapterKind) -> &DiscoverySettings {
        match kind {
            AdapterKind::Npm => &self.npm,
            AdapterKind::Cargo => &self.cargo,
        }
    }

    /// The line of the configuration file where `value` starts.
    pub fn line<T>(&self, value: &Spanned<T>) -> usize {
        self.lines.line(value.span().start)
    }
}

/// The JSON Schema of the configuration file, as the repository publishes it.
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(Config);
    let mut text = serde_json::to_string_pretty(&schema).expect("a schema serializes to JSON");
    text.push('\n');
    text
}

/// The start offsets of a text's lines.
#[derive(Debug, Default)]
struct LineIndex(Vec<usize>);

impl LineIndex {
    fn new(text: &str) -> Self {
        Self(
            std::iter::once(0)
                .chain(text.match_indices('\n').map(|(at, _)| at + 1))
                .collect(),
        )
    }

    /// The one-based line of `offset`.
    fn line(&self, offset: usize) -> usize {
        self.0.partition_point(|&start| start <= offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> String {
        format!("{:#}", Config::parse(text).unwrap_err())
    }

    #[test]
    fn defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.default_branch, "main");
        assert_eq!(config.release_branch, "cutwater/release");
        assert!(!config.npm.discover && !config.cargo.discover);
    }

    #[test]
    fn unknown_setting_names_the_setting_and_its_line() {
        let message =
            error("[npm]\ndiscover = true\n\n[[unit]]\nname = \"api\"\nshiped = [\"src/**\"]\n");
        assert!(message.contains("line 6"), "{message}");
        assert!(message.contains("unknown field `shiped`"), "{message}");
    }

    #[test]
    fn unknown_top_level_setting() {
        let message = error("default-branch = \"main\"\nmade-up-setting = true\n");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("`made-up-setting`"), "{message}");
    }

    #[test]
    fn unknown_adapter() {
        let message = error("[[unit]]\nname = \"api\"\npath = \"api\"\nadapter = \"gradle\"\n");
        assert!(message.contains("line 4"), "{message}");
        assert!(message.contains("gradle"), "{message}");
    }

    #[test]
    fn release_branch_must_differ_from_default_branch() {
        let message = error("default-branch = \"trunk\"\nrelease-branch = \"trunk\"\n");
        assert!(message.contains("`release-branch`"), "{message}");
    }

    #[test]
    fn exclusions_need_discovery() {
        let message = error("[cargo]\nexclude = [\"xtask\"]\n");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("`cargo.discover`"), "{message}");
    }

    #[test]
    fn line_index() {
        let lines = LineIndex::new("a\nbc\n\nd");
        assert_eq!(
            [0, 1, 2, 4, 5, 6].map(|offset| lines.line(offset)),
            [1, 1, 2, 2, 3, 4]
        );
    }
}
