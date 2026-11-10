//! Prerelease mode, `.cutwater/pre.toml`. While it is recorded, releases are
//! prereleases `<next>-<label>.<n>`, and the file keeps, per unit, the final
//! version its prereleases lead to, how many prereleases of it were made,
//! and the changeset entries they applied, so that the final release's
//! changelog carries all of them.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::changeset::Bump;

/// The prerelease file, relative to the repository root.
pub const PRE_FILE: &str = ".cutwater/pre.toml";

/// Whether releases are prereleases, or the next release leaves
/// prerelease mode with final versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Pre,
    Exit,
}

/// The recorded prerelease mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreState {
    pub mode: Mode,
    /// The prerelease label, also the npm distribution tag.
    pub label: String,
    /// The units released during prerelease mode, by name.
    #[serde(default)]
    pub units: BTreeMap<String, PreUnit>,
}

/// A unit's prereleases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreUnit {
    /// The final version the unit's prereleases lead to.
    pub version: String,
    /// How many prereleases of `version` were made: the next one's number.
    pub prereleases: u64,
    /// The units whose new versions the prereleases named, which the final
    /// release names at their final versions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    /// The highest bump above patch those dependencies caused, such as a
    /// peer dependency's major, which places them in the final changelog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_bump: Option<String>,
    /// The changeset entries the prereleases applied, in order.
    #[serde(default, rename = "entry")]
    pub entries: Vec<PreEntry>,
}

/// A changeset entry applied by a prerelease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreEntry {
    /// The changeset file it came from.
    pub changeset: String,
    pub bump: String,
    pub text: String,
}

impl PreEntry {
    pub fn bump(&self) -> Result<Bump> {
        self.bump.parse().map_err(|problem| {
            anyhow::anyhow!("{PRE_FILE}: entry of {}: {problem}", self.changeset)
        })
    }
}

impl PreUnit {
    pub fn dependency_bump(&self) -> Result<Option<Bump>> {
        self.dependency_bump
            .as_deref()
            .map(|bump| {
                bump.parse()
                    .map_err(|problem| anyhow::anyhow!("{PRE_FILE}: `dependency_bump`: {problem}"))
            })
            .transpose()
    }
}

impl PreState {
    /// The prerelease state of the repository at `root`, `None` outside
    /// prerelease mode.
    pub fn load(root: &Path) -> Result<Option<PreState>> {
        let path = root.join(PRE_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("cannot read {PRE_FILE}"))?;
        Self::parse(&text).map(Some)
    }

    pub fn parse(text: &str) -> Result<PreState> {
        let state: PreState =
            toml::from_str(text).map_err(|err| anyhow::anyhow!("{PRE_FILE}: {err}"))?;
        check_label(&state.label).map_err(|problem| anyhow::anyhow!("{PRE_FILE}: {problem}"))?;
        for (name, unit) in &state.units {
            let version = semver::Version::parse(&unit.version).map_err(|err| {
                anyhow::anyhow!(
                    "{PRE_FILE}: unit `{name}`: `{}` is not a semantic version: {err}",
                    unit.version
                )
            })?;
            if !version.pre.is_empty() {
                bail!(
                    "{PRE_FILE}: unit `{name}`: `{}` is a prerelease; `version` is the final \
                     version the prereleases lead to",
                    unit.version
                );
            }
            for entry in &unit.entries {
                entry.bump()?;
            }
            unit.dependency_bump()?;
        }
        Ok(state)
    }

    pub fn render(&self) -> String {
        let mut text = String::from(
            "# Prerelease mode: `cutwater pre enter` and `cutwater pre exit` set the mode,\n\
             # `cutwater version` records the prereleases.\n\n",
        );
        text.push_str(&toml::to_string_pretty(self).expect("the prerelease state serializes"));
        text
    }
}

/// Whether `label` can name prereleases: one identifier of letters, digits
/// and hyphens that starts with a letter, so that `<next>-<label>.<n>` is a
/// version and the label a valid npm distribution tag.
pub fn check_label(label: &str) -> Result<(), String> {
    let valid = label.starts_with(|c: char| c.is_ascii_alphabetic())
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "`{label}` cannot label prereleases; use letters, digits and hyphens, starting with \
             a letter, such as `beta` or `rc`"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let state = PreState {
            mode: Mode::Pre,
            label: "beta".to_string(),
            units: BTreeMap::from([(
                "@acme/api".to_string(),
                PreUnit {
                    version: "1.5.0".to_string(),
                    prereleases: 2,
                    dependencies: vec!["@acme/core".to_string()],
                    dependency_bump: Some("major".to_string()),
                    entries: vec![PreEntry {
                        changeset: ".changeset/brave-lions-dance.md".to_string(),
                        bump: "minor".to_string(),
                        text: "Add the export endpoint.\n\nWith details.".to_string(),
                    }],
                },
            )]),
        };
        let text = state.render();
        expect_test::expect![[r##"
            # Prerelease mode: `cutwater pre enter` and `cutwater pre exit` set the mode,
            # `cutwater version` records the prereleases.

            mode = "pre"
            label = "beta"

            [units."@acme/api"]
            version = "1.5.0"
            prereleases = 2
            dependencies = ["@acme/core"]
            dependency_bump = "major"

            [[units."@acme/api".entry]]
            changeset = ".changeset/brave-lions-dance.md"
            bump = "minor"
            text = """
            Add the export endpoint.

            With details."""
        "##]]
        .assert_eq(&text);
        assert_eq!(PreState::parse(&text).unwrap(), state);
    }

    #[test]
    fn labels() {
        for label in ["beta", "rc", "next-2"] {
            assert!(check_label(label).is_ok(), "{label}");
        }
        for label in ["", "1", "beta.1", "Ünicode", "-x"] {
            assert!(check_label(label).is_err(), "{label}");
        }
    }

    #[test]
    fn problems() {
        let error = |text: &str| format!("{:#}", PreState::parse(text).unwrap_err());
        assert!(error("mode = \"pre\"\nlabel = \"beta.1\"\n").contains("`beta.1`"));
        assert!(error("mode = \"later\"\nlabel = \"beta\"\n").contains("later"));
        let message = error(
            "mode = \"pre\"\nlabel = \"beta\"\n[units.api]\nversion = \"1.5.0-beta.0\"\nprereleases = 1\n",
        );
        assert!(message.contains("is a prerelease"), "{message}");
    }
}
