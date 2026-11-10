//! Changeset files: `.changeset/<name>.md`, in the format of Changesets. The
//! YAML front matter maps unit names, quoted or not, to bumps, and the body
//! is the entry for the release notes.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use crate::git::Git;
use crate::units::Units;

/// The directory of changeset files, relative to the repository root.
pub const CHANGESET_DIR: &str = ".changeset";

/// The version bump a changeset asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    /// Releases nothing: the unit is named, but needs no release.
    None,
    Patch,
    Minor,
    Major,
}

impl Bump {
    pub const ALL: [Bump; 4] = [Bump::Patch, Bump::Minor, Bump::Major, Bump::None];

    pub fn as_str(self) -> &'static str {
        match self {
            Bump::None => "none",
            Bump::Patch => "patch",
            Bump::Minor => "minor",
            Bump::Major => "major",
        }
    }
}

impl fmt::Display for Bump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Bump {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Bump::ALL
            .into_iter()
            .find(|bump| bump.as_str() == value)
            .ok_or_else(|| format!("unknown bump `{value}`; use major, minor, patch or none"))
    }
}

/// A changeset file as written: the bump of each unit it names, unchecked,
/// and its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changeset {
    /// The file's path relative to the repository root.
    pub file: String,
    pub releases: BTreeMap<String, String>,
    pub entry: String,
}

/// Why a file is not a changeset.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("{file}: it does not start with front matter, a `---` line")]
    MissingFrontMatter { file: String },
    #[error("{file}: its front matter does not end with a `---` line")]
    UnclosedFrontMatter { file: String },
    #[error("{file}: its front matter must map unit names to bumps: {message}")]
    FrontMatter { file: String, message: String },
}

impl Changeset {
    /// Parses the text of the changeset file at `file`.
    pub fn parse(file: &str, text: &str) -> Result<Changeset, ParseError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines = text.split_inclusive('\n').enumerate();
        let opening = lines
            .by_ref()
            .find(|(_, line)| !line.trim().is_empty())
            .filter(|(_, line)| line.trim() == "---");
        let Some((opening, _)) = opening else {
            return Err(ParseError::MissingFrontMatter {
                file: file.to_string(),
            });
        };
        let mut front_matter = String::new();
        let mut closed = false;
        let mut entry = String::new();
        for (_, line) in lines {
            if closed {
                entry.push_str(line);
            } else if line.trim() == "---" {
                closed = true;
            } else {
                front_matter.push_str(line);
            }
        }
        if !closed {
            return Err(ParseError::UnclosedFrontMatter {
                file: file.to_string(),
            });
        }
        let releases: Option<BTreeMap<String, String>> = serde_saphyr::from_str(&front_matter)
            .map_err(|err| ParseError::FrontMatter {
                file: file.to_string(),
                message: err.render_with_options(serde_saphyr::render_options! {
                    line_offset: opening as u64 + 1,
                    snippets: serde_saphyr::SnippetMode::Off,
                }),
            })?;
        Ok(Changeset {
            file: file.to_string(),
            releases: releases.unwrap_or_default(),
            entry: entry.trim().to_string(),
        })
    }

    /// Whether the changeset names no unit, and so releases nothing.
    pub fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }

    /// Whether the changeset names `unit`, with any bump.
    pub fn names(&self, unit: &str) -> bool {
        self.releases.contains_key(unit)
    }

    /// What makes the changeset invalid for the repository's units: each
    /// unit it names that does not exist and each unknown bump.
    pub fn problems(&self, units: &Units) -> Vec<String> {
        let mut problems = Vec::new();
        for (unit, bump) in &self.releases {
            if units.get(unit).is_none() {
                problems.push(format!(
                    "{}: names `{unit}` with `{bump}`, but `{unit}` is not a unit of this repository",
                    self.file
                ));
            } else if let Err(problem) = bump.parse::<Bump>() {
                problems.push(format!("{}: unit `{unit}`: {problem}", self.file));
            }
        }
        problems
    }

    /// The text of a changeset file that gives each unit its bump.
    pub fn render(releases: &[(String, Bump)], entry: &str) -> String {
        let mut text = String::from("---\n");
        for (unit, bump) in releases {
            text.push_str(&format!("{}: {bump}\n", quoted(unit)));
        }
        text.push_str("---\n");
        let entry = entry.trim();
        if !entry.is_empty() {
            text.push('\n');
            text.push_str(entry);
            text.push('\n');
        }
        text
    }
}

/// A YAML double-quoted scalar, as Changesets writes unit names.
fn quoted(name: &str) -> String {
    let mut out = String::from("\"");
    for c in name.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Markdown files in `.changeset/` that Changesets does not read as
/// changesets, besides any README.md; `@changesets/read` keeps this list.
const NOT_CHANGESETS: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "GEMINI.md"];

/// Whether `path`, relative to the repository root, is a changeset file as
/// Changesets finds them: a Markdown file directly in `.changeset/` whose
/// name starts with no dot, other than a README.md in any case and the
/// agent instruction files.
pub fn is_changeset_path(path: &str) -> bool {
    path.strip_prefix(CHANGESET_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| {
            !name.contains('/')
                && !name.starts_with('.')
                && name.ends_with(".md")
                && !name.eq_ignore_ascii_case("README.md")
                && !NOT_CHANGESETS.contains(&name)
        })
}

