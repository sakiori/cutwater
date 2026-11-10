//! The version manifest, `.cutwater/versions.toml`: the released version of
//! every unit. Each unit's entry is one line, so that the commit that last
//! changed it is the unit's release commit.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The manifest file, relative to the repository root.
pub const MANIFEST_FILE: &str = ".cutwater/versions.toml";

/// The released version of a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub version: semver::Version,
    /// Whether `init` took the version from the unit's files instead of a
    /// release of the tool: it counts as released, but was never tagged.
    pub seeded: bool,
}

/// The released versions of the units, by unit name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub units: BTreeMap<String, Entry>,
}

/// The file as it is written.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "cutwater version manifest",
    description = "The released version of every unit, in `.cutwater/versions.toml`. Only \
                   `cutwater init` and `cutwater version` write it."
)]
struct File {
    /// The released version of each unit, by unit name.
    #[serde(default)]
    units: BTreeMap<String, FileEntry>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "UnitEntry")]
enum FileEntry {
    /// The version the unit's last release published.
    Released(String),
    /// A version `init` took from the unit's files when the repository
    /// adopted cutwater. It counts as released but was never tagged; the
    /// unit's next release replaces the entry with an ordinary one.
    Seeded(Seeded),
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Seeded {
    /// The version the unit's files declared.
    version: String,
    #[schemars(extend("const" = true))]
    seeded: bool,
}

impl Manifest {
    /// The manifest of the repository at `root`; empty when the file does
    /// not exist, as before the first release.
    pub fn load(root: &Path) -> Result<Manifest> {
        let path = root.join(MANIFEST_FILE);
        if !path.exists() {
            return Ok(Manifest::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {MANIFEST_FILE}"))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Manifest> {
        let file: File =
            toml::from_str(text).map_err(|err| anyhow::anyhow!("{MANIFEST_FILE}: {err}"))?;
        let mut units = BTreeMap::new();
        let mut problems = Vec::new();
        for (name, entry) in file.units {
            let (version, seeded) = match entry {
                FileEntry::Released(version) => (version, false),
                FileEntry::Seeded(Seeded { version, seeded }) => {
                    if !seeded {
                        problems.push(format!(
                            "unit `{name}`: `seeded = false`; write a released version as \
                             `{} = \"{version}\"`",
                            key(&name)
                        ));
                        continue;
                    }
                    (version, true)
                }
            };
            match semver::Version::parse(&version) {
                Ok(version) => {
                    units.insert(name, Entry { version, seeded });
                }
                Err(err) => problems.push(format!(
                    "unit `{name}`: `{version}` is not a semantic version: {err}"
                )),
            }
        }
        if !problems.is_empty() {
            bail!("{MANIFEST_FILE}:\n  {}", problems.join("\n  "));
        }
        Ok(Manifest { units })
    }

    pub fn get(&self, unit: &str) -> Option<&Entry> {
        self.units.get(unit)
    }

    /// The text of the file: a comment, then the `[units]` table with one
    /// line per unit, ordered by name.
    pub fn render(&self) -> String {
        let mut text = String::from(
            "# The released version of every unit. Only `cutwater init` and `cutwater version`\n\
             # write this file.\n\n[units]\n",
        );
        for (name, entry) in &self.units {
            let version = toml_edit::Value::from(entry.version.to_string());
            if entry.seeded {
                text.push_str(&format!(
                    "{} = {{ version = {version}, seeded = true }}\n",
                    key(name)
                ));
            } else {
                text.push_str(&format!("{} = {version}\n", key(name)));
            }
        }
        text
    }
}

/// A TOML key for `name`, quoted when it needs quotes.
fn key(name: &str) -> String {
    toml_edit::Key::new(name).display_repr().into_owned()
}

/// The JSON Schema of the manifest file, as the repository publishes it.
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(File);
    let mut text = serde_json::to_string_pretty(&schema).expect("a schema serializes to JSON");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

    #[test]
    fn round_trip() {
        let mut manifest = Manifest::default();
        for (name, v, seeded) in [
            ("core", "1.4.2", false),
            ("@acme/web", "2.0.0-beta.1", false),
            ("odd \"name\"", "0.1.0", true),
            ("api", "1.0.0", true),
        ] {
            manifest.units.insert(
                name.to_string(),
                Entry {
                    version: version(v),
                    seeded,
                },
            );
        }
        let text = manifest.render();
        expect_test::expect![[r##"
            # The released version of every unit. Only `cutwater init` and `cutwater version`
            # write this file.

            [units]
            "@acme/web" = "2.0.0-beta.1"
            api = { version = "1.0.0", seeded = true }
            core = "1.4.2"
            'odd "name"' = { version = "0.1.0", seeded = true }
        "##]]
        .assert_eq(&text);
        assert_eq!(Manifest::parse(&text).unwrap(), manifest);
    }

    #[test]
    fn empty_file() {
        assert_eq!(Manifest::parse("").unwrap(), Manifest::default());
        assert_eq!(
            Manifest::parse(&Manifest::default().render()).unwrap(),
            Manifest::default()
        );
    }

    #[test]
    fn problems() {
        let message = format!(
            "{:#}",
            Manifest::parse(
                "[units]\napi = \"1.4\"\ncore = { version = \"1.0.0\", seeded = false }\n"
            )
            .unwrap_err()
        );
        assert!(
            message.contains("unit `api`: `1.4` is not a semantic version"),
            "{message}"
        );
        assert!(
            message.contains("unit `core`: `seeded = false`"),
            "{message}"
        );
        let message = format!(
            "{:#}",
            Manifest::parse("[units]\napi = { version = \"1.0.0\", tagged = true }\n").unwrap_err()
        );
        assert!(message.contains(MANIFEST_FILE), "{message}");
        let message = format!("{:#}", Manifest::parse("units = 1\n").unwrap_err());
        assert!(message.contains("line 1"), "{message}");
    }
}
