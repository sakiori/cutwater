//! `released`: the units whose release commit is HEAD, which the pipeline
//! of HEAD publishes, so that CI builds and signs a release only in that
//! pipeline.

use std::process::ExitCode;

use anyhow::Result;
use serde::Serialize;

use super::Repository;
use super::publish::{ReleasedHere, released_here};
use crate::cli::ReleasedArgs;

#[derive(Serialize)]
struct Record {
    commit: String,
    units: Vec<ReleasedHere>,
}

pub fn run(args: &ReleasedArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let (commit, units) = released_here(&repo)?;
    if args.json {
        let text = serde_json::to_string_pretty(&Record { commit, units })?;
        crate::outln!("{text}")?;
    } else {
        for unit in &units {
            crate::outln!("{} {} {}", unit.name, unit.version, unit.adapter)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
