//! `init`: adopt cutwater. It creates `.cutwater/config.toml`, turning on
//! the discovery of every ecosystem it finds and importing the settings of
//! Changesets, and seeds the version manifest with each unit's version
//! that its registry holds.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::adapter::{AdapterKind, DiscoverRequest, PublishedRequest, UnitRef, VersionQuery};
use crate::changeset::CHANGESET_DIR;
use crate::cli::InitArgs;
use crate::commands::Repository;
use crate::config::{CONFIG_FILE, Config};
use crate::git::Git;
use crate::manifest::{self, MANIFEST_FILE, Manifest};
use crate::units::Units;

const CHANGESETS_CONFIG: &str = ".changeset/config.json";

/// The settings of `.changeset/config.json` that `init` imports or refuses.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangesetsConfig {
    #[serde(default)]
    fixed: Vec<Vec<String>>,
    #[serde(default)]
    linked: Vec<Vec<String>>,
    #[serde(default)]
    ignore: Vec<String>,
    base_branch: Option<String>,
    /// The settings `init` does not carry over, by name.
    #[serde(skip)]
    others: Vec<String>,
}

/// The settings of `.changeset/config.json` that `init` reads; any other
/// but the schema reference is named as not carried over.
const CHANGESETS_IMPORTED: [&str; 5] = ["fixed", "linked", "ignore", "baseBranch", "$schema"];

/// What `init` writes into the configuration.
struct Settings<'a> {
    default_branch: Option<&'a str>,
    discovered: &'a BTreeMap<AdapterKind, Vec<String>>,
    excluded: &'a [String],
    /// The tag template of each adapter's units, written out so that a unit
    /// added later does not change the tags of the others.
    adapter_tags: &'a BTreeMap<AdapterKind, &'static str>,
    /// The units tagged `v<version>`, with that template of their own.
    unit_tags: &'a [(String, &'static str)],
    groups: &'a [(String, Vec<String>, String)],
}