/// The changesets pending in commit `rev`, ordered by file. Fails listing
/// every file that is not a valid changeset.
pub fn pending_at(git: &Git, rev: &str) -> Result<Vec<Changeset>> {
    let listing = git.run(&["ls-tree", "-z", rev, "--", &format!("{CHANGESET_DIR}/")])?;
    let mut changesets = Vec::new();
    let mut problems = Vec::new();
    for entry in listing.split('\0').filter(|entry| !entry.is_empty()) {
        let Some((meta, file)) = entry.split_once('\t') else {
            continue;
        };
        let regular_file = meta.starts_with("100644 ") || meta.starts_with("100755 ");
        if !regular_file || !is_changeset_path(file) {
            continue;
        }
        let text = git.run(&["cat-file", "blob", &format!("{rev}:{file}")])?;
        match Changeset::parse(file, &text) {
            Ok(changeset) => changesets.push(changeset),
            Err(err) => problems.push(err.to_string()),
        }
    }
    if !problems.is_empty() {
        bail!("invalid changesets:\n  {}", problems.join("\n  "));
    }
    changesets.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(changesets)
}

/// Fails listing every problem of the pending changesets.
pub fn validate(changesets: &[Changeset], units: &Units) -> Result<()> {
    let problems: Vec<String> = changesets
        .iter()
        .flat_map(|changeset| changeset.problems(units))
        .collect();
    if !problems.is_empty() {
        bail!("invalid changesets:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Writes a new changeset file with a generated name into the repository at
/// `root` and returns its path relative to the root.
pub fn create(root: &Path, releases: &[(String, Bump)], entry: &str) -> Result<String> {
    let dir = root.join(CHANGESET_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {CHANGESET_DIR}/"))?;
    let text = Changeset::render(releases, entry);
    for _ in 0..100 {
        let name = petname::petname(3, "-").context("cannot generate a changeset name")?;
        let file = format!("{CHANGESET_DIR}/{name}.md");
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(&file));
        match created {
            Ok(mut handle) => {
                use std::io::Write;
                handle
                    .write_all(text.as_bytes())
                    .with_context(|| format!("cannot write {file}"))?;
                return Ok(file);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err).with_context(|| format!("cannot create {file}")),
        }
    }
    bail!("cannot find an unused changeset name in {CHANGESET_DIR}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Changeset {
        Changeset::parse(".changeset/x.md", text).unwrap()
    }

    fn releases(changeset: &Changeset) -> Vec<(&str, &str)> {
        changeset
            .releases
            .iter()
            .map(|(unit, bump)| (unit.as_str(), bump.as_str()))
            .collect()
    }

    #[test]
    fn changesets_file_with_quoted_keys() {
        let changeset = parse(
            "---\n\"@acme/web\": minor\n'@acme/api': patch\ncore: none\n---\n\nAdd the export endpoint.\n\nWith details.\n",
        );
        assert_eq!(
            releases(&changeset),
            [
                ("@acme/api", "patch"),
                ("@acme/web", "minor"),
                ("core", "none")
            ]
        );
        assert_eq!(changeset.entry, "Add the export endpoint.\n\nWith details.");
    }

    #[test]
    fn empty_front_matter() {
        for text in [
            "---\n---\n",
            "---\n---",
            "\n---\n# nothing to release\n---\n\nDocs only.\r\n",
        ] {
            let changeset = parse(text);
            assert!(changeset.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn crlf_and_byte_order_mark() {
        let changeset = parse("\u{feff}---\r\napi: major\r\n---\r\nBreaking.\r\n");
        assert_eq!(releases(&changeset), [("api", "major")]);
        assert_eq!(changeset.entry, "Breaking.");
    }

    #[test]
    fn malformed_files() {
        let parse = |text| Changeset::parse(".changeset/x.md", text).unwrap_err();
        assert!(matches!(
            parse("api: minor\n"),
            ParseError::MissingFrontMatter { .. }
        ));
        assert!(matches!(
            parse("---\napi: minor\n"),
            ParseError::UnclosedFrontMatter { .. }
        ));
        let ParseError::FrontMatter { message, .. } = parse("---\napi: minor\napi: patch\n---\n")
        else {
            panic!("a duplicate key is a front matter error");
        };
        assert!(message.contains("line 3"), "{message}");
        assert!(matches!(
            parse("---\napi: [minor]\n---\n"),
            ParseError::FrontMatter { .. }
        ));
    }

    #[test]
    fn rendered_changesets_parse_back() {
        let wanted = [
            ("@acme/web".to_string(), Bump::Minor),
            ("odd \"name\"".to_string(), Bump::Patch),
        ];
        let text = Changeset::render(&wanted, "  Add the export endpoint.\n");
        assert!(text.starts_with("---\n\"@acme/web\": minor\n"), "{text}");
        let changeset = parse(&text);
        assert_eq!(
            releases(&changeset),
            [("@acme/web", "minor"), ("odd \"name\"", "patch")]
        );
        assert_eq!(changeset.entry, "Add the export endpoint.");
        assert_eq!(Changeset::render(&[], ""), "---\n---\n");
    }

    #[test]
    fn changeset_paths() {
        assert!(is_changeset_path(".changeset/brave-lions-dance.md"));
        for name in [
            "README.md",
            "readme.md",
            "ReadMe.md",
            "AGENTS.md",
            "CLAUDE.md",
            "GEMINI.md",
            ".draft.md",
            "notes.txt",
        ] {
            assert!(!is_changeset_path(&format!(".changeset/{name}")), "{name}");
        }
        assert!(is_changeset_path(".changeset/agents.md"));
        assert!(!is_changeset_path(".changeset/config.json"));
        assert!(!is_changeset_path(".changeset/drafts/x.md"));
        assert!(!is_changeset_path("docs/.changeset/x.md"));
    }

    #[test]
    fn bumps() {
        assert_eq!("none".parse(), Ok(Bump::None));
        assert!("Minor".parse::<Bump>().is_err());
    }
}
