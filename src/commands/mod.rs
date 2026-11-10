//! The subcommands.

pub mod add;
mod base;
pub mod check;
pub mod goal;
pub mod init;
pub mod mr;
pub mod pre;
pub mod publish;
pub mod status;
pub mod version;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use semver::Version;

use crate::adapter::{AcceptsRequest, AdapterKind, Edge, InspectRequest, UnitRef, UnitVersion};
use crate::changeset::{self, Changeset};
use crate::checks::{self, PlanInput, UnitInput};
use crate::config::{CheckStep, Config};
use crate::git::Git;
use crate::manifest::Manifest;
use crate::plan::{self, Plan};
use crate::prerelease::PreState;
use crate::units::Units;

/// The variables GitLab CI sets for the branch of a branch pipeline.
const COMMIT_BRANCH_VARIABLE: &str = "CI_COMMIT_BRANCH";
const GITLAB_CI_VARIABLE: &str = "GITLAB_CI";

/// Refuses to run `command` in a GitLab CI pipeline of anything but the
/// default branch.
pub fn check_pipeline(command: &str, default_branch: &str) -> Result<()> {
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if variable(GITLAB_CI_VARIABLE).is_none() {
        return Ok(());
    }
    match variable(COMMIT_BRANCH_VARIABLE) {
        Some(branch) if branch == default_branch => Ok(()),
        Some(branch) => anyhow::bail!(
            "`{command}` runs in pipelines of the default branch `{default_branch}`, but this \
             pipeline is of `{branch}`"
        ),
        None => anyhow::bail!(
            "`{command}` runs in pipelines of the default branch `{default_branch}`, but this \
             pipeline is not a branch pipeline"
        ),
    }
}

/// The repository the tool runs in, with its configuration and units.
pub struct Repository {
    pub git: Git,
    pub config: Config,
    pub units: Units,
}

/// What the adapters read from the units' files.
pub struct Inspected {
    /// The version each unit's files declare, by unit.
    pub declared: BTreeMap<String, UnitVersion>,
    /// The references to units.
    pub edges: Vec<Edge>,
}

/// The plan of the working tree and what it rests on.
pub struct Planned {
    pub plan: Plan,
    pub changesets: Vec<Changeset>,
    pub pre: Option<PreState>,
    pub manifest: Manifest,
}

impl Repository {
    /// The repository around the current directory.
    pub fn open() -> Result<Repository> {
        let cwd = std::env::current_dir().context("cannot read the current directory")?;
        Self::open_with(Git::discover(&cwd)?)
    }

    /// The repository of the working tree `git` is bound to.
    pub fn open_with(git: Git) -> Result<Repository> {
        let config = Config::load(git.root())?;
        let units = Units::resolve(git.root(), &config)?;
        Ok(Repository { git, config, units })
    }

    /// The units of one adapter, as its requests name them.
    pub fn unit_refs(&self, kind: AdapterKind) -> Vec<UnitRef> {
        self.units
            .iter()
            .filter(|unit| unit.adapter == kind)
            .map(|unit| UnitRef {
                name: unit.name.clone(),
                path: unit.path.clone(),
            })
            .collect()
    }

    /// The units' versions and the references to them, from every adapter
    /// that has units.
    pub fn inspect(&self) -> Result<Inspected> {
        let mut declared = BTreeMap::new();
        let mut edges = Vec::new();
        for kind in AdapterKind::ALL {
            let units = self.unit_refs(kind);
            if units.is_empty() {
                continue;
            }
            let inspection = kind.adapter().inspect(&InspectRequest {
                root: self.git.root().to_path_buf(),
                units,
            })?;
            for unit in inspection.units {
                declared.insert(unit.name.clone(), unit);
            }
            edges.extend(inspection.edges);
        }
        Ok(Inspected { declared, edges })
    }

    /// The plan of the pending changesets of the working tree.
    pub fn plan(&self) -> Result<Planned> {
        let changesets = changeset::pending_in(self.git.root())?;
        changeset::validate(&changesets, &self.units)?;
        let inspected = self.inspect()?;
        self.plan_from(changesets, &inspected)
    }

    /// The plan of `changesets`, valid for the units, from the manifest and
    /// prerelease state of the working tree and what the adapters read.
    pub fn plan_from(&self, changesets: Vec<Changeset>, inspected: &Inspected) -> Result<Planned> {
        let root = self.git.root();
        let manifest = Manifest::load(root)?;
        let pre = PreState::load(root)?;
        let accepts =
            |kind: AdapterKind, range: &str, file: &str, version: &Version| -> Result<bool> {
                Ok(kind.adapter().accepts(&AcceptsRequest {
                    range: range.to_string(),
                    version: version.to_string(),
                    file: file.to_string(),
                })?)
            };
        let plan = plan::plan(
            &plan::Input {
                units: &self.units,
                manifest: &manifest,
                pre: pre.as_ref(),
                changesets: &changesets,
                declared: &inspected.declared,
                edges: &inspected.edges,
            },
            &accepts,
        )?;
        Ok(Planned {
            plan,
            changesets,
            pre,
            manifest,
        })
    }

    /// Runs the repository checks the configuration names for `step` with
    /// the units and `plan` as their input; `true` when all passed, or none
    /// is configured.
    pub fn repository_checks(
        &self,
        step: CheckStep,
        manifest: &Manifest,
        inspected: &Inspected,
        plan: PlanInput,
    ) -> Result<bool> {
        let checks = checks::for_step(&self.config.checks, step);
        if checks.is_empty() {
            return Ok(true);
        }
        let units = self
            .units
            .iter()
            .map(|unit| UnitInput {
                name: unit.name.clone(),
                path: unit.path.clone(),
                adapter: unit.adapter,
                group: self
                    .units
                    .group_of(&unit.name)
                    .map(|group| group.name.clone()),
                released: manifest
                    .get(&unit.name)
                    .map(|entry| entry.version.to_string()),
                declared: inspected
                    .declared
                    .get(&unit.name)
                    .map(|declared| declared.version.clone()),
            })
            .collect();
        checks::run(
            self.git.root(),
            &checks,
            &checks::Input { step, units, plan },
        )
    }
}
