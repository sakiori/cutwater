//! The subcommands.

pub mod add;
pub mod check;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::git::Git;
use crate::units::Units;

/// The repository the tool runs in, with its configuration and units.
pub struct Repository {
    pub git: Git,
    pub config: Config,
    pub units: Units,
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
}
