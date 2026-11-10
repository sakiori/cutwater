//! `check`: the merge request rule. A change to a unit's shipped files needs
//! a changeset that the merge request adds or edits and that names the unit
//! or a member of its fixed group, or an empty changeset the merge request
//! adds. Edits and deletions of other merge requests' pending changesets are
//! listed for the reviewers.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use anyhow::{Result, bail};

use super::Repository;
use crate::changes::{self, ChangedFile, Status};
use crate::changeset::{self, Changeset};
use crate::cli::CheckArgs;
use crate::git::Git;
use crate::units::Units;

/// The merge request's diff base.
const DIFF_BASE_VARIABLE: &str = "CI_MERGE_REQUEST_DIFF_BASE_SHA";
/// The target branch commit a merged results or merge train pipeline merged
/// the merge request onto; set only in those pipelines.
const TARGET_VARIABLE: &str = "CI_MERGE_REQUEST_TARGET_BRANCH_SHA";

pub fn run(args: &CheckArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let pending = changeset::pending_at(&repo.git, "HEAD")?;
    changeset::validate(&pending, &repo.units)?;

    if let Some(release) = ReleaseMergeRequest::detect(&repo.config.release_branch) {
        println!(
            "This is the release merge request, from the branch `{}` of this project: it needs \
             no changeset.",
            release.branch
        );
        return Ok(ExitCode::SUCCESS);
    }

    let base = Base::resolve(&repo.git, args.base.as_deref())?;
    let changed = changes::between(&repo.git, &base.commit, "HEAD")?;
    let before = changed
        .iter()
        .filter(|file| file.status != Status::Added && changeset::is_changeset_path(&file.path))
        .map(|file| {
            let changeset = match repo.git.file_at(&base.commit, &file.path) {
                Ok(Some(text)) => {
                    Changeset::parse(&file.path, &text).map_err(|err| err.to_string())
                }
                Ok(None) => Err(format!("{}: missing in the base", file.path)),
                Err(err) => Err(format!("{err:#}")),
            };
            (file.path.clone(), changeset)
        })
        .collect();
    let report = evaluate(&repo.units, &changed, &pending, &before);

    println!("Comparing HEAD with {} ({}).", base.commit, base.origin);
    print(&report);
    Ok(if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The verdict of `check` and what it rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The merge request's edits and deletions of changesets pending in the
    /// base.
    pub corrections: Vec<Correction>,
    /// The units whose shipped files changed, by name.
    pub units: Vec<ChangedUnit>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.units.iter().all(|unit| unit.covered_by.is_some())
    }
}

/// A changeset pending in the base that the merge request edits or deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub file: String,
    /// The changeset in the base, or why it cannot be read.
    pub before: Result<BTreeMap<String, String>, String>,
    /// The changeset in HEAD; `None` when the merge request deletes it.
    pub after: Option<BTreeMap<String, String>>,
}

/// A unit whose shipped files changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedUnit {
    pub name: String,
    pub path: String,
    pub files: Vec<String>,
    pub covered_by: Option<Coverage>,
}

/// The changeset that covers a changed unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub file: String,
    /// Whether it is an empty changeset, which covers every unit.
    pub empty: bool,
}

/// Judges the files changed since the base. `pending` holds the changesets
/// of HEAD and `before` the base's version of every changed changeset file
/// the base has.
pub fn evaluate(
    units: &Units,
    changed: &[ChangedFile],
    pending: &[Changeset],
    before: &BTreeMap<String, Result<Changeset, String>>,
) -> Report {
    let in_head = |path: &str| pending.iter().find(|changeset| changeset.file == path);
    let corrections = changed
        .iter()
        .filter_map(|file| {
            let before = before.get(&file.path)?;
            Some(Correction {
                file: file.path.clone(),
                before: before.clone().map(|changeset| changeset.releases),
                after: in_head(&file.path).map(|changeset| changeset.releases.clone()),
            })
        })
        .collect();

    let own: Vec<&Changeset> = changed
        .iter()
        .filter(|file| file.status != Status::Deleted)
        .filter_map(|file| in_head(&file.path))
        .collect();
    let added_empty = changed
        .iter()
        .filter(|file| file.status == Status::Added)
        .filter_map(|file| in_head(&file.path))
        .find(|changeset| changeset.is_empty());

    let units = units
        .iter()
        .filter_map(|unit| {
            let files: Vec<String> = changed
                .iter()
                .filter(|file| unit.ships(&file.path))
                .map(|file| file.path.clone())
                .collect();
            if files.is_empty() {
                return None;
            }
            let mut names = BTreeSet::from([unit.name.as_str()]);
            if let Some(group) = units.group_of(&unit.name) {
                names.extend(group.members.iter().map(String::as_str));
            }
            let covered_by = own
                .iter()
                .find(|changeset| names.iter().any(|name| changeset.names(name)))
                .map(|changeset| Coverage {
                    file: changeset.file.clone(),
                    empty: false,
                })
                .or_else(|| {
                    added_empty.map(|changeset| Coverage {
                        file: changeset.file.clone(),
                        empty: true,
                    })
                });
            Some(ChangedUnit {
                name: unit.name.clone(),
                path: unit.path.clone(),
                files,
                covered_by,
            })
        })
        .collect();
    Report { corrections, units }
}

