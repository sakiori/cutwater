//! Changelogs in the layout Changesets writes with its default changelog
//! format, `@changesets/changelog-git`, without commit hashes, so that a
//! repository that adopts cutwater keeps one consistent file.

use crate::changeset::Bump;

/// A changeset's entry for one unit's changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub bump: Bump,
    pub text: String,
}

/// The changelog section of one release: the version heading, then the
/// release's [`body`].
pub fn section(
    version: &str,
    dependencies_bump: Bump,
    entries: &[Entry],
    dependencies: &[(String, String)],
) -> String {
    format!(
        "## {version}\n\n{}",
        body(dependencies_bump, entries, dependencies)
    )
}

/// The changes of one release: the entries grouped by bump, major first,
/// with the updated dependencies among the changes of `dependencies_bump`:
/// patch, as in Changesets, or a higher bump a dependency caused.
pub fn body(
    dependencies_bump: Bump,
    entries: &[Entry],
    dependencies: &[(String, String)],
) -> String {
    let mut parts = Vec::new();
    for (bump, title) in [
        (Bump::Major, "Major Changes"),
        (Bump::Minor, "Minor Changes"),
        (Bump::Patch, "Patch Changes"),
    ] {
        let mut lines: Vec<String> = entries
            .iter()
            .filter(|entry| entry.bump == bump)
            .map(|entry| item(&entry.text))
            .collect();
        if bump == dependencies_bump.max(Bump::Patch) && !dependencies.is_empty() {
            let mut item = String::from("- Updated dependencies");
            for (name, version) in dependencies {
                item.push_str(&format!("\n  - {name}@{version}"));
            }
            lines.push(item);
        }
        if !lines.is_empty() {
            parts.push(format!("### {title}\n\n{}", lines.join("\n")));
        }
    }
    if parts.is_empty() {
        parts.push("No changes in this release.".to_string());
    }
    parts.join("\n\n")
}

/// A changeset's entry as a list item: its first line after the bullet, the
/// others indented below it.
fn item(text: &str) -> String {
    let mut lines = text.trim().split('\n').map(str::trim_end);
    let mut item = format!("- {}", lines.next().unwrap_or_default());
    for line in lines {
        item.push('\n');
        if !line.is_empty() {
            item.push_str("  ");
            item.push_str(line);
        }
    }
    item
}

/// The changelog `existing`, or a new one titled `# <title>`, with
/// `section` before its first version heading, or else after its first
/// line, as Changesets places it.
pub fn prepend(existing: Option<&str>, title: &str, section: &str) -> String {
    let template = format!("\n\n{}\n", section.trim());
    let existing = existing.unwrap_or_default();
    if existing.is_empty() {
        return format!("# {title}{template}");
    }
    if let Some(at) = first_version_heading(existing) {
        return format!(
            "{}{}\n{}",
            &existing[..at],
            template.trim_start(),
            &existing[at..]
        );
    }
    match existing.find('\n') {
        Some(at) => format!("{}{template}{}", &existing[..at], &existing[at + 1..]),
        None => format!("{existing}{template}"),
    }
}

/// The offset of the first line that is a Markdown heading starting with a
/// version, `## 1.2.3`.
fn first_version_heading(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let rest = &line[hashes..];
        let after_space = rest.trim_start_matches([' ', '\t']);
        let digits = after_space.chars().take_while(char::is_ascii_digit).count();
        let is_heading = (1..=6).contains(&hashes)
            && after_space.len() < rest.len()
            && digits > 0
            && after_space[digits..].starts_with('.')
            && after_space[digits + 1..].starts_with(|c: char| c.is_ascii_digit());
        if is_heading {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(bump: Bump, text: &str) -> Entry {
        Entry {
            bump,
            text: text.to_string(),
        }
    }

    #[test]
    fn section_in_the_layout_of_changesets() {
        let text = section(
            "1.5.0",
            Bump::Patch,
            &[
                entry(Bump::Patch, "Fix the export of empty tables."),
                entry(
                    Bump::Minor,
                    "Add the export endpoint.\r\n\r\nIt streams CSV:\n\n```sh\ncurl /export\n```  ",
                ),
                entry(Bump::Minor, "Accept filters."),
            ],
            &[
                ("@acme/core".to_string(), "2.0.0".to_string()),
                ("@acme/ui".to_string(), "1.0.1".to_string()),
            ],
        );
        expect_test::expect![[r#"
            ## 1.5.0

            ### Minor Changes

            - Add the export endpoint.

              It streams CSV:

              ```sh
              curl /export
              ```
            - Accept filters.

            ### Patch Changes

            - Fix the export of empty tables.
            - Updated dependencies
              - @acme/core@2.0.0
              - @acme/ui@1.0.1"#]]
        .assert_eq(&text);
        assert_eq!(
            section("2.2.0", Bump::Patch, &[], &[]),
            "## 2.2.0\n\nNo changes in this release."
        );
        // A major release caused by a peer dependency lists it among the
        // major changes.
        assert_eq!(
            section(
                "1.0.0",
                Bump::Major,
                &[entry(Bump::Patch, "Fix.")],
                &[("web".to_string(), "2.0.0".to_string())]
            ),
            "## 1.0.0\n\n### Major Changes\n\n- Updated dependencies\n  - web@2.0.0\n\n\
             ### Patch Changes\n\n- Fix."
        );
    }

    #[test]
    fn prepending() {
        let new = "## 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.";
        assert_eq!(
            prepend(None, "@acme/api", new),
            "# @acme/api\n\n## 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.\n"
        );
        assert_eq!(prepend(Some(""), "api", new), prepend(None, "api", new));
        assert_eq!(
            prepend(
                Some("# api\n\nIntro.\n\n## 1.4.2\n\n### Patch Changes\n\n- Fix.\n"),
                "api",
                new
            ),
            "# api\n\nIntro.\n\n## 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.\n\n## 1.4.2\n\n### Patch Changes\n\n- Fix.\n"
        );
        assert_eq!(
            prepend(Some("# Changelog of api\n\nIntro.\n"), "api", new),
            "# Changelog of api\n\n## 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.\n\nIntro.\n"
        );
        assert_eq!(
            prepend(Some("# api"), "api", new),
            "# api\n\n## 1.5.0\n\n### Minor Changes\n\n- Add the export endpoint.\n"
        );
    }

    #[test]
    fn version_headings() {
        assert_eq!(first_version_heading("# a\n## 1.0.0\n"), Some(4));
        assert_eq!(first_version_heading("# a\n### v1.0.0\n#2.0\n"), None);
        assert_eq!(first_version_heading("####### 1.0\n#\t0.1.0"), Some(12));
    }
}
