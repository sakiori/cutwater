//! The internal interface of the ecosystem adapters. Every operation takes
//! and returns serializable values, so that a protocol for external adapters
//! can carry them unchanged.

mod cargo;
mod npm;
mod pnpm;
mod walk;

use std::path::{Path, PathBuf};

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

/// A unit of the adapter, as the core knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRef {
    /// The unit's name.
    pub name: String,
    /// The unit's directory relative to the repository root, with `/`
    /// separators; `.` for the root itself.
    pub path: String,
}

/// The request to read the units' versions and the dependencies on them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectRequest {
    pub root: PathBuf,
    /// The adapter's units.
    pub units: Vec<UnitRef>,
}

/// The units' versions and the dependencies on them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inspection {
    /// Each unit's version, in the order of the request.
    pub units: Vec<UnitVersion>,
    /// The references to the units from the packages of the repository,
    /// units or not.
    pub edges: Vec<Edge>,
}

/// The version a unit's files declare: for npm and Cargo, the version it
/// was released at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitVersion {
    pub name: String,
    pub version: String,
    /// Whether the files hold the released version, which the manifest
    /// must then agree with, rather than one that leads to the next
    /// release, such as a snapshot.
    pub holds_release: bool,
    /// Where the version is written. Units whose versions are written in
    /// the same place share their version.
    pub location: Location,
}

/// A place in a file: the file relative to the repository root and the
/// path of keys to a value in it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    pub keys: Vec<String>,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} `{}`", self.file, self.keys.join("."))
    }
}

/// How a dependent uses a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// Needed at run time: npm `dependencies`, Cargo `dependencies`.
    Runtime,
    /// Needed to build: Cargo `build-dependencies`.
    Build,
    /// Provided by the dependent's user: npm `peerDependencies`.
    Peer,
    /// Needed at run time where available: npm `optionalDependencies`,
    /// optional Cargo `dependencies`.
    Optional,
    /// Needed only to develop the dependent: npm `devDependencies`, Cargo
    /// `dev-dependencies`.
    Development,
}

impl EdgeKind {
    /// Whether a release of the dependency outside the dependent's range
    /// releases the dependent.
    pub fn releases_dependent(self) -> bool {
        self != EdgeKind::Development
    }

    /// Whether the edge orders publication: the dependency is published
    /// before the dependent.
    pub fn orders_publication(self) -> bool {
        matches!(
            self,
            EdgeKind::Runtime | EdgeKind::Build | EdgeKind::Optional
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            EdgeKind::Runtime => "runtime",
            EdgeKind::Build => "build",
            EdgeKind::Peer => "peer",
            EdgeKind::Optional => "optional",
            EdgeKind::Development => "development",
        }
    }
}

impl std::fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A package's reference to a unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// The directory of the dependent package.
    pub dependent_path: String,
    /// The dependent's unit; `None` for a package that is not a unit, whose
    /// references are kept current all the same.
    pub dependent_unit: Option<String>,
    /// The unit depended on.
    pub dependency: String,
    pub kind: EdgeKind,
    /// The reference as written, such as `workspace:^`, `catalog:` or
    /// `^1.2.0`.
    pub reference: String,
    /// The range of the dependency's versions that the dependent's last
    /// release accepts, in the ecosystem's syntax; `None` when the reference
    /// does not constrain the version.
    pub range: Option<String>,
    /// Whether the reference follows the dependency's version by itself,
    /// such as `workspace:^`, so that it never needs rewriting.
    pub follows: bool,
    /// Where the reference's range is written.
    pub location: Location,
}

/// The request to judge whether a range accepts a version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptsRequest {
    pub range: String,
    pub version: String,
    /// The file the range is written in, relative to the repository root.
    pub file: String,
}

/// The request to write new versions and references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRequest {
    pub root: PathBuf,
    pub units: Vec<UnitRef>,
    /// The units' new versions.
    pub versions: Vec<VersionWrite>,
    /// The references to rewrite because their range does not accept the
    /// dependency's new version.
    pub references: Vec<ReferenceWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionWrite {
    pub unit: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceWrite {
    pub edge: Edge,
    /// The dependency's new version, which the rewritten range accepts.
    pub version: String,
}

/// The new content of files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Writes {
    pub files: Vec<FileWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWrite {
    /// The file relative to the repository root.
    pub path: String,
    pub content: String,
}

/// The request about the lockfiles an adapter owns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockfileRequest {
    pub root: PathBuf,
    /// The adapter's units.
    pub units: Vec<UnitRef>,
}

/// The lockfiles of the repository that an adapter refreshes when versions
/// change, with the commands that refresh them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfiles {
    pub lockfiles: Vec<Lockfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    /// The lockfile relative to the repository root.
    pub path: String,
    /// The directory, relative to the repository root, to run the command
    /// in.
    pub dir: String,
    /// The command that refreshes it; its first word is the tool.
    pub command: Vec<String>,
}

/// An ecosystem adapter.
pub trait Adapter: Sync {
    /// Finds the publishable packages of the repository: the units the
    /// adapter discovers when the configuration enables it.
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError>;

    /// Reads the units' versions and the references to them, with their
    /// kinds and ranges.
    fn inspect(&self, request: &InspectRequest) -> Result<Inspection, AdapterError>;

    /// Judges by the ecosystem's rules whether a range accepts a version.
    fn accepts(&self, request: &AcceptsRequest) -> Result<bool, AdapterError>;