fn print(report: &Report) {
    if !report.corrections.is_empty() {
        println!("This merge request edits or deletes pending changesets of other merge requests:");
        for correction in &report.corrections {
            println!("  {}", describe(correction));
        }
    }
    let covered: Vec<&ChangedUnit> = report
        .units
        .iter()
        .filter(|unit| unit.covered_by.is_some())
        .collect();
    if !covered.is_empty() {
        println!("Changed units with a changeset:");
        for unit in covered {
            if let Some(coverage) = &unit.covered_by {
                let empty = if coverage.empty { " (empty)" } else { "" };
                println!("  {}: {}{empty}", unit.name, coverage.file);
            }
        }
    }
    if report.units.is_empty() {
        println!("No unit's shipped files changed.");
    }
    if report.passed() {
        return;
    }
    eprintln!(
        "error: these units' shipped files changed, but no changeset of this merge request \
         names them:"
    );
    for unit in report.units.iter().filter(|unit| unit.covered_by.is_none()) {
        eprintln!("  {} ({}):", unit.name, unit.path);
        for file in &unit.files {
            eprintln!("    {file}");
        }
    }
    eprintln!(
        "Add a changeset with `cutwater add`, or with `cutwater add --empty` when the change \
         releases nothing."
    );
}

fn describe(correction: &Correction) -> String {
    let file = &correction.file;
    let before = match &correction.before {
        Ok(before) => before,
        Err(problem) => {
            let what = if correction.after.is_some() {
                "edited"
            } else {
                "deleted"
            };
            return format!("{file}: {what}; its pending version cannot be read: {problem}");
        }
    };
    let Some(after) = &correction.after else {
        return format!("{file}: deleted, {}", bumps(before));
    };
    let units: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let changes: Vec<String> = units
        .into_iter()
        .filter(|unit| before.get(*unit) != after.get(*unit))
        .map(|unit| {
            let old = before.get(unit).map_or("(not named)", String::as_str);
            let new = after.get(unit).map_or("(not named)", String::as_str);
            format!("{unit} {old} -> {new}")
        })
        .collect();
    if changes.is_empty() {
        format!("{file}: edited, same bumps ({})", bumps(before))
    } else {
        format!("{file}: edited, {}", changes.join(", "))
    }
}

fn bumps(releases: &BTreeMap<String, String>) -> String {
    if releases.is_empty() {
        return "empty".to_string();
    }
    releases
        .iter()
        .map(|(unit, bump)| format!("{unit} {bump}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The commit HEAD is compared with.
struct Base {
    commit: String,
    origin: String,
}

impl Base {
    /// `--base`'s merge base with HEAD; in a merged results or merge train
    /// pipeline, the target branch commit the pipeline merged onto; else the
    /// merge base of the merge request's diff base with HEAD.
    fn resolve(git: &Git, flag: Option<&str>) -> Result<Base> {
        let variable = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        if let Some(rev) = flag {
            return merge_base(git, rev, "--base");
        }
        if let Some(target) = variable(TARGET_VARIABLE) {
            // HEAD is a commit that GitLab made on top of this one; a
            // fast-forward merge train may have rebased several commits onto
            // it, so it need not be HEAD's first parent.
            let Some(commit) = git.commit(&target)? else {
                bail!(missing(git, &target, TARGET_VARIABLE)?);
            };
            return Ok(Base {
                commit,
                origin: format!(
                    "the target branch commit of this merged result, {TARGET_VARIABLE}"
                ),
            });
        }
        if let Some(sha) = variable(DIFF_BASE_VARIABLE) {
            return merge_base(git, &sha, DIFF_BASE_VARIABLE);
        }
        bail!(
            "no base to compare with: run `check` in a merge request pipeline, which sets \
             {DIFF_BASE_VARIABLE}, or pass `--base <commit>`"
        )
    }
}

fn merge_base(git: &Git, rev: &str, source: &str) -> Result<Base> {
    let Some(commit) = git.commit(rev)? else {
        // Only a full commit name can be missing from a shallow clone; any
        // other revision is one the repository does not have.
        let full_name = matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit());
        if !full_name {
            bail!(
                "`{rev}` ({source}) names no commit of this repository; pass a commit, branch or \
                 tag"
            );
        }
        bail!(missing(git, rev, source)?);
    };
    let Some(base) = git.merge_base(&commit, "HEAD")? else {
        bail!(
            "the history in this clone does not connect `{rev}` ({source}) with HEAD.{} Raise \
             the clone depth, GIT_DEPTH in GitLab CI, so that the clone reaches their merge base.",
            shallow_note(git)?
        );
    };
    Ok(Base {
        commit: base,
        origin: format!("the merge base of {source} with HEAD"),
    })
}

fn missing(git: &Git, rev: &str, source: &str) -> Result<String> {
    Ok(format!(
        "this clone lacks the commit `{rev}` ({source}).{} Raise the clone depth, GIT_DEPTH in \
         GitLab CI, so that the clone reaches it, or fetch that commit.",
        shallow_note(git)?
    ))
}

fn shallow_note(git: &Git) -> Result<&'static str> {
    Ok(if git.is_shallow()? {
        " This is a shallow clone."
    } else {
        ""
    })
}

/// The merge request whose pipeline this is, when it is the release merge
/// request: from the release branch, of the project the pipeline runs in,
/// into the same project. A fork's branch of the same name is not.
struct ReleaseMergeRequest {
    branch: String,
}

impl ReleaseMergeRequest {
    fn detect(release_branch: &str) -> Option<ReleaseMergeRequest> {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let branch = var("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME")?;
        let source_project = var("CI_MERGE_REQUEST_SOURCE_PROJECT_ID")?;
        let target_project = var("CI_MERGE_REQUEST_PROJECT_ID")?;
        let project = var("CI_PROJECT_ID")?;
        (branch == release_branch && source_project == target_project && target_project == project)
            .then_some(ReleaseMergeRequest { branch })
    }
}
