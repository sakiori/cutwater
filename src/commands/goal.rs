//! `goal`: the checks of the release merge request's pipeline. The release
//! is what the merge request changes in the version manifest against its
//! target. It fails when a pending changeset names a unit the release
//! publishes, when the manifest and the units' files disagree, or when what
//! the adapters would publish fails their checks, and then runs the
//! repository checks.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::Repository;
use super::base::Base;
use super::check::{disagreements, print_disagreements};
use crate::adapter::{AdapterKind, PrepareRequest, VersionQuery, VersionWrite};
use crate::changeset::{self, Bump, Changeset};
use crate::checks::{PlanInput, ReleaseInput};
use crate::cli::GoalArgs;
use crate::config::CheckStep;
use crate::manifest::{MANIFEST_FILE, Manifest};
use crate::plan;
use crate::units::Units;

pub fn run(args: &GoalArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let base = Base::resolve(&repo.git, args.base.as_deref())?;
    let manifest = Manifest::load(repo.git.root())?;
    let target = match repo.git.file_at(&base.commit, MANIFEST_FILE)? {
        Some(text) => Manifest::parse(&text)
            .with_context(|| format!("{MANIFEST_FILE} in the target commit {}", base.commit))?,
        None => Manifest::default(),
    };
    let inspected = repo.inspect()?;
    let mut released = released(&repo.units, &target, &manifest);
    // A tree with a dependency cycle, which `check` fails, has no order.
    if let Ok(order) = plan::plannable(&repo.units, &inspected.declared, &inspected.edges) {
        released.sort_by_key(|release| order.iter().position(|unit| *unit == release.unit));
    }
    crate::outln!(
        "Comparing {MANIFEST_FILE} with {} ({}).",
        base.commit,
        base.origin
    )?;
    if released.is_empty() {
        crate::outln!("This merge request releases no unit.")?;
    } else {
        crate::outln!("This merge request releases:")?;
        for release in &released {
            let current = release.current.as_deref().unwrap_or("new");
            crate::outln!("  {} {current} -> {}", release.unit, release.next)?;
        }
    }

    let pending = changeset::pending_at(&repo.git, "HEAD")?;
    changeset::validate(&pending, &repo.units)?;
    let stale = stale(&pending, &released);
    let mut sound = stale.is_empty();
    if !stale.is_empty() {
        eprintln!(
            "error: these pending changesets name units this merge request releases, whose \
             published packages would then hold changes that no release accounts for:"
        );
        for (file, units) in &stale {
            eprintln!("  {file}: {}", units.join(", "));
        }
        eprintln!(
            "They go into the next release; the release merge request is updated once the \
             pipeline of the default branch runs `cutwater mr`."
        );
    }

    sound &= print_disagreements(&disagreements(&manifest, &inspected.declared));
    sound &= inspected.print_problems();

    let versions: Vec<VersionWrite> = manifest
        .units
        .iter()
        .map(|(unit, entry)| VersionWrite {
            unit: unit.clone(),
            version: entry.version.to_string(),
        })
        .collect();
    for kind in AdapterKind::ALL {
        let packages: Vec<VersionQuery> = released
            .iter()
            .filter(|release| release.adapter == kind)
            .map(|release| VersionQuery {
                unit: repo
                    .unit_refs(kind)
                    .into_iter()
                    .find(|unit| unit.name == release.unit)
                    .expect("a released unit is a unit of its adapter"),
                version: release.next.clone(),
            })
            .collect();
        if packages.is_empty() {
            continue;
        }
        let prepared = kind.adapter().prepare(&PrepareRequest {
            root: repo.git.root().to_path_buf(),
            units: repo.unit_refs(kind),
            packages,
            released: versions.clone(),
        })?;
        if prepared.problems.is_empty() {
            crate::outln!("The {kind} packages of the release pass the {kind} adapter's checks.")?;
            continue;
        }
        sound = false;
        eprintln!("error: what the {kind} adapter would publish fails its checks:");
        for problem in &prepared.problems {
            eprintln!("  {problem}");
        }
    }

    let checked = repo.repository_checks(
        CheckStep::Goal,
        &manifest,
        &inspected,
        PlanInput { releases: released },
    )?;
    Ok(if sound && checked {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The releases a merge request makes: the units whose manifest entry it
/// changes against the target, other than to a seeded one, in the order of
/// the units.
pub fn released(units: &Units, target: &Manifest, head: &Manifest) -> Vec<ReleaseInput> {
    units
        .iter()
        .filter_map(|unit| {
            let entry = head.get(&unit.name).filter(|entry| !entry.seeded)?;
            let before = target.get(&unit.name);
            (before != Some(entry)).then(|| ReleaseInput {
                unit: unit.name.clone(),
                adapter: unit.adapter,
                current: before.map(|before| before.version.to_string()),
                next: entry.version.to_string(),
            })
        })
        .collect()
}

/// The pending changesets that name released units with a bump, each with
/// those units.
pub fn stale(pending: &[Changeset], released: &[ReleaseInput]) -> BTreeMap<String, Vec<String>> {
    let mut stale = BTreeMap::new();
    for changeset in pending {
        let units: Vec<String> = changeset
            .releases
            .iter()
            .filter(|(_, bump)| bump.parse::<Bump>() != Ok(Bump::None))
            .filter(|(unit, _)| released.iter().any(|release| &release.unit == *unit))
            .map(|(unit, _)| unit.clone())
            .collect();
        if !units.is_empty() {
            stale.insert(changeset.file.clone(), units);
        }
    }
    stale
}