    /// The new content of the files that hold the units' versions and the
    /// references to rewrite, keeping their formatting.
    fn write(&self, request: &WriteRequest) -> Result<Writes, AdapterError>;

    /// The lockfiles to refresh after the adapter's files changed.
    fn lockfiles(&self, request: &LockfileRequest) -> Result<Lockfiles, AdapterError>;

    /// Asks each unit's registry, through the ecosystem's tool and the
    /// environment's settings, whether it holds a version of the unit.
    fn published(&self, request: &PublishedRequest) -> Result<Publications, AdapterError>;
}

/// The request to ask the units' registries about versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedRequest {
    pub root: PathBuf,
    pub queries: Vec<VersionQuery>,
}

/// A version of a unit to look for in the unit's registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionQuery {
    pub unit: UnitRef,
    pub version: String,
}

/// The registries' answers, in the order of the queries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publications {
    pub units: Vec<Publication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub unit: String,
    /// The registry asked.
    pub registry: String,
    /// Whether the ecosystem's tool lists the version asked for.
    pub published: bool,
    /// The highest version the tool lists, `None` when it lists none.
    pub highest: Option<String>,
    /// Why the tool could not answer, in its words; `None` when it did.
    pub problem: Option<String>,
}

/// The answer about `version` of `unit` from a registry that lists
/// `versions`.
fn publication(unit: &str, version: &str, registry: String, versions: &[String]) -> Publication {
    let parsed = |version: &String| semver::Version::parse(version).ok();
    Publication {
        unit: unit.to_string(),
        registry,
        published: versions.iter().any(|listed| listed == version),
        highest: versions
            .iter()
            .filter_map(parsed)
            .max()
            .map(|v| v.to_string()),
        problem: None,
    }
}

/// The variables that turn colour off in the tools whose output an adapter
/// captures, so that their messages reach ours without escape codes even
/// where the environment forces colour, as CI often does.
const PLAIN_OUTPUT: [(&str, &str); 4] = [
    ("CARGO_TERM_COLOR", "never"),
    ("NO_COLOR", "1"),
    ("FORCE_COLOR", "0"),
    ("npm_config_color", "false"),
];

/// The captured result of an ecosystem tool, or the adapter error of a
/// tool that cannot run, naming `file`.
fn run_tool(
    command: &mut std::process::Command,
    tool: &str,
    file: &str,
) -> Result<std::process::Output, AdapterError> {
    command
        .envs(PLAIN_OUTPUT)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| {
            let problem = if err.kind() == std::io::ErrorKind::NotFound {
                format!("asking the registry needs {tool}, which is not on the PATH")
            } else {
                format!("cannot run {tool}: {err}")
            };
            AdapterError::new(file, problem)
        })
}

/// Runs a lockfile's command in its directory below `root`, failing with
/// the tool's output.
pub fn refresh(root: &Path, lockfile: &Lockfile) -> Result<(), AdapterError> {
    let (tool, args) = lockfile
        .command
        .split_first()
        .expect("a lockfile command names its tool");
    let output = std::process::Command::new(tool)
        .args(args)
        .envs(PLAIN_OUTPUT)
        .current_dir(root.join(&lockfile.dir))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| {
            let problem = if err.kind() == std::io::ErrorKind::NotFound {
                format!("refreshing it needs {tool}, which is not on the PATH")
            } else {
                format!("cannot run {tool}: {err}")
            };
            AdapterError::new(&lockfile.path, problem)
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(AdapterError::new(
            &lockfile.path,
            format!("`{}` failed: {detail}", lockfile.command.join(" ")),
        ));
    }
    Ok(())
}

/// The lockfiles of the units' workspaces, each in the nearest directory at
/// or above a unit that holds a file of one of the `kinds`: a lockfile's
/// name and the command that refreshes it.
fn nearest_lockfiles(request: &LockfileRequest, kinds: &[(&str, &[&str])]) -> Lockfiles {
    let mut lockfiles = std::collections::BTreeMap::new();
    for unit in &request.units {
        let found = ancestors(&unit.path).into_iter().find_map(|dir| {
            let present: Vec<Lockfile> = kinds
                .iter()
                .filter(|(name, _)| request.root.join(join(&dir, name)).is_file())
                .map(|(name, command)| Lockfile {
                    path: join(&dir, name),
                    dir: dir.clone(),
                    command: command.iter().map(ToString::to_string).collect(),
                })
                .collect();
            (!present.is_empty()).then_some(present)
        });
        for lockfile in found.into_iter().flatten() {
            lockfiles.insert(lockfile.path.clone(), lockfile);
        }
    }
    Lockfiles {
        lockfiles: lockfiles.into_values().collect(),
    }
}

/// `dir` and the directories above it, up to the root `.`.
fn ancestors(dir: &str) -> Vec<String> {
    let mut dirs = vec![dir.to_string()];
    let mut current = dir;
    while current != "." {
        current = current.rsplit_once('/').map_or(".", |(parent, _)| parent);
        dirs.push(current.to_string());
    }
    dirs
}

/// `dir/file`, or `file` for the root directory `.`.
fn join(dir: &str, file: &str) -> String {
    if dir == "." {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// `path`, relative to the directory `base`, resolved to a directory
/// relative to the repository root; `None` when it leaves the repository.
fn resolve_dir(base: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = if base == "." {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}
