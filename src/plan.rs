//! The version plan: the next version of every unit that the pending
//! changesets, the fixed groups, the dependencies between units and
//! prerelease mode release.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use semver::{Prerelease, Version};
use serde::Serialize;

use crate::adapter::{AdapterKind, Edge, EdgeKind, UnitVersion};
use crate::changelog;
use crate::changeset::{Bump, Changeset};
use crate::graph;
use crate::manifest::{self, Manifest};
use crate::prerelease::{Mode, PreEntry, PreState, PreUnit};
use crate::units::Units;

/// What the plan starts from.
pub struct Input<'a> {
    pub units: &'a Units,
    pub manifest: &'a Manifest,
    pub pre: Option<&'a PreState>,
    /// The pending changesets, valid for the units, ordered by file.
    pub changesets: &'a [Changeset],
    /// The version each unit's files declare.
    pub declared: &'a BTreeMap<String, UnitVersion>,
    /// The references to units, as the adapters report them.
    pub edges: &'a [Edge],
}

/// Judges by the ecosystem's rules whether a range, written in a file,
/// accepts a version.
pub type Accepts<'a> = dyn Fn(AdapterKind, &str, &str, &Version) -> Result<bool> + 'a;

/// The plan, and what applying it writes.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The releases, in the order of publication.
    pub releases: Vec<Release>,
    /// Every unit in the order of publication.
    pub order: Vec<String>,
    /// The manifest entries of units that no longer exist.
    pub removed: Vec<String>,
    /// The references whose range does not accept the new version of the
    /// unit they name, one per place they are written.
    pub references: Vec<ReferenceUpdate>,
    /// The manifest after the plan.
    pub manifest: Manifest,
    /// The prerelease state after the plan: `None` outside prerelease mode
    /// and after the release that leaves it.
    pub pre: Option<PreState>,
    /// Whether `version` has anything to do: a release. Changesets that
    /// release nothing and removed units wait for the next release.
    pub pending: bool,
}

/// A unit's release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    pub unit: String,
    pub adapter: AdapterKind,
    /// The released version; `None` for a new unit.
    pub current: Option<Version>,
    /// Whether the current version was seeded by `init`.
    pub seeded: bool,
    pub next: Version,
    /// The release type: the bump from the current version to the next,
    /// or for a new unit the bump its group or changesets give it.
    pub bump: Bump,
    pub reasons: Vec<Reason>,
    /// The changelog entries of the release.
    #[serde(skip)]
    pub entries: Vec<changelog::Entry>,
    /// The released units whose new version the unit's published package
    /// names, with those versions.
    #[serde(skip)]
    pub dependencies: Vec<(String, Version)>,
    /// The changelog section of the dependency updates: patch, or the bump
    /// a dependency caused when it is higher.
    #[serde(skip)]
    pub dependency_bump: Bump,
}

/// Why a unit is released.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Reason {
    /// Its own changesets, with their highest bump; for a new unit, they
    /// only give the entries of its first release.
    Changesets { bump: Bump, changesets: Vec<String> },
    /// Its fixed group releases.
    Group { group: String },
    /// Dependencies released at versions its ranges do not accept.
    Dependencies { dependencies: Vec<DependencyCause> },
    /// It leaves prerelease mode: its final release after its prereleases.
    Prerelease { label: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyCause {
    pub unit: String,
    pub version: Version,
    pub range: String,
    pub kind: EdgeKind,
}

/// A reference to rewrite so that its range accepts the dependency's new
/// version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceUpdate {
    pub edge: Edge,
    pub version: Version,
}

/// The next version of `version` for `bump`, as npm's `semver.inc` and
/// Changesets compute it: a bump of a prerelease that its final version
/// already covers releases that final version.
pub fn bump(version: &Version, bump: Bump) -> Version {
    let prerelease = !version.pre.is_empty();
    let mut next = Version::new(version.major, version.minor, version.patch);
    match bump {
        Bump::None => return version.clone(),
        Bump::Major => {
            if version.minor != 0 || version.patch != 0 || !prerelease {
                next = Version::new(version.major + 1, 0, 0);
            }
        }
        Bump::Minor => {
            if version.patch != 0 || !prerelease {
                next = Version::new(version.major, version.minor + 1, 0);
            }
        }
        Bump::Patch => {
            if !prerelease {
                next.patch += 1;
            }
        }
    }
    next
}

/// A unit's state while the plan is computed.
struct UnitPlan<'a> {
    name: &'a str,
    adapter: AdapterKind,
    current: Option<&'a manifest::Entry>,
    declared: Option<Version>,
    /// The highest bump of its own changesets, other than `none`.
    own: Option<Bump>,
    own_files: Vec<String>,
    entries: Vec<changelog::Entry>,
    /// The bump the plan gives it, `None` while it is not released.
    bump: Option<Bump>,
    /// The highest bump its dependencies cause.
    caused: Option<Bump>,
}

