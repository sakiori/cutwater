//! The command line.

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::changeset::Bump;
use crate::commands;

/// Releases the units of a repository through one merge request that a bot
/// keeps up to date, from changeset files reviewed with each change.
#[derive(Debug, Parser)]
#[command(name = "cutwater", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a changeset: the units a change releases, their bumps and the
    /// entry for the release notes. Without arguments, asks in the terminal.
    Add(AddArgs),
    /// Check that a merge request carries a changeset for every unit whose
    /// shipped files it changes, and list its edits of other merge requests'
    /// pending changesets.
    Check(CheckArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// A unit the change releases and its bump: major, minor, patch or none.
    /// Repeat it for several units.
    #[arg(
        long = "unit",
        value_name = "NAME:BUMP",
        value_parser = unit_bump,
        requires = "message",
        conflicts_with = "empty"
    )]
    pub units: Vec<(String, Bump)>,
    /// The entry for the release notes.
    #[arg(long, short, requires = "units", conflicts_with = "empty")]
    pub message: Option<String>,
    /// Create a changeset that releases nothing, for a change that needs no
    /// release.
    #[arg(long)]
    pub empty: bool,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// The commit to compare with, normally the merge request's diff base;
    /// the comparison starts from its merge base with HEAD. Default: the
    /// commit CI_MERGE_REQUEST_DIFF_BASE_SHA names.
    #[arg(long, value_name = "COMMIT")]
    pub base: Option<String>,
}

fn unit_bump(value: &str) -> Result<(String, Bump), String> {
    let (name, bump) = value
        .rsplit_once(':')
        .ok_or_else(|| format!("`{value}` is not NAME:BUMP"))?;
    if name.is_empty() {
        return Err(format!("`{value}` names no unit"));
    }
    Ok((name.to_string(), bump.parse()?))
}

/// Runs the command line and returns the process's exit status: 1 when a
/// rule is violated, 2 when the tool cannot reach a verdict.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Add(args) => commands::add::run(&args),
        Command::Check(args) => commands::check::run(&args),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}
