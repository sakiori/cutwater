//! `status`: the pending changesets and the releases they make.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::Result;
use serde::Serialize;

use super::{Planned, Repository};
use crate::changeset::Changeset;
use crate::cli::StatusArgs;
use crate::plan::{Plan, Reason, Release};
use crate::prerelease::{Mode, PreState};

pub fn run(args: &StatusArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let planned = repo.plan()?;
    if args.json {
        let status = Status::new(&planned);
        crate::outln!(
            "{}",
            serde_json::to_string_pretty(&status).expect("the status serializes")
        )?;
    } else {
        print_prerelease(planned.pre.as_ref())?;
        print_changesets(&planned.changesets)?;
        print_plan(&planned.plan)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The JSON form of `status`.
#[derive(Serialize)]
struct Status<'a> {
    prerelease: Option<Prerelease<'a>>,
    changesets: Vec<PendingChangeset<'a>>,
    /// The releases in the order of publication.
    releases: &'a [Release],
    /// The units whose manifest entries `version` removes.
    removed: &'a [String],
}

#[derive(Serialize)]
struct Prerelease<'a> {
    label: &'a str,
    /// Whether the next release leaves prerelease mode.
    exiting: bool,
}

#[derive(Serialize)]
struct PendingChangeset<'a> {
    file: &'a str,
    releases: &'a BTreeMap<String, String>,
}

impl<'a> Status<'a> {
    fn new(planned: &'a Planned) -> Status<'a> {
        Status {
            prerelease: planned.pre.as_ref().map(|pre| Prerelease {
                label: &pre.label,
                exiting: pre.mode == Mode::Exit,
            }),
            changesets: planned
                .changesets
                .iter()
                .map(|changeset| PendingChangeset {
                    file: &changeset.file,
                    releases: &changeset.releases,
                })
                .collect(),
            releases: &planned.plan.releases,
            removed: &planned.plan.removed,
        }
    }
}

pub fn print_prerelease(pre: Option<&PreState>) -> std::io::Result<()> {
    match pre {
        Some(pre) if pre.mode == Mode::Exit => crate::outln!(
            "Leaving prerelease mode `{}`: the next release is final.",
            pre.label
        ),
        Some(pre) => crate::outln!(
            "Prerelease mode `{}`: releases are prereleases <version>-{}.<n>.",
            pre.label,
            pre.label
        ),
        None => Ok(()),
    }
}

fn print_changesets(changesets: &[Changeset]) -> std::io::Result<()> {
    if changesets.is_empty() {
        return crate::outln!("No pending changesets.");
    }
    crate::outln!("Pending changesets:")?;
    for changeset in changesets {
        let releases = if changeset.is_empty() {
            "empty".to_string()
        } else {
            changeset
                .releases
                .iter()
                .map(|(unit, bump)| format!("{unit} {bump}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        crate::outln!("  {}: {releases}", changeset.file)?;
    }
    Ok(())
}

/// Prints the releases of a plan and the manifest entries it removes.
pub fn print_plan(plan: &Plan) -> std::io::Result<()> {
    if plan.releases.is_empty() {
        crate::outln!("Nothing to release.")?;
    } else {
        crate::outln!("Releases, in the order of publication:")?;
        for release in &plan.releases {
            let current = match &release.current {
                Some(current) if release.seeded => format!("{current} (seeded)"),
                Some(current) => current.to_string(),
                None => "new".to_string(),
            };
            // A new unit's first release is the version its files declare,
            // unless it joins a fixed group, whose version it takes.
            let declared = release.current.is_none()
                && !release
                    .reasons
                    .iter()
                    .any(|reason| matches!(reason, Reason::Group { .. }));
            let reasons: Vec<String> = release
                .reasons
                .iter()
                .map(|reason| describe(reason, declared))
                .collect();
            crate::outln!(
                "  {} {current} -> {}: {}",
                release.unit,
                release.next,
                reasons.join("; ")
            )?;
        }
    }
    if !plan.removed.is_empty() {
        let when = if plan.releases.is_empty() {
            "the next release removes"
        } else {
            "this release removes"
        };
        crate::outln!(
            "Units no longer in the repository, whose manifest entries {when}: {}",
            plan.removed.join(", ")
        )?;
    }
    Ok(())
}

fn describe(reason: &Reason, declared: bool) -> String {
    match reason {
        Reason::Changesets { changesets, .. } if declared => format!(
            "first release, the version its files declare ({})",
            changesets.join(", ")
        ),
        Reason::Changesets { bump, changesets } => {
            format!("{bump} ({})", changesets.join(", "))
        }
        Reason::Group { group } => format!("fixed group {group}"),
        Reason::Dependencies { dependencies } => {
            let causes: Vec<String> = dependencies
                .iter()
                .map(|cause| {
                    format!(
                        "{} {} is outside its {} range {}",
                        cause.unit, cause.version, cause.kind, cause.range
                    )
                })
                .collect();
            causes.join(", ")
        }
        Reason::Prerelease { label } => format!("final release after the `{label}` prereleases"),
    }
}
