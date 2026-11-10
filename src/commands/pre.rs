//! `pre enter` and `pre exit`: record and leave prerelease mode in
//! `.cutwater/pre.toml`, to be merged through a merge request like any
//! change.

use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use super::Repository;
use crate::cli::PreCommand;
use crate::prerelease::{self, Mode, PRE_FILE, PreState};

pub fn run(command: &PreCommand) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let root = repo.git.root();
    let state = PreState::load(root)?;
    let path = root.join(PRE_FILE);
    match command {
        PreCommand::Enter { label } => {
            prerelease::check_label(label).map_err(|problem| anyhow::anyhow!(problem))?;
            let state = match state {
                Some(state) if state.mode == Mode::Pre => bail!(
                    "{PRE_FILE} already records prerelease mode `{}`; to change the label, run \
                     `cutwater pre exit` and then `cutwater pre enter {label}`",
                    state.label
                ),
                // Leaving prerelease mode was not released yet: stay, under
                // the new label, and keep the prereleases' entries.
                Some(state) => PreState {
                    mode: Mode::Pre,
                    label: label.clone(),
                    units: state.units,
                },
                None => PreState {
                    mode: Mode::Pre,
                    label: label.clone(),
                    units: Default::default(),
                },
            };
            write(&path, &state)?;
            crate::outln!(
                "Wrote {PRE_FILE}: releases are prereleases <version>-{label}.<n> from the next \
                 `cutwater version` on. Merge it through a merge request."
            )?;
        }
        PreCommand::Exit => {
            let Some(state) = state else {
                bail!("not in prerelease mode: {PRE_FILE} does not exist");
            };
            if state.mode == Mode::Exit {
                crate::outln!(
                    "{PRE_FILE} already leaves prerelease mode `{}` with the next release.",
                    state.label
                )?;
            } else if state.units.is_empty() {
                std::fs::remove_file(&path).with_context(|| format!("cannot delete {PRE_FILE}"))?;
                crate::outln!(
                    "Deleted {PRE_FILE}: no prerelease was made, so nothing needs a final \
                     release. Merge it through a merge request."
                )?;
            } else {
                write(
                    &path,
                    &PreState {
                        mode: Mode::Exit,
                        ..state
                    },
                )?;
                crate::outln!(
                    "Wrote {PRE_FILE}: the next `cutwater version` makes the final releases. \
                     Merge it through a merge request."
                )?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn write(path: &std::path::Path, state: &PreState) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {PRE_FILE}"))?;
    }
    std::fs::write(path, state.render()).with_context(|| format!("cannot write {PRE_FILE}"))
}
