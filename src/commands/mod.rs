//! The subcommands.

pub mod add;
pub mod check;
pub mod init;
pub mod pre;
pub mod status;
pub mod version;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use semver::Version;

use crate::adapter::{AcceptsRequest, AdapterKind, Edge, InspectRequest, UnitRef, UnitVersion};
use crate::changeset::{self, Changeset};
use crate::config::Config;
use crate::git::Git;
use crate::manifest::Manifest;
use crate::plan::{self, Plan};
use crate::prerelease::PreState;
use crate::units::Units;

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
        let git = Git::discover(&cwd)?;
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
        let root = self.git.root();
        let changesets = changeset::pending_in(root)?;
        changeset::validate(&changesets, &self.units)?;
        let manifest = Manifest::load(root)?;
        let pre = PreState::load(root)?;
        let inspected = self.inspect()?;
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
}
