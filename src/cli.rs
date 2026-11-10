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
    /// Set the repository up: create .cutwater/config.toml for the npm
    /// packages and Cargo crates it finds, importing the settings of
    /// Changesets, and seed the version manifest with the versions their
    /// registries list.
    Init(InitArgs),
    /// Create a changeset: the units a change releases, their bumps and the
    /// entry for the release notes. Without arguments, asks in the terminal.
    Add(AddArgs),
    /// Check that a merge request carries a changeset for every unit whose
    /// shipped files it changes, list its edits of other merge requests'
    /// pending changesets, and check that the units' files hold the
    /// versions of the manifest.
    Check(CheckArgs),
    /// List the pending changesets and the releases they make: each unit's
    /// current and next version and why it is released.
    Status(StatusArgs),
    /// Apply the pending changesets: write the new versions, the
    /// references of dependents, the changelogs and the version manifest,
    /// delete the changesets and refresh the lockfiles.
    Version(VersionArgs),
    /// Enter or leave prerelease mode.
    #[command(subcommand)]
    Pre(PreCommand),
    /// Bring the release merge request up to date, in a pipeline of the
    /// default branch: apply the pending changesets to the default branch's
    /// tip, commit the result onto the release branch through GitLab's API
    /// and create or update the merge request, or close it when nothing is
    /// pending. Never merges it.
    Mr(MrArgs),
    /// Check the release merge request, in its pipeline: no pending
    /// changeset names a unit it releases, the units' files hold the
    /// manifest's versions, and what would be published passes the
    /// adapters' checks; then run the repository checks.
    Goal(GoalArgs),
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

#[derive(Debug, Args)]
pub struct MrArgs {
    /// Say what would be written and how the merge request would change,
    /// and change nothing on GitLab.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct GoalArgs {
    /// The commit to compare the version manifest with, normally the merge
    /// request's target; the comparison starts from its merge base with
    /// HEAD. Default: as for `check`, from GitLab's CI variables.
    #[arg(long, value_name = "COMMIT")]
    pub base: Option<String>,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// A unit that was never published: it gets no manifest entry, and its
    /// first release is the version its files declare. Repeat it for
    /// several units.
    #[arg(long = "new", value_name = "UNIT")]
    pub new: Vec<String>,
    /// A unit whose declared version is published although its registry
    /// does not list it, as a yanked version: it is seeded with that version
    /// without asking. Repeat it for several units.
    #[arg(long = "published", value_name = "UNIT")]
    pub published: Vec<String>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print the status as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct VersionArgs {
    /// Print the plan and the files it would change, and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
pub enum PreCommand {
    /// Record prerelease mode in .cutwater/pre.toml: from the next release
    /// on, releases are prereleases <version>-<label>.<n>.
    Enter {
        /// The prerelease label, such as beta or rc; also the npm
        /// distribution tag of the prereleases.
        label: String,
    },
    /// Make the next release leave prerelease mode with final versions.
    Exit,
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
/// rule is violated, 2 when the tool cannot reach a verdict or deliver it.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init(args) => commands::init::run(&args),
        Command::Add(args) => commands::add::run(&args),
        Command::Check(args) => commands::check::run(&args),
        Command::Status(args) => commands::status::run(&args),
        Command::Version(args) => commands::version::run(&args),
        Command::Pre(command) => commands::pre::run(&command),
        Command::Mr(args) => commands::mr::run(&args),
        Command::Goal(args) => commands::goal::run(&args),
    };
    match result {
        Ok(code) => code,
        // The reader of standard output is gone, as `head` leaves; the
        // command stopped at its next output, so `version` stops before it
        // writes the tree or after it wrote all of it.
        Err(err) if crate::output::is_broken_pipe(&err) => ExitCode::from(2),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}