pub fn plan(input: &Input, accepts: &Accepts) -> Result<Plan> {
    let order = plannable(input.units, input.declared, input.edges)?;

    let pre = input.pre;
    let exiting = pre.is_some_and(|pre| pre.mode == Mode::Exit);
    let mut units: BTreeMap<&str, UnitPlan> = BTreeMap::new();
    for unit in input.units.iter() {
        // Files that lead to the next release, as a snapshot does, declare
        // that release without its prerelease part.
        let declared = input
            .declared
            .get(&unit.name)
            .map(|declared| {
                Version::parse(&declared.version)
                    .map(|version| {
                        if declared.holds_release {
                            version
                        } else {
                            final_version(&version)
                        }
                    })
                    .with_context(|| {
                        format!(
                            "{}: unit `{}`: `{}` is not a semantic version",
                            declared.location, unit.name, declared.version
                        )
                    })
            })
            .transpose()?;
        units.insert(
            &unit.name,
            UnitPlan {
                name: &unit.name,
                adapter: unit.adapter,
                current: input.manifest.get(&unit.name),
                declared,
                own: None,
                own_files: Vec::new(),
                entries: Vec::new(),
                bump: None,
                caused: None,
            },
        );
    }
    for changeset in input.changesets {
        for (name, bump) in &changeset.releases {
            let bump: Bump = bump.parse().map_err(|problem| {
                anyhow::anyhow!("{}: unit `{name}`: {problem}", changeset.file)
            })?;
            let Some(unit) = units.get_mut(name.as_str()) else {
                bail!(
                    "{}: `{name}` is not a unit of this repository",
                    changeset.file
                );
            };
            if bump == Bump::None {
                continue;
            }
            unit.own = unit.own.max(Some(bump));
            unit.own_files.push(changeset.file.clone());
            unit.entries.push(changelog::Entry {
                bump,
                text: changeset.entry.clone(),
            });
        }
    }
    for unit in units.values_mut() {
        if unit.own.is_some() {
            unit.bump = unit.own;
        }
    }
    if let Some(pre) = pre.filter(|_| exiting) {
        for name in pre.units.keys() {
            if let Some(unit) = units.get_mut(name.as_str()) {
                unit.bump = unit.bump.max(Some(Bump::Patch));
            }
        }
    }
    for (name, unit) in &units {
        if unit.bump.is_some() && unit.current.is_none() && unit.declared.is_none() {
            bail!("unit `{name}` has neither a released version nor a declared one");
        }
    }

    let label = pre.filter(|_| !exiting).map(|pre| pre.label.as_str());
    let mut versions;
    loop {
        // A fixed group releases all its members when one releases.
        for group in input.units.groups() {
            let bumps: Vec<Option<Bump>> = group
                .members
                .iter()
                .map(|member| units[member.as_str()].bump)
                .collect();
            if bumps.iter().all(Option::is_none) {
                continue;
            }
            for member in &group.members {
                let unit = units.get_mut(member.as_str()).expect("a member is a unit");
                unit.bump = unit.bump.max(Some(Bump::None));
            }
        }
        versions = next_versions(input, &units, pre, label);
        let mut changed = false;
        for edge in input.edges {
            let Some(dependent) = edge.dependent_unit.as_deref() else {
                continue;
            };
            let (Some(range), Some(version)) =
                (&edge.range, versions.get(edge.dependency.as_str()))
            else {
                continue;
            };
            let unit = &units[dependent];
            // A new unit releases only through its own changesets, and a
            // member of the dependency's own fixed group releases with it.
            if !edge.kind.releases_dependent()
                || unit.current.is_none()
                || same_group(input.units, dependent, &edge.dependency)
            {
                continue;
            }
            let adapter = units[edge.dependency.as_str()].adapter;
            if accepts(adapter, range, &edge.location.file, version)? {
                continue;
            }
            // A peer range that drops versions its consumers install, for a
            // minor or major release, makes the dependent's next release
            // major, as in Changesets; any other release outside a range
            // makes a patch.
            let dependency_bump = release_bump(input, &units, &edge.dependency);
            let caused = if edge.kind == EdgeKind::Peer
                && dependency_bump >= Bump::Minor
                && !accepts(adapter, range, &edge.location.file, &final_version(version))?
            {
                Bump::Major
            } else {
                Bump::Patch
            };
            let unit = units.get_mut(dependent).expect("a unit");
            unit.caused = unit.caused.max(Some(caused));
            if unit.bump.is_none_or(|bump| bump < caused) {
                unit.bump = Some(caused);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // The judgement of every reference to a released unit.
    let mut outside: Vec<(&Edge, &Version)> = Vec::new();
    let mut inside: Vec<(&Edge, &Version)> = Vec::new();
    for edge in input.edges {
        let (Some(range), Some(version)) = (&edge.range, versions.get(edge.dependency.as_str()))
        else {
            continue;
        };
        if accepts(
            units[edge.dependency.as_str()].adapter,
            range,
            &edge.location.file,
            version,
        )? {
            inside.push((edge, version));
        } else {
            outside.push((edge, version));
        }
    }

    // A final release replaces the prereleases that ranges written during
    // prerelease mode name: a range that accepts the unit's current
    // prerelease names one of its version, and takes the final version.
    let mut prerelease_ranges: Vec<(&Edge, &Version)> = Vec::new();
    for (edge, version) in &inside {
        let current = &units[edge.dependency.as_str()].current;
        let Some(current) = current.filter(|current| !current.version.pre.is_empty()) else {
            continue;
        };
        if edge.follows || !version.pre.is_empty() {
            continue;
        }
        let range = edge.range.as_deref().expect("a judged edge has a range");
        let adapter = units[edge.dependency.as_str()].adapter;
        if accepts(adapter, range, &edge.location.file, &current.version)? {
            prerelease_ranges.push((edge, version));
        }
    }
    let rewritten: Vec<(&Edge, &Version)> =
        outside.iter().chain(&prerelease_ranges).copied().collect();

    let mut releases = Vec::new();
    for name in &order {
        let Some(next) = versions.get(name.as_str()) else {
            continue;
        };
        let unit = &units[name.as_str()];
        if let Some(current) = unit.current
            && *next <= current.version
        {
            bail!(
                "unit `{name}` would be released at {next}, which is not above its released \
                 version {}{}",
                current.version,
                if label.is_some() {
                    "; choose a prerelease label that sorts after the current one"
                } else {
                    ""
                }
            );
        }
        let mut reasons = Vec::new();
        if let Some(bump) = unit.own {
            reasons.push(Reason::Changesets {
                bump,
                changesets: unit.own_files.clone(),
            });
        }
        let recorded = pre.and_then(|pre| pre.units.get(name.as_str()));
        let group = input.units.group_of(name);
        if let Some(group) = group {
            // The unit's own reasons alone would give it its version.
            let explained = unit
                .bump
                .filter(|bump| *bump >= Bump::Patch)
                .is_some_and(|bump| final_base(unit, bump, label.is_some()) == final_version(next));
            if !explained {
                reasons.push(Reason::Group {
                    group: group.name.clone(),
                });
            }
        }
        // A dependency in the unit's own fixed group releases with it.
        let causes: Vec<DependencyCause> = outside
            .iter()
            .filter(|(edge, _)| {
                edge.dependent_unit.as_deref() == Some(name.as_str())
                    && edge.kind.releases_dependent()
                    && !same_group(input.units, name, &edge.dependency)
            })
            .map(|(edge, version)| DependencyCause {
                unit: edge.dependency.clone(),
                version: (*version).clone(),
                range: edge.range.clone().unwrap_or_default(),
                kind: edge.kind,
            })
            .collect();
        if !causes.is_empty() && unit.current.is_some() {
            reasons.push(Reason::Dependencies {
                dependencies: causes,
            });
        }
        if let (true, Some(pre), Some(_)) = (exiting, pre, recorded) {
            reasons.push(Reason::Prerelease {
                label: pre.label.clone(),
            });
        }

        let mut entries = Vec::new();
        if exiting && let Some(recorded) = recorded {
            for entry in &recorded.entries {
                entries.push(changelog::Entry {
                    bump: entry.bump()?,
                    text: entry.text.clone(),
                });
            }
        }
        entries.extend(unit.entries.iter().cloned());

        // A published package names a dependency's new version when its
        // reference follows the version, or when it is rewritten.
        let mut dependencies: BTreeMap<String, Version> = BTreeMap::new();
        for (edge, version) in outside.iter().chain(&inside) {
            let names_it = edge.follows || rewritten.iter().any(|(other, _)| other == edge);
            if edge.dependent_unit.as_deref() == Some(name.as_str())
                && edge.kind != EdgeKind::Development
                && names_it
            {
                dependencies.insert(edge.dependency.clone(), (*version).clone());
            }
        }
        // The final release names the dependencies its prereleases
        // updated, at their final versions.
        if exiting && let Some(recorded) = recorded {
            for dependency in &recorded.dependencies {
                let version = versions.get(dependency.as_str()).cloned().or_else(|| {
                    input
                        .manifest
                        .get(dependency)
                        .map(|entry| entry.version.clone())
                });
                if let Some(version) = version {
                    dependencies.entry(dependency.clone()).or_insert(version);
                }
            }
        }

        // A final release places the dependency updates its prereleases
        // made where the bump they caused puts them.
        let mut dependency_bump = unit.caused.unwrap_or(Bump::Patch).max(Bump::Patch);
        if exiting && let Some(recorded) = recorded {
            dependency_bump =
                dependency_bump.max(recorded.dependency_bump()?.unwrap_or(Bump::Patch));
        }

        let mut bump = release_bump(input, &units, name).max(dependency_bump);
        if let Some(current) = unit.current {
            bump = bump.max(bump_between(&current.version, next));
        }
        bump = entries.iter().map(|entry| entry.bump).fold(bump, Bump::max);
        releases.push(Release {
            unit: name.clone(),
            adapter: unit.adapter,
            current: unit.current.map(|current| current.version.clone()),
            seeded: unit.current.is_some_and(|current| current.seeded),
            next: next.clone(),
            bump,
            dependency_bump,
            reasons,
            entries,
            dependencies: dependencies.into_iter().collect(),
        });
    }

    let mut references: Vec<ReferenceUpdate> = Vec::new();
    for (edge, version) in &rewritten {
        if edge.follows
            || references
                .iter()
                .any(|update| update.edge.location == edge.location)
        {
            continue;
        }
        references.push(ReferenceUpdate {
            edge: (*edge).clone(),
            version: (*version).clone(),
        });
    }

    let removed: Vec<String> = input
        .manifest
        .units
        .keys()
        .filter(|name| input.units.get(name).is_none())
        .cloned()
        .collect();
    let mut manifest = input.manifest.clone();
    for name in &removed {
        manifest.units.remove(name);
    }
    for release in &releases {
        manifest.units.insert(
            release.unit.clone(),
            manifest::Entry {
                version: release.next.clone(),
                seeded: false,
            },
        );
    }

    let pending = !releases.is_empty();
    let next_pre = match pre {
        None => None,
        Some(_) if exiting && pending => None,
        Some(pre) if exiting => Some(pre.clone()),
        Some(pre) => {
            let mut state = pre.clone();
            state
                .units
                .retain(|name, _| input.units.get(name).is_some());
            for release in &releases {
                let unit = &units[release.unit.as_str()];
                let base = final_version(&release.next);
                let recorded = state
                    .units
                    .entry(release.unit.clone())
                    .or_insert_with(|| PreUnit {
                        version: base.to_string(),
                        prereleases: 0,
                        dependencies: Vec::new(),
                        dependency_bump: None,
                        entries: Vec::new(),
                    });
                recorded.version = base.to_string();
                recorded.prereleases = prerelease_number(&release.next) + 1;
                for (file, entry) in unit.own_files.iter().zip(&unit.entries) {
                    recorded.entries.push(PreEntry {
                        changeset: file.clone(),
                        bump: entry.bump.to_string(),
                        text: entry.text.clone(),
                    });
                }
                for (dependency, _) in &release.dependencies {
                    if !recorded.dependencies.contains(dependency) {
                        recorded.dependencies.push(dependency.clone());
                    }
                }
                recorded.dependencies.sort();
                if release.dependency_bump > Bump::Patch {
                    let previous = recorded.dependency_bump()?.unwrap_or(Bump::Patch);
                    recorded.dependency_bump =
                        Some(previous.max(release.dependency_bump).to_string());
                }
            }
            Some(state)
        }
    };

    Ok(Plan {
        releases,
        order,
        removed,
        references,
        manifest,
        pre: next_pre,
        pending,
    })
}

/// The release type of a unit the plan releases: its group's highest bump,
/// at least a patch, or its own bump.
fn release_bump(input: &Input, units: &BTreeMap<&str, UnitPlan>, name: &str) -> Bump {
    let bump = |name: &str| units[name].bump.unwrap_or(Bump::None);
    match input.units.group_of(name) {
        Some(group) => group
            .members
            .iter()
            .map(|member| bump(member))
            .max()
            .unwrap_or(Bump::None)
            .max(Bump::Patch),
        None => bump(name),
    }
}

fn same_group(units: &Units, a: &str, b: &str) -> bool {
    units
        .group_of(a)
        .is_some_and(|group| group.members.iter().any(|member| member == b))
}

/// The units in the order of publication, when they can be planned. Fails
/// naming the edges of a cycle of the release graph, or units that share a
/// written version outside a fixed group.
pub fn plannable(
    units: &Units,
    declared: &BTreeMap<String, UnitVersion>,
    edges: &[Edge],
) -> Result<Vec<String>> {
    let order = graph::order(units, edges).map_err(|cycle| {
        let edges: Vec<String> = cycle
            .iter()
            .map(|edge| {
                format!(
                    "  {} -> {} ({}, {})",
                    edge.dependent_unit
                        .as_deref()
                        .unwrap_or(&edge.dependent_path),
                    edge.dependency,
                    edge.kind,
                    edge.location.file
                )
            })
            .collect();
        let advice = if cycle.iter().any(|edge| edge.published_first) {
            "Remove one of these dependencies. A fixed group does not help, because the \
             ecosystem's tool publishes its members one at a time too: cargo publishes a crate \
             only once the registry holds every dependency that names a version, development \
             dependencies included. Dropping the version of a development dependency leaves it \
             out."
        } else {
            "Remove one of these dependencies, or make the units a fixed group."
        };
        anyhow::anyhow!(
            "the units depend on each other in a cycle, so no order of publication exists:\n{}\n\
             {advice}",
            edges.join("\n")
        )
    })?;
    check_shared_versions(units, declared)?;
    Ok(order)
}

/// Units whose versions are written in one place share their version, so
/// they must be a fixed group.
fn check_shared_versions(units: &Units, declared: &BTreeMap<String, UnitVersion>) -> Result<()> {
    let mut by_location: BTreeMap<_, Vec<&str>> = BTreeMap::new();
    for (name, declared) in declared {
        by_location
            .entry(&declared.location)
            .or_default()
            .push(name.as_str());
    }
    let mut problems = Vec::new();
    for (location, names) in by_location {
        if names.len() < 2 {
            continue;
        }
        let grouped = units.group_of(names[0]).is_some_and(|group| {
            names
                .iter()
                .all(|name| group.members.iter().any(|member| member == name))
        });
        if !grouped {
            let names: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
            problems.push(format!(
                "units {} share their version, written in {location}; make them one fixed \
                 group in .cutwater/config.toml",
                names.join(", ")
            ));
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }
    Ok(())
}

/// The versions of the units the plan releases so far.
fn next_versions<'a>(
    input: &Input,
    units: &BTreeMap<&'a str, UnitPlan<'a>>,
    pre: Option<&PreState>,
    label: Option<&str>,
) -> BTreeMap<&'a str, Version> {
    let mut versions = BTreeMap::new();
    for group in input.units.groups() {
        let members: Vec<&UnitPlan> = group
            .members
            .iter()
            .map(|member| &units[member.as_str()])
            .filter(|unit| unit.bump.is_some())
            .collect();
        if members.is_empty() {
            continue;
        }
        let highest = members
            .iter()
            .filter_map(|unit| unit.bump)
            .max()
            .unwrap_or(Bump::None)
            .max(Bump::Patch);
        // A group with released members takes their next version; only a
        // group of new members starts from the versions they declare.
        let existing: Vec<&&UnitPlan> = members
            .iter()
            .filter(|unit| unit.current.is_some())
            .collect();
        let base = if existing.is_empty() {
            members
                .iter()
                .map(|unit| final_base(unit, highest, label.is_some()))
                .max()
        } else {
            existing
                .iter()
                .map(|unit| final_base(unit, highest, label.is_some()))
                .max()
        }
        .expect("a member releases");
        let number = members
            .iter()
            .map(|unit| counter(pre, unit.name, &base))
            .max()
            .unwrap_or_default();
        for unit in members {
            versions.insert(unit.name, finish(&base, label, number));
        }
    }
    for (name, unit) in units {
        if versions.contains_key(name) {
            continue;
        }
        if let Some(bump) = unit.bump {
            versions.insert(*name, next_version(unit, bump, pre, label));
        }
    }
    versions
}

/// A unit's next version for `bump` on its own.
fn next_version(
    unit: &UnitPlan,
    bump: Bump,
    pre: Option<&PreState>,
    label: Option<&str>,
) -> Version {
    let base = final_base(unit, bump.max(Bump::Patch), label.is_some());
    finish(&base, label, counter(pre, unit.name, &base))
}

/// The final version a unit's release leads to: its released version
/// bumped, or for a new unit the version its files declare, which in
/// prerelease mode leads to the final version of it.
fn final_base(unit: &UnitPlan, bump: Bump, prerelease_mode: bool) -> Version {
    match (unit.current, &unit.declared) {
        (Some(current), _) => self::bump(&current.version, bump),
        (None, Some(declared)) if prerelease_mode => final_version(declared),
        (None, Some(declared)) => declared.clone(),
        (None, None) => unreachable!("a released unit has a version"),
    }
}

/// The release of `base`: `<base>-<label>.<number>` in prerelease mode.
fn finish(base: &Version, label: Option<&str>, number: u64) -> Version {
    let mut version = base.clone();
    if let Some(label) = label {
        version.pre = Prerelease::new(&format!("{label}.{number}"))
            .expect("a checked label makes a prerelease");
    }
    version
}

/// The number of the next prerelease of `base` of unit `name`, counting
/// from 0 for each unit and final version.
fn counter(pre: Option<&PreState>, name: &str, base: &Version) -> u64 {
    pre.and_then(|pre| pre.units.get(name))
        .filter(|recorded| recorded.version == base.to_string())
        .map_or(0, |recorded| recorded.prereleases)
}

/// The number of the prerelease `version`, `<base>-<label>.<number>`.
fn prerelease_number(version: &Version) -> u64 {
    version
        .pre
        .as_str()
        .rsplit('.')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_default()
}

/// The bump from `from` to the final version of `to`.
fn bump_between(from: &Version, to: &Version) -> Bump {
    let to = final_version(to);
    if to.major != from.major {
        Bump::Major
    } else if to.minor != from.minor {
        Bump::Minor
    } else {
        Bump::Patch
    }
}

fn final_version(version: &Version) -> Version {
    Version::new(version.major, version.minor, version.patch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Location;
    use crate::config::Config;
    use crate::prerelease::PreUnit;

    /// Units `(name, released version or "" for a new unit, declared
    /// version)`, all npm, and fixed groups.
    struct Fixture {
        _root: tempfile::TempDir,
        units: Units,
        manifest: Manifest,
        declared: BTreeMap<String, UnitVersion>,
        edges: Vec<Edge>,
        changesets: Vec<Changeset>,
        pre: Option<PreState>,
    }

    fn fixture(units: &[(&str, &str, &str)], groups: &[&[&str]]) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let mut config = String::new();
        let mut manifest = Manifest::default();
        let mut declared = BTreeMap::new();
        for (name, released, version) in units {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
            config.push_str(&format!(
                "[[unit]]\nname = \"{name}\"\npath = \"{name}\"\nadapter = \"npm\"\n"
            ));
            if !released.is_empty() {
                manifest.units.insert(
                    name.to_string(),
                    manifest::Entry {
                        version: Version::parse(released).unwrap(),
                        seeded: false,
                    },
                );
            }
            declared.insert(
                name.to_string(),
                UnitVersion {
                    name: name.to_string(),
                    package: name.to_string(),
                    version: version.to_string(),
                    holds_release: true,
                    location: Location {
                        file: format!("{name}/package.json"),
                        keys: vec!["version".to_string()],
                    },
                },
            );
        }
        for (index, members) in groups.iter().enumerate() {
            let members: Vec<String> = members.iter().map(|m| format!("\"{m}\"")).collect();
            config.push_str(&format!(
                "[[group]]\nname = \"group{index}\"\nunits = [{}]\n",
                members.join(", ")
            ));
        }
        let units = Units::resolve(root.path(), &Config::parse(&config).unwrap()).unwrap();
        Fixture {
            _root: root,
            units,
            manifest,
            declared,
            edges: Vec::new(),
            changesets: Vec::new(),
            pre: None,
        }
    }

    impl Fixture {
        fn edge(mut self, dependent: &str, dependency: &str, kind: EdgeKind, range: &str) -> Self {
            self.edges.push(Edge {
                dependent_path: dependent.to_string(),
                dependent_unit: self.units.get(dependent).map(|unit| unit.name.clone()),
                dependency: dependency.to_string(),
                kind,
                reference: range.to_string(),
                range: Some(range.to_string()),
                follows: false,
                published_first: false,
                location: Location {
                    file: format!("{dependent}/package.json"),
                    keys: vec![kind.to_string(), dependency.to_string()],
                },
            });
            self
        }

        /// The ecosystem's tool needs the last edge's dependency published
        /// first.
        fn published_first(mut self) -> Self {
            self.edges.last_mut().unwrap().published_first = true;
            self
        }

        /// The last edge follows the dependency's version.
        fn following(mut self) -> Self {
            self.edges.last_mut().unwrap().follows = true;
            self
        }

        fn changeset(mut self, file: &str, releases: &[(&str, &str)], entry: &str) -> Self {
            self.changesets.push(Changeset {
                file: format!(".changeset/{file}.md"),
                releases: releases
                    .iter()
                    .map(|(unit, bump)| (unit.to_string(), bump.to_string()))
                    .collect(),
                entry: entry.to_string(),
            });
            self
        }

        fn pre(mut self, label: &str) -> Self {
            self.pre = Some(PreState {
                mode: Mode::Pre,
                label: label.to_string(),
                units: BTreeMap::new(),
            });
            self
        }

        fn plan(&self) -> Result<Plan> {
            let accepts =
                |kind: AdapterKind, range: &str, file: &str, version: &Version| -> Result<bool> {
                    Ok(kind.adapter().accepts(&crate::adapter::AcceptsRequest {
                        range: range.to_string(),
                        version: version.to_string(),
                        file: file.to_string(),
                    })?)
                };
            plan(
                &Input {
                    units: &self.units,
                    manifest: &self.manifest,
                    pre: self.pre.as_ref(),
                    changesets: &self.changesets,
                    declared: &self.declared,
                    edges: &self.edges,
                },
                &accepts,
            )
        }

        /// Applies the plan to the fixture's state, as `version` does: the
        /// manifest, the declared versions and prerelease mode advance and
        /// the changesets are consumed.
        fn apply(&mut self) -> Plan {
            let plan = self.plan().unwrap();
            self.manifest = plan.manifest.clone();
            for release in &plan.releases {
                self.declared.get_mut(&release.unit).unwrap().version = release.next.to_string();
            }
            self.pre = plan.pre.clone();
            self.changesets.clear();
            plan
        }
    }

    fn releases(plan: &Plan) -> Vec<String> {
        plan.releases
            .iter()
            .map(|release| {
                let current = release
                    .current
                    .as_ref()
                    .map_or("new".to_string(), ToString::to_string);
                let reasons: Vec<String> = release
                    .reasons
                    .iter()
                    .map(|reason| match reason {
                        Reason::Changesets { bump, .. } => bump.to_string(),
                        Reason::Group { group } => format!("group {group}"),
                        Reason::Dependencies { dependencies } => dependencies
                            .iter()
                            .map(|cause| {
                                format!("{} {} outside {}", cause.unit, cause.version, cause.range)
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                        Reason::Prerelease { label } => format!("leaves {label}"),
                    })
                    .collect();
                format!(
                    "{} {current} -> {} ({})",
                    release.unit,
                    release.next,
                    reasons.join("; ")
                )
            })
            .collect()
    }

    #[test]
    fn highest_bump_wins() {
        let plan = fixture(
            &[("api", "1.4.2", "1.4.2"), ("core", "1.0.0", "1.0.0")],
            &[],
        )
        .changeset("a", &[("api", "patch")], "Fix.")
        .changeset("b", &[("api", "minor"), ("core", "none")], "Add.")
        .plan()
        .unwrap();
        assert_eq!(releases(&plan), ["api 1.4.2 -> 1.5.0 (minor)"]);
        let api = &plan.releases[0];
        assert_eq!(
            api.entries,
            [
                changelog::Entry {
                    bump: Bump::Patch,
                    text: "Fix.".to_string()
                },
                changelog::Entry {
                    bump: Bump::Minor,
                    text: "Add.".to_string()
                },
            ]
        );
        assert!(plan.pending);
    }

    #[test]
    fn fixed_group_takes_the_highest_version() {
        let plan = fixture(
            &[
                ("web", "2.1.0", "2.1.0"),
                ("ui", "2.1.0", "2.1.0"),
                ("api", "1.0.0", "1.0.0"),
            ],
            &[&["web", "ui"]],
        )
        .changeset("a", &[("ui", "minor")], "New button.")
        .changeset("b", &[("web", "patch")], "Fix.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            [
                "ui 2.1.0 -> 2.2.0 (minor)",
                "web 2.1.0 -> 2.2.0 (patch; group group0)"
            ]
        );
    }

    #[test]
    fn releases_caused_by_dependencies() {
        let fixture = fixture(
            &[
                ("core", "1.2.0", "1.2.0"),
                ("api", "1.4.2", "1.4.2"),
                ("app", "3.0.0", "3.0.0"),
                ("cli", "0.1.0", "0.1.0"),
                ("docs", "1.0.0", "1.0.0"),
                ("web", "1.0.0", "1.0.0"),
                ("ui", "1.0.0", "1.0.0"),
            ],
            &[&["web", "ui"]],
        )
        .edge("api", "core", EdgeKind::Runtime, "^1.2.0")
        // The exact version, as `workspace:*` gives it.
        .edge("app", "api", EdgeKind::Peer, "1.4.2")
        .following()
        .edge("cli", "core", EdgeKind::Runtime, ">=1.0.0")
        .edge("docs", "core", EdgeKind::Development, "^1.2.0")
        .edge("ui", "api", EdgeKind::Optional, "=1.4.2")
        .edge("x", "core", EdgeKind::Runtime, "~1.2.0")
        .changeset("a", &[("core", "major")], "Drop the old API.");
        let plan = fixture.plan().unwrap();
        assert_eq!(
            releases(&plan),
            [
                // An exact peer range, as `workspace:*` gives it, does not
                // accept a patch, which gives a patch.
                "app 3.0.0 -> 3.0.1 (api 1.4.3 outside 1.4.2)",
                "core 1.2.0 -> 2.0.0 (major)",
                "api 1.4.2 -> 1.4.3 (core 2.0.0 outside ^1.2.0)",
                "ui 1.0.0 -> 1.0.1 (api 1.4.3 outside =1.4.2)",
                "web 1.0.0 -> 1.0.1 (group group0)",
            ]
        );
        // The references to rewrite: not `workspace:*`, which follows, and
        // also those of development dependencies and of packages that are
        // no units.
        let references: Vec<String> = plan
            .references
            .iter()
            .map(|update| {
                format!(
                    "{} -> {}@{}",
                    update.edge.dependent_path, update.edge.dependency, update.version
                )
            })
            .collect();
        assert_eq!(
            references,
            [
                "api -> core@2.0.0",
                "docs -> core@2.0.0",
                "ui -> api@1.4.3",
                "x -> core@2.0.0"
            ]
        );
        let api = plan
            .releases
            .iter()
            .find(|release| release.unit == "api")
            .unwrap();
        assert_eq!(
            api.dependencies,
            [("core".to_string(), Version::new(2, 0, 0))]
        );
        let app = plan
            .releases
            .iter()
            .find(|release| release.unit == "app")
            .unwrap();
        assert_eq!(
            app.dependencies,
            [("api".to_string(), Version::new(1, 4, 3))]
        );
    }

    #[test]
    fn dependency_release_of_a_group_member_releases_the_group() {
        let plan = fixture(
            &[
                ("core", "1.0.0", "1.0.0"),
                ("web", "2.1.0", "2.1.0"),
                ("ui", "2.1.0", "2.1.0"),
            ],
            &[&["web", "ui"]],
        )
        .edge("ui", "core", EdgeKind::Runtime, "^1.0.0")
        .changeset("a", &[("core", "major")], "Break.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            [
                "core 1.0.0 -> 2.0.0 (major)",
                "ui 2.1.0 -> 2.1.1 (core 2.0.0 outside ^1.0.0)",
                "web 2.1.0 -> 2.1.1 (group group0)",
            ]
        );
    }

    #[test]
    fn cycle_is_refused_with_its_edges() {
        let message = format!(
            "{:#}",
            fixture(&[("a", "1.0.0", "1.0.0"), ("b", "1.0.0", "1.0.0")], &[])
                .edge("a", "b", EdgeKind::Runtime, "^1.0.0")
                .edge("b", "a", EdgeKind::Runtime, "^1.0.0")
                .plan()
                .unwrap_err()
        );
        assert!(
            message
                .contains("  a -> b (runtime, a/package.json)\n  b -> a (runtime, b/package.json)"),
            "{message}"
        );
        assert!(
            message.contains("or make the units a fixed group"),
            "{message}"
        );
        // A fixed group does not break a cycle the ecosystem's tool needs
        // published in order.
        let message = format!(
            "{:#}",
            fixture(
                &[("core", "1.0.0", "1.0.0"), ("testkit", "1.0.0", "1.0.0")],
                &[&["core", "testkit"]]
            )
            .edge("core", "testkit", EdgeKind::Development, "1.0.0")
            .published_first()
            .edge("testkit", "core", EdgeKind::Runtime, "1.0.0")
            .published_first()
            .plan()
            .unwrap_err()
        );
        assert!(
            message.contains("core -> testkit (development")
                && message.contains("testkit -> core (runtime"),
            "{message}"
        );
        assert!(message.contains("A fixed group does not help"), "{message}");
        assert!(
            !message.contains("make the units a fixed group"),
            "{message}"
        );
    }

    #[test]
    fn order_inside_a_group() {
        let plan = fixture(
            &[("web", "2.1.0", "2.1.0"), ("ui", "2.1.0", "2.1.0")],
            &[&["web", "ui"]],
        )
        .edge("ui", "web", EdgeKind::Peer, "^2.0.0")
        .edge("web", "ui", EdgeKind::Runtime, "^2.0.0")
        .changeset("a", &[("web", "minor")], "Add.")
        .plan()
        .unwrap();
        assert_eq!(plan.order, ["ui", "web"]);
        assert_eq!(
            releases(&plan),
            [
                "ui 2.1.0 -> 2.2.0 (group group0)",
                "web 2.1.0 -> 2.2.0 (minor)"
            ]
        );
    }

    #[test]
    fn new_and_removed_units() {
        let mut fixture = fixture(
            &[
                ("api", "1.0.0", "1.0.0"),
                ("export", "", "0.1.0"),
                ("draft", "", "0.0.1"),
            ],
            &[],
        )
        .edge("export", "api", EdgeKind::Runtime, "^0.9.0")
        .changeset("a", &[("export", "major"), ("draft", "none")], "First.")
        .changeset("b", &[("api", "major")], "Break.");
        fixture.manifest.units.insert(
            "old".to_string(),
            manifest::Entry {
                version: Version::new(3, 0, 0),
                seeded: false,
            },
        );
        let plan = fixture.plan().unwrap();
        assert_eq!(
            releases(&plan),
            ["api 1.0.0 -> 2.0.0 (major)", "export new -> 0.1.0 (major)"]
        );
        assert_eq!(plan.removed, ["old"]);
        assert_eq!(
            plan.manifest.units.keys().collect::<Vec<_>>(),
            ["api", "export"]
        );
    }

    #[test]
    fn new_member_of_a_group() {
        // Its minor changeset counts toward the group's bump; the version
        // its files declare does not count.
        let plan = fixture(
            &[
                ("web", "2.1.0", "2.1.0"),
                ("ui", "2.1.0", "2.1.0"),
                ("icons", "", "0.1.0"),
            ],
            &[&["web", "ui", "icons"]],
        )
        .changeset("a", &[("icons", "minor")], "First icons.")
        .changeset("b", &[("ui", "patch")], "Fix.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            [
                "icons new -> 2.2.0 (minor; group group0)",
                "ui 2.1.0 -> 2.2.0 (patch; group group0)",
                "web 2.1.0 -> 2.2.0 (group group0)"
            ]
        );
        // Named alone, the new member releases its group with its bump.
        let plan = fixture(
            &[("web", "2.1.0", "2.1.0"), ("icons", "", "3.0.0")],
            &[&["web", "icons"]],
        )
        .changeset("a", &[("icons", "minor")], "First icons.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            [
                "icons new -> 2.2.0 (minor; group group0)",
                "web 2.1.0 -> 2.2.0 (group group0)"
            ]
        );
        // A group of new members starts from the versions they declare.
        let plan = fixture(
            &[("web", "", "1.0.0"), ("icons", "", "0.1.0")],
            &[&["web", "icons"]],
        )
        .changeset("a", &[("icons", "minor"), ("web", "minor")], "First.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            [
                "icons new -> 1.0.0 (minor; group group0)",
                "web new -> 1.0.0 (minor)"
            ]
        );
    }

    #[test]
    fn peer_range_excludes_the_new_major() {
        let plan = fixture(
            &[
                ("web", "1.3.0", "1.3.0"),
                ("ui", "0.4.1", "0.4.1"),
                ("api", "1.0.0", "1.0.0"),
            ],
            &[],
        )
        .edge("ui", "web", EdgeKind::Peer, "^1.0.0")
        .edge("api", "web", EdgeKind::Runtime, "^1.0.0")
        .changeset("a", &[("web", "major")], "Break.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            // A peer dependency does not order publication.
            [
                "ui 0.4.1 -> 1.0.0 (web 2.0.0 outside ^1.0.0)",
                "web 1.3.0 -> 2.0.0 (major)",
                "api 1.0.0 -> 1.0.1 (web 2.0.0 outside ^1.0.0)",
            ]
        );
        let ui = plan
            .releases
            .iter()
            .find(|release| release.unit == "ui")
            .unwrap();
        assert_eq!(
            ui.dependencies,
            [("web".to_string(), Version::new(2, 0, 0))]
        );
        // The peer's update goes under Major Changes, the runtime
        // dependency's under Patch Changes.
        assert_eq!(ui.dependency_bump, Bump::Major);
        let api = plan
            .releases
            .iter()
            .find(|release| release.unit == "api")
            .unwrap();
        assert_eq!(api.dependency_bump, Bump::Patch);
    }

    #[test]
    fn peer_prereleases() {
        let peer = |bump: &str| {
            fixture(&[("web", "1.3.0", "1.3.0"), ("ui", "0.4.1", "0.4.1")], &[])
                .edge("ui", "web", EdgeKind::Peer, "^1.0.0")
                .pre("beta")
                .changeset("a", &[("web", bump)], "Change.")
                .plan()
                .unwrap()
        };
        // The range accepts the minor prerelease's final version: a patch.
        assert_eq!(
            releases(&peer("minor")),
            [
                "ui 0.4.1 -> 0.4.2-beta.0 (web 1.4.0-beta.0 outside ^1.0.0)",
                "web 1.3.0 -> 1.4.0-beta.0 (minor)",
            ]
        );
        // A patch prerelease is outside every final range: a patch too.
        assert_eq!(
            releases(&peer("patch")),
            [
                "ui 0.4.1 -> 0.4.2-beta.0 (web 1.3.1-beta.0 outside ^1.0.0)",
                "web 1.3.0 -> 1.3.1-beta.0 (patch)",
            ]
        );
        // A major prerelease whose final version is outside: a major.
        let plan = peer("major");
        assert_eq!(
            releases(&plan),
            [
                "ui 0.4.1 -> 1.0.0-beta.0 (web 2.0.0-beta.0 outside ^1.0.0)",
                "web 1.3.0 -> 2.0.0-beta.0 (major)",
            ]
        );
        assert_eq!(plan.releases[0].bump, Bump::Major);
    }

    #[test]
    fn peer_range_inside_a_group_does_not_raise_the_group() {
        let plan = fixture(
            &[("web", "2.1.0", "2.1.0"), ("ui", "2.1.0", "2.1.0")],
            &[&["web", "ui"]],
        )
        .edge("ui", "web", EdgeKind::Peer, "~2.1.0")
        .changeset("a", &[("web", "minor")], "Add.")
        .plan()
        .unwrap();
        assert_eq!(
            releases(&plan),
            [
                "ui 2.1.0 -> 2.2.0 (group group0)",
                "web 2.1.0 -> 2.2.0 (minor)"
            ]
        );
        assert_eq!(plan.references.len(), 1);
    }

    #[test]
    fn seeded_entry_is_replaced() {
        let mut fixture = fixture(
            &[("api", "1.0.0", "1.0.0"), ("core", "2.0.0", "2.0.0")],
            &[],
        )
        .changeset("a", &[("api", "patch")], "Fix.");
        for entry in fixture.manifest.units.values_mut() {
            entry.seeded = true;
        }
        let plan = fixture.plan().unwrap();
        assert!(plan.releases[0].seeded);
        assert_eq!(
            plan.manifest.render().lines().skip(4).collect::<Vec<_>>(),
            [
                "api = \"1.0.1\"",
                "core = { version = \"2.0.0\", seeded = true }"
            ]
        );
    }

    #[test]
    fn nothing_pending() {
        let plan = fixture(&[("api", "1.0.0", "1.0.0")], &[]).plan().unwrap();
        assert!(!plan.pending && plan.releases.is_empty());
        // An empty changeset and a removed unit make no release.
        let mut fixture = fixture(&[("api", "1.0.0", "1.0.0")], &[]).changeset(
            "a",
            &[],
            "Delete the legacy package.",
        );
        fixture.manifest.units.insert(
            "legacy".to_string(),
            manifest::Entry {
                version: Version::new(3, 0, 0),
                seeded: false,
            },
        );
        let plan = fixture.plan().unwrap();
        assert!(!plan.pending && plan.releases.is_empty());
        assert_eq!(plan.removed, ["legacy"]);
    }

    #[test]
    fn prerelease_sequence() {
        let mut fixture = fixture(
            &[
                ("api", "1.4.2", "1.4.2"),
                ("core", "1.0.0", "1.0.0"),
                ("web", "2.0.0", "2.0.0"),
                ("ui", "2.0.0", "2.0.0"),
            ],
            &[&["web", "ui"]],
        )
        .edge("api", "core", EdgeKind::Runtime, "^1.0.0")
        .pre("beta")
        .changeset("a", &[("api", "minor")], "First.");
        assert_eq!(
            releases(&fixture.apply()),
            ["api 1.4.2 -> 1.5.0-beta.0 (minor)"]
        );
        fixture = fixture.changeset("b", &[("api", "minor"), ("ui", "patch")], "Second.");
        assert_eq!(
            releases(&fixture.apply()),
            [
                "api 1.5.0-beta.0 -> 1.5.0-beta.1 (minor)",
                "ui 2.0.0 -> 2.0.1-beta.0 (patch)",
                "web 2.0.0 -> 2.0.1-beta.0 (group group0)",
            ]
        );
        // A dependency's prerelease is outside a range of final versions.
        fixture = fixture.changeset("c", &[("core", "patch")], "Third.");
        assert_eq!(
            releases(&fixture.apply()),
            [
                "core 1.0.0 -> 1.0.1-beta.0 (patch)",
                "api 1.5.0-beta.1 -> 1.5.0-beta.2 (core 1.0.1-beta.0 outside ^1.0.0)",
            ]
        );
        // A higher final version counts from 0 again.
        fixture = fixture.changeset("d", &[("api", "major")], "Fourth.");
        assert_eq!(
            releases(&fixture.apply()),
            ["api 1.5.0-beta.2 -> 2.0.0-beta.0 (major)"]
        );
        let state = fixture.pre.clone().unwrap();
        assert_eq!(
            state.units["api"],
            PreUnit {
                version: "2.0.0".to_string(),
                prereleases: 1,
                dependencies: vec!["core".to_string()],
                dependency_bump: None,
                entries: ["a:minor:First.", "b:minor:Second.", "d:major:Fourth."]
                    .map(|entry| {
                        let [file, bump, text] = entry.splitn(3, ':').collect::<Vec<_>>()[..]
                        else {
                            unreachable!()
                        };
                        PreEntry {
                            changeset: format!(".changeset/{file}.md"),
                            bump: bump.to_string(),
                            text: text.to_string(),
                        }
                    })
                    .to_vec(),
            }
        );

        fixture.pre.as_mut().unwrap().mode = Mode::Exit;
        let plan = fixture.apply();
        assert_eq!(
            releases(&plan),
            [
                "core 1.0.1-beta.0 -> 1.0.1 (leaves beta)",
                "api 2.0.0-beta.0 -> 2.0.0 (leaves beta)",
                "ui 2.0.1-beta.0 -> 2.0.1 (leaves beta)",
                "web 2.0.1-beta.0 -> 2.0.1 (leaves beta)",
            ]
        );
        let api = plan
            .releases
            .iter()
            .find(|release| release.unit == "api")
            .unwrap();
        let texts: Vec<&str> = api
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect();
        assert_eq!(texts, ["First.", "Second.", "Fourth."]);
        // The prereleases updated `core`; the final release names its final
        // version.
        assert_eq!(
            api.dependencies,
            [("core".to_string(), Version::new(1, 0, 1))]
        );
        assert_eq!(plan.pre, None);
    }

    #[test]
    fn final_release_replaces_prerelease_ranges() {
        let mut fixture = fixture(
            &[
                ("web", "1.4.0-beta.0", "1.4.0-beta.0"),
                ("ui", "0.4.2-beta.0", "0.4.2-beta.0"),
            ],
            &[],
        )
        .edge("ui", "web", EdgeKind::Runtime, "^1.4.0-beta.0")
        .edge("x", "web", EdgeKind::Development, ">=1.4.0-beta.0")
        .edge("y", "web", EdgeKind::Runtime, "^1.0.0");
        let mut pre = PreState {
            mode: Mode::Exit,
            label: "beta".to_string(),
            units: BTreeMap::new(),
        };
        for (name, version) in [("web", "1.4.0"), ("ui", "0.4.2")] {
            pre.units.insert(
                name.to_string(),
                PreUnit {
                    version: version.to_string(),
                    prereleases: 1,
                    // `ui` took its prerelease only because `web` did.
                    dependencies: if name == "ui" {
                        vec!["web".to_string()]
                    } else {
                        Vec::new()
                    },
                    dependency_bump: None,
                    entries: Vec::new(),
                },
            );
        }
        fixture.pre = Some(pre);
        let plan = fixture.plan().unwrap();
        assert_eq!(
            releases(&plan),
            [
                "web 1.4.0-beta.0 -> 1.4.0 (leaves beta)",
                "ui 0.4.2-beta.0 -> 0.4.2 (leaves beta)",
            ]
        );
        let ui = &plan.releases[1];
        assert_eq!(
            ui.dependencies,
            [("web".to_string(), Version::new(1, 4, 0))]
        );
        let references: Vec<String> = plan
            .references
            .iter()
            .map(|update| {
                format!(
                    "{} {} -> {}",
                    update.edge.dependent_path,
                    update.edge.range.as_deref().unwrap(),
                    update.version
                )
            })
            .collect();
        assert_eq!(
            references,
            ["ui ^1.4.0-beta.0 -> 1.4.0", "x >=1.4.0-beta.0 -> 1.4.0"]
        );
    }

    #[test]
    fn prerelease_must_sort_above_the_current_version() {
        let mut fixture = fixture(&[("api", "1.5.0-rc.0", "1.5.0-rc.0")], &[])
            .pre("beta")
            .changeset("a", &[("api", "patch")], "Fix.");
        let message = format!("{:#}", fixture.plan().unwrap_err());
        assert!(
            message.contains("unit `api` would be released at 1.5.0-beta.0, which is not above its released version 1.5.0-rc.0"),
            "{message}"
        );
        fixture.pre = None;
        assert_eq!(
            releases(&fixture.plan().unwrap()),
            ["api 1.5.0-rc.0 -> 1.5.0 (patch)"]
        );
    }

    #[test]
    fn units_sharing_a_version_must_be_a_group() {
        let mut fixture = fixture(
            &[("api", "1.0.0", "1.0.0"), ("core", "1.0.0", "1.0.0")],
            &[],
        );
        for declared in fixture.declared.values_mut() {
            declared.location = Location {
                file: "Cargo.toml".to_string(),
                keys: vec![
                    "workspace".to_string(),
                    "package".to_string(),
                    "version".to_string(),
                ],
            };
        }
        let message = format!("{:#}", fixture.plan().unwrap_err());
        assert!(
            message.contains("units `api`, `core` share their version, written in Cargo.toml `workspace.package.version`"),
            "{message}"
        );
    }

    #[test]
    fn declared_version_that_is_no_version() {
        let message = format!(
            "{:#}",
            fixture(&[("api", "1.0.0", "1.0")], &[]).plan().unwrap_err()
        );
        assert!(
            message.contains(
                "api/package.json `version`: unit `api`: `1.0` is not a semantic version"
            ),
            "{message}"
        );
    }

    #[test]
    fn bumps_follow_npm() {
        let bumped =
            |version: &str, by: Bump| bump(&Version::parse(version).unwrap(), by).to_string();
        assert_eq!(bumped("1.4.2", Bump::Patch), "1.4.3");
        assert_eq!(bumped("1.4.2", Bump::Minor), "1.5.0");
        assert_eq!(bumped("1.4.2", Bump::Major), "2.0.0");
        assert_eq!(bumped("1.5.0-beta.1", Bump::Patch), "1.5.0");
        assert_eq!(bumped("1.5.0-beta.1", Bump::Minor), "1.5.0");
        assert_eq!(bumped("1.5.0-beta.1", Bump::Major), "2.0.0");
        assert_eq!(bumped("1.4.3-beta.0", Bump::Minor), "1.5.0");
        assert_eq!(bumped("2.0.0-rc.1", Bump::Major), "2.0.0");
        assert_eq!(bumped("1.0.0+build.1", Bump::Patch), "1.0.1");
    }
}