pub fn run(args: &InitArgs) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let git = Git::discover(&cwd)?;
    let root = git.root().to_path_buf();
    if root.join(CONFIG_FILE).exists() {
        bail!("{CONFIG_FILE} already exists, so this repository is set up for cutwater already");
    }
    let changesets = read_changesets_config(&root)?;

    let mut discovered: BTreeMap<AdapterKind, Vec<String>> = BTreeMap::new();
    for kind in AdapterKind::ALL {
        let discovery = kind
            .adapter()
            .discover(&DiscoverRequest { root: root.clone() })?;
        if !discovery.units.is_empty() {
            discovered.insert(
                kind,
                discovery.units.into_iter().map(|unit| unit.name).collect(),
            );
        }
    }
    if discovered.is_empty() {
        bail!(
            "found no publishable npm package or Cargo crate; list the repository's units in \
             {CONFIG_FILE} by hand"
        );
    }

    let mut notes = Vec::new();
    let mut excluded = Vec::new();
    let mut groups: Vec<(String, Vec<String>, String)> = Vec::new();
    if let Some(changesets) = &changesets {
        let npm = discovered
            .get(&AdapterKind::Npm)
            .cloned()
            .unwrap_or_default();
        let ignore = glob_set(&changesets.ignore)?;
        excluded = npm
            .iter()
            .filter(|name| ignore.is_match(name.as_str()))
            .cloned()
            .collect();
        for patterns in &changesets.fixed {
            // The members in the order of the group's patterns; the first
            // names the group.
            let mut members: Vec<String> = Vec::new();
            for pattern in patterns {
                let set = glob_set(std::slice::from_ref(pattern))?;
                let mut matched: Vec<&String> = npm
                    .iter()
                    .filter(|name| set.is_match(name.as_str()) && !excluded.contains(name))
                    .filter(|name| !members.contains(name))
                    .collect();
                matched.sort();
                members.extend(matched.into_iter().cloned());
            }
            if members.len() < 2 {
                notes.push(format!(
                    "The fixed group [{}] of {CHANGESETS_CONFIG} has fewer than two publishable \
                     packages, so it is not imported.",
                    patterns.join(", ")
                ));
                continue;
            }
            groups.push((members[0].clone(), members, "{group}@{version}".to_string()));
        }
    }
    let imported_groups = groups.len();
    let count = discovered.values().map(Vec::len).sum::<usize>() - excluded.len();
    // Each adapter's units are tagged as several units are, by the tool's
    // template or, for the npm units of a repository on Changesets, by
    // its `<name>@<version>`. A unit tagged `v<version>`, as the
    // repository's only unit or as the package of a repository on
    // Changesets without workspaces, gets that tag of its own, so that a
    // unit added later does not change it.
    let mut adapter_tags = BTreeMap::new();
    for kind in discovered.keys() {
        let tag = match (kind, &changesets) {
            (AdapterKind::Npm, Some(_)) => "{unit}@{version}",
            _ => "{unit}-v{version}",
        };
        adapter_tags.insert(*kind, tag);
    }
    let single_package = changesets.is_some() && !is_npm_workspace(&root)?;
    let unit_tags: Vec<(String, &str)> = discovered
        .iter()
        .flat_map(|(kind, names)| names.iter().map(move |name| (*kind, name)))
        .filter(|(_, name)| !excluded.contains(name))
        .filter(|(kind, _)| match (kind, &changesets) {
            (AdapterKind::Npm, Some(_)) => single_package,
            _ => count == 1,
        })
        .map(|(_, name)| (name.clone(), "v{version}"))
        .collect();
    let default_branch = changesets
        .as_ref()
        .and_then(|changesets| changesets.base_branch.as_deref());
    let mut settings = Settings {
        default_branch,
        discovered: &discovered,
        excluded: &excluded,
        adapter_tags: &adapter_tags,
        unit_tags: &unit_tags,
        groups: &[],
    };

    // Units that share the place their version is written in, such as
    // crates inheriting `workspace.package.version`, become a fixed group.
    let text = render_config(&settings);
    let config = Config::parse(&text)?;
    let units = Units::resolve(&root, &config)?;
    let repo = Repository {
        git: git.clone(),
        config,
        units,
    };
    let inspected = repo.inspect()?;
    let mut sharing: BTreeMap<_, Vec<String>> = BTreeMap::new();
    for (name, declared) in &inspected.declared {
        sharing
            .entry(declared.location.clone())
            .or_default()
            .push(name.clone());
    }
    for (location, members) in sharing {
        if members.len() < 2 {
            continue;
        }
        let name = if groups.iter().any(|(group, _, _)| group == "workspace") {
            members[0].clone()
        } else {
            "workspace".to_string()
        };
        notes.push(format!(
            "Units {} share their version, written in {location}, so they form the fixed group \
             `{name}`.",
            members
                .iter()
                .map(|member| format!("`{member}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let tag = if members.len() == count {
            "v{version}"
        } else {
            "{group}-v{version}"
        };
        groups.push((name, members, tag.to_string()));
    }

    settings.groups = &groups;
    let text = render_config(&settings);
    let config = Config::parse(&text)?;
    let units = Units::resolve(&root, &config)?;
    let names: Vec<String> = units
        .iter()
        .map(|unit| format!("`{}`", unit.name))
        .collect();
    for (option, named) in [("--new", &args.new), ("--published", &args.published)] {
        if let Some(name) = named.iter().find(|name| units.get(name).is_none()) {
            bail!(
                "`{option}` names `{name}`, which is not a unit of this repository; its units \
                 are {}",
                names.join(", ")
            );
        }
    }
    if let Some(name) = args.new.iter().find(|name| args.published.contains(name)) {
        bail!("`--new` and `--published` both name `{name}`; a unit is one or the other");
    }
    let seed = |manifest: &mut Manifest, unit: &str| -> Result<()> {
        let declared = &inspected.declared[unit];
        let version = semver::Version::parse(&declared.version).with_context(|| {
            format!(
                "{}: the version of unit `{unit}`, `{}`",
                declared.location, declared.version
            )
        })?;
        manifest.units.insert(
            unit.to_string(),
            manifest::Entry {
                version,
                seeded: true,
            },
        );
        Ok(())
    };
    // A unit is seeded only with a version its registry lists, or that
    // `--published` vouches for: an unreachable, replaced or denying
    // registry looks like one without the package.
    let mut manifest = Manifest::default();
    for unit in &args.published {
        seed(&mut manifest, unit)?;
    }
    let mut problems = Vec::new();
    for kind in AdapterKind::ALL {
        let queries: Vec<VersionQuery> = units
            .iter()
            .filter(|unit| {
                unit.adapter == kind
                    && !args.new.contains(&unit.name)
                    && !args.published.contains(&unit.name)
            })
            .map(|unit| VersionQuery {
                unit: UnitRef {
                    name: unit.name.clone(),
                    path: unit.path.clone(),
                },
                version: inspected.declared[&unit.name].version.clone(),
            })
            .collect();
        if queries.is_empty() {
            continue;
        }
        let publications = kind.adapter().published(&PublishedRequest {
            root: root.clone(),
            queries,
        })?;
        for publication in publications.units {
            let unit = &publication.unit;
            if publication.published {
                seed(&mut manifest, unit)?;
                continue;
            }
            let declared = &inspected.declared[unit];
            let registry = &publication.registry;
            let version = &declared.version;
            let unlisted = match kind {
                AdapterKind::Cargo => "not listed, as a yanked version is",
                AdapterKind::Npm => "not listed",
            };
            problems.push(match (&publication.problem, &publication.highest) {
                // The tool did not answer, so nothing says whether the unit
                // was published.
                (Some(problem), _) => format!(
                    "unit `{unit}`: {registry} could not be asked: {problem}\n  Fix the tool, \
                     the network or the configuration and run `cutwater init` again; if \
                     {version} is published, `--published {unit}` seeds it without asking."
                ),
                (None, Some(highest)) => format!(
                    "unit `{unit}`: {registry} does not list {version}, the version it declares \
                     in {}; the highest version it lists is {highest}. Pass `--published \
                     {unit}` if {version} is published but {unlisted}.",
                    declared.location
                ),
                (None, None) => format!(
                    "unit `{unit}`: {registry} lists no version of it. Pass `--new {unit}` if it \
                     was never published, or `--published {unit}` if {version} is published but \
                     {unlisted}."
                ),
            });
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }

    std::fs::create_dir_all(root.join(".cutwater")).context("cannot create .cutwater/")?;
    std::fs::write(root.join(CONFIG_FILE), &text)
        .with_context(|| format!("cannot write {CONFIG_FILE}"))?;
    std::fs::write(root.join(MANIFEST_FILE), manifest.render())
        .with_context(|| format!("cannot write {MANIFEST_FILE}"))?;

    if manifest.units.is_empty() {
        crate::outln!("Created {CONFIG_FILE} and {MANIFEST_FILE}.")?;
    } else {
        crate::outln!(
            "Created {CONFIG_FILE} and {MANIFEST_FILE}, with the released versions as seeded:"
        )?;
        for (name, entry) in &manifest.units {
            crate::outln!("  {name} {}", entry.version)?;
        }
    }
    if !args.new.is_empty() {
        crate::outln!(
            "New, without a release yet: {}. Their first release is the version their files \
             declare.",
            args.new.join(", ")
        )?;
    }
    if let Some(changesets) = &changesets {
        let quoted = |names: &[String]| {
            names
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut imported = Vec::new();
        if imported_groups > 0 {
            let names: Vec<String> = groups[..imported_groups]
                .iter()
                .map(|(name, _, _)| name.clone())
                .collect();
            imported.push(format!("the fixed groups {}", quoted(&names)));
        }
        if !excluded.is_empty() {
            imported.push(format!("the ignored packages {}", quoted(&excluded)));
        }
        if let Some(branch) = default_branch {
            imported.push(format!("the base branch `{branch}`"));
        }
        if imported.is_empty() {
            crate::outln!("{CHANGESETS_CONFIG} sets nothing cutwater imports.")?;
        } else {
            crate::outln!(
                "Imported from {CHANGESETS_CONFIG}: {}.",
                imported.join(", ")
            )?;
        }
        if let Some(tag) = adapter_tags.get(&AdapterKind::Npm) {
            match unit_tags
                .iter()
                .find(|(name, _)| discovered[&AdapterKind::Npm].contains(name))
            {
                Some((name, own)) if single_package => crate::outln!(
                    "Kept the tag format of Changesets: `{own}` for `{name}`, and `{tag}` for \
                     npm units added later."
                )?,
                _ => {
                    crate::outln!("Kept the tag format of Changesets for the npm units, `{tag}`.")?
                }
            }
        }
        crate::outln!("Pending changesets stay in {CHANGESET_DIR}/.")?;
        if !changesets.others.is_empty() {
            eprintln!(
                "warning: {CHANGESETS_CONFIG}: {} not carried over; cutwater has no such \
                 settings",
                changesets
                    .others
                    .iter()
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
                    + if changesets.others.len() == 1 {
                        " is"
                    } else {
                        " are"
                    }
            );
        }
    }
    for note in notes {
        crate::outln!("{note}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The Changesets configuration, `None` without one. Refuses what
/// cutwater cannot take over: prerelease mode and linked groups.
fn read_changesets_config(root: &std::path::Path) -> Result<Option<ChangesetsConfig>> {
    for path in [".changeset/pre.json", ".changeset/pre"] {
        if root.join(path).exists() {
            bail!(
                "{path} exists: Changesets is in prerelease mode. Leave it first, with \
                 `changeset pre exit` and `changeset version`, and run `cutwater init` once \
                 that has landed"
            );
        }
    }
    let path = root.join(CHANGESETS_CONFIG);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {CHANGESETS_CONFIG}"))?;
    let invalid = || format!("{CHANGESETS_CONFIG} is not a Changesets configuration");
    let value: serde_json::Value = serde_json::from_str(&text).with_context(invalid)?;
    let others: Vec<String> = value
        .as_object()
        .map(|object| {
            object
                .keys()
                .filter(|key| !CHANGESETS_IMPORTED.contains(&key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut config: ChangesetsConfig = serde_json::from_value(value).with_context(invalid)?;
    config.others = others;
    if config.linked.iter().any(|group| !group.is_empty()) {
        bail!(
            "{CHANGESETS_CONFIG} declares `linked` groups, which cutwater does not support; \
             remove them, or turn them into `fixed` groups, before running `cutwater init`"
        );
    }
    Ok(Some(config))
}

/// Package names and patterns, as Changesets matches them: `*` stays
/// within a scope.
fn glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        set.add(
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .with_context(|| format!("{CHANGESETS_CONFIG}: invalid pattern `{pattern}`"))?,
        );
    }
    set.build()
        .with_context(|| format!("{CHANGESETS_CONFIG}: invalid patterns"))
}

/// Whether the repository's npm packages form a workspace, as Changesets
/// tells: a pnpm workspace, or `workspaces` in the root `package.json`.
fn is_npm_workspace(root: &std::path::Path) -> Result<bool> {
    if root.join("pnpm-workspace.yaml").is_file() {
        return Ok(true);
    }
    let text =
        std::fs::read_to_string(root.join("package.json")).context("cannot read package.json")?;
    let manifest: serde_json::Value =
        serde_json::from_str(&text).context("package.json is not JSON")?;
    Ok(manifest.get("workspaces").is_some())
}

fn render_config(settings: &Settings) -> String {
    let Settings {
        default_branch,
        discovered,
        excluded,
        adapter_tags,
        unit_tags,
        groups,
    } = settings;
    let value = |text: &str| toml_edit::Value::from(text).to_string();
    let list = |items: &[String]| {
        let items: Vec<String> = items.iter().map(|item| value(item)).collect();
        format!("[{}]", items.join(", "))
    };
    let mut text = String::from(
        "# The settings of cutwater. Its JSON Schema, schema/config.schema.json in the\n\
         # cutwater repository, describes them.\n",
    );
    if let Some(branch) = default_branch {
        text.push_str(&format!("default-branch = {}\n", value(branch)));
    }
    for kind in discovered.keys() {
        text.push_str(&format!("\n[{kind}]\ndiscover = true\n"));
        if *kind == AdapterKind::Npm && !excluded.is_empty() {
            text.push_str(&format!("exclude = {}\n", list(excluded)));
        }
        if let Some(tag) = adapter_tags.get(kind) {
            text.push_str(&format!("tag = {}\n", value(tag)));
        }
    }
    for (name, tag) in unit_tags.iter() {
        text.push_str(&format!(
            "\n[[unit]]\nname = {}\ntag = {}\n",
            value(name),
            value(tag)
        ));
    }
    for (name, members, tag) in groups.iter() {
        text.push_str(&format!(
            "\n[[group]]\nname = {}\nunits = {}\n",
            value(name),
            list(members)
        ));
        text.push_str(&format!("tag = {}\n", value(tag)));
    }
    text
}
