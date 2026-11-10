//! The internal interface of the ecosystem adapters. Every operation takes
//! and returns serializable values, so that a protocol for external adapters
//! can carry them unchanged.

mod cargo;
mod npm;
mod walk;

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The ecosystem adapter a unit belongs to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    /// npm packages: pnpm and npm workspaces, or a single `package.json`.
    Npm,
    /// Cargo crates: the members of a workspace, or a single crate.
    Cargo,
}

impl AdapterKind {
    pub const ALL: [AdapterKind; 2] = [AdapterKind::Npm, AdapterKind::Cargo];

    pub fn name(self) -> &'static str {
        match self {
            AdapterKind::Npm => "npm",
            AdapterKind::Cargo => "cargo",
        }
    }

    pub fn adapter(self) -> &'static dyn Adapter {
        match self {
            AdapterKind::Npm => &npm::Npm,
            AdapterKind::Cargo => &cargo::Cargo,
        }
    }
}

impl std::fmt::Display for AdapterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The request to find the units of a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverRequest {
    /// The repository's root directory.
    pub root: PathBuf,
}

/// The units an adapter found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub units: Vec<DiscoveredUnit>,
}

/// A publishable package an adapter found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredUnit {
    /// The package's name in its ecosystem.
    pub name: String,
    /// The package's directory relative to the repository root, with `/`
    /// separators; `.` for the root itself.
    pub path: String,
}

/// An adapter's failure, naming the file it concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{file}: {message}")]
pub struct AdapterError {
    /// The file the failure concerns, relative to the repository root.
    pub file: String,
    pub message: String,
}

impl AdapterError {
    pub fn new(file: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            message: message.into(),
        }
    }
}

/// An ecosystem adapter.
pub trait Adapter: Sync {
    /// Finds the publishable packages of the repository: the units the
    /// adapter discovers when the configuration enables it.
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError>;
}
