//! The Maven adapter. A unit is an aggregator POM whose version is
//! `${revision}`, released with the modules that share that version; the
//! committed `revision` is always the snapshot of its next release, and a
//! dependent refers to a unit through a version property kept equal to it.
//! The adapter reads POMs itself, with roxmltree, and runs Maven only to
//! build, to ask the deploy repository and to deploy.

mod mvn;
mod pom;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use semver::Version;

use super::{
    AcceptsRequest, Adapter, AdapterError, Adoption, DiscoverRequest, DiscoveredUnit, Discovery,
    Edge, EdgeKind, FileWrite, InspectRequest, Inspection, Location, LockfileRequest, Lockfiles,
    PackageProblem, PrepareRequest, Prepared, Publication, Publications, PublishOutcome,
    PublishRequest, Published, PublishedRequest, Tools, ToolsRequest, UnitRef, UnitVersion,
    VersionQuery, VersionWrite, WriteRequest, Writes, join,
};
use mvn::{Build, Deployment, File, Holding, Places, Upload};
use pom::{POM, Pom, coordinates};

pub struct Maven;

/// The suffix of a snapshot version.
const SNAPSHOT: &str = "-SNAPSHOT";

/// The property that holds a unit's version.
const REVISION: &str = "revision";

/// The expressions by which a POM names its own version.
const OWN_VERSION: [&str; 5] = [
    "${revision}",
    "${project.version}",
    "${pom.version}",
    "${version}",
    "${project.parent.version}",
];

impl Adapter for Maven {
    fn discover(&self, request: &DiscoverRequest) -> Result<Discovery, AdapterError> {
        // POMs that cannot be read belong to no unit discovery could find.
        let poms = pom::scan(&request.root)?.poms;
        let roots: Vec<&Pom> = poms.values().filter(|pom| is_unit_root(pom)).collect();
        // An aggregator below another unit's aggregator is one of its
        // modules, whatever it declares.
        let mut nested = BTreeSet::new();
        for root in &roots {
            for file in tree(&poms, root).into_iter().skip(1) {
                nested.insert(file);
            }
        }
        let mut units = Vec::new();
        for root in roots {
            if nested.contains(&root.file) {
                continue;
            }
            let name = root.artifact_id.clone().ok_or_else(|| {
                AdapterError::new(&root.file, "has no `artifactId` to name its unit")
            })?;
            units.push(DiscoveredUnit {
                name,
                path: root.dir.clone(),
            });
        }
        units.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.name.cmp(&b.name)));
        Ok(Discovery { units })
    }

    fn inspect(&self, request: &InspectRequest) -> Result<Inspection, AdapterError> {
        let analysis = Analysis::of(&request.root, &request.units, &request.released)?;
        Ok(Inspection {
            units: analysis.versions(),
            problems: analysis
                .findings
                .iter()
                .map(|finding| PackageProblem {
                    units: finding.units.clone(),
                    message: finding.message.clone(),
                })
                .collect(),
            edges: analysis.edges,
        })
    }

    /// A deployed POM names one version of each dependency, which its
    /// consumers get by Maven's nearest-wins mediation, so a reference
    /// accepts the exact version it names, `[1.3.0]` in Maven's syntax.
    fn accepts(&self, request: &AcceptsRequest) -> Result<bool, AdapterError> {
        let range = request.range.trim();
        let exact = range
            .strip_prefix('[')
            .and_then(|range| range.strip_suffix(']'))
            .unwrap_or(range);
        if exact.is_empty() || exact.contains(['[', '(', ']', ')', ',']) {
            return Err(AdapterError::new(
                &request.file,
                format!(
                    "`{}` is a Maven version range; units refer to each other through version \
                     properties instead",
                    request.range
                ),
            ));
        }
        Ok(exact == request.version)
    }

    /// Writes each released unit's next snapshot as its `revision` and into
    /// every property that refers to it.
    fn write(&self, request: &WriteRequest) -> Result<Writes, AdapterError> {
        let analysis = Analysis::of(&request.root, &request.units, &[])?;
        let mut edits = Edits::default();
        for write in &request.versions {
            let unit = analysis.unit(&write.unit)?;
            let snapshot = next_snapshot(&write.version, &unit.file)?;
            edits.set(&unit.file, REVISION, &snapshot)?;
            for (location, producers) in &analysis.properties {
                if producers.contains(&write.unit) {
                    edits.set(&location.file, &location.keys[1], &snapshot)?;
                }
            }
        }
        for write in &request.references {
            let location = &write.edge.location;
            if is_property(location) {
                let snapshot = next_snapshot(&write.version, &location.file)?;
                edits.set(&location.file, &location.keys[1], &snapshot)?;
            }
        }
        edits.apply(&request.root)
    }

    fn lockfiles(&self, _request: &LockfileRequest) -> Result<Lockfiles, AdapterError> {
        Ok(Lockfiles::default())
    }

    /// Sets every producer property to its producer's `revision`.
    fn adopt(&self, request: &InspectRequest) -> Result<Adoption, AdapterError> {
        let analysis = Analysis::of(&request.root, &request.units, &request.released)?;
        let mut edits = Edits::default();
        let mut problems = Vec::new();
        for finding in &analysis.findings {
            match &finding.fix {
                Some((location, value)) => edits.set(&location.file, &location.keys[1], value)?,
                None => problems.push(PackageProblem {
                    units: finding.units.clone(),
                    message: finding.message.clone(),
                }),
            }
        }
        Ok(Adoption {
            files: edits.apply(&request.root)?.files,
            problems,
        })
    }

    /// Asks each unit's deploy repository, through Maven with isolated
    /// settings and a local repository of the run's own, or an empty one
    /// without the run's directory, for the files of every
    /// module the unit deploys. With the run's directory, the release build
    /// of the working tree tells the files, so that a unit counts as
    /// published only once every file it installed is there; without it,
    /// each module's POM is asked for, and a unit that is not published
    /// gets the highest final version the repository lists of its first
    /// module.
    /// Units are asked in the order of publication, and later ones build
    /// against the versions found for earlier ones.
    fn published(&self, request: &PublishedRequest) -> Result<Publications, AdapterError> {
        let root = &request.root;
        let analysis = Analysis::of(root, &request.units, &request.released)?;
        let mut released = request.released.clone();
        let mut answers: BTreeMap<usize, Publication> = BTreeMap::new();
        for index in analysis.order(&request.queries) {
            let query = &request.queries[index];
            let name = &query.unit.name;
            let file = join(&query.unit.path, POM);
            let mut answer = Publication {
                unit: name.clone(),
                registry: "its Maven deploy repository".to_string(),
                published: false,
                highest: None,
                problem: None,
            };
            let state = match &request.work {
                Some(work) => Workspace::of(work).stage(
                    root,
                    &analysis,
                    &query.unit,
                    &query.version,
                    &released,
                )?,
                None => {
                    // Producers without a known release keep the committed
                    // snapshots.
                    let properties =
                        analysis.build_properties(name, &query.version, &released, false)?;
                    Deployment::resolve(root, &query.unit, &properties, None, None)?.map(
                        |deployment| Staged {
                            deployment,
                            files: None,
                            local: None,
                            plugins: None,
                            held: None,
                        },
                    )
                }
            };
            match state {
                Err(problem) => answer.problem = Some(problem),
                Ok(staged) => {
                    answer.registry = staged.deployment.describe();
                    match staged.holdings(root, &query.version, &file, false)? {
                        Err(problem) => answer.problem = Some(problem),
                        Ok(holdings) if holdings.iter().all(Holding::complete) => {
                            answer.published = true;
                        }
                        // A release commit's pipeline needs only the answer
                        // whether every file is there.
                        Ok(_) if request.work.is_some() => {}
                        Ok(_) => match staged.deployment.modules[0].versions(
                            root,
                            &file,
                            staged.plugins.as_deref(),
                        )? {
                            Ok(versions) => answer.highest = highest_final(&versions),
                            Err(problem) => answer.problem = Some(problem),
                        },
                    }
                }
            }
            let found = if answer.published {
                Some(query.version.clone())
            } else {
                answer.highest.clone()
            };
            if let Some(version) = found
                && !released.iter().any(|write| write.unit == *name)
            {
                released.push(VersionWrite {
                    unit: name.clone(),
                    version,
                });
            }
            answers.insert(index, answer);
        }
        Ok(Publications {
            units: answers.into_values().collect(),
        })
    }

    /// Builds the released units, each with one Maven invocation in the
    /// order of publication, into a local repository of their own, and
    /// checks the POMs each module would deploy.
    fn prepare(&self, request: &PrepareRequest) -> Result<Prepared, AdapterError> {
        if request.packages.is_empty() {
            return Ok(Prepared::default());
        }
        let root = &request.root;
        let analysis = Analysis::of(root, &request.units, &request.released)?;
        let scratch = tempfile::Builder::new()
            .prefix("cutwater-maven-")
            .tempdir()
            .map_err(|err| {
                AdapterError::new(POM, format!("cannot create a local repository: {err}"))
            })?;
        let workspace = Workspace::of(scratch.path());
        let mut problems = Vec::new();
        for index in analysis.order(&request.packages) {
            let query = &request.packages[index];
            let problem = |message: String| PackageProblem {
                units: vec![query.unit.name.clone()],
                message,
            };
            let staged = match workspace.stage(
                root,
                &analysis,
                &query.unit,
                &query.version,
                &request.released,
            )? {
                Ok(staged) => staged,
                Err(message) => {
                    problems.push(problem(message));
                    continue;
                }
            };
            for module in &staged.deployment.modules {
                for message in module.check_installed(&workspace.local, &query.version) {
                    problems.push(problem(message));
                }
            }
        }
        Ok(Prepared { problems })
    }

    fn publishing_tools(&self, _request: &ToolsRequest) -> Result<Tools, AdapterError> {
        Ok(Tools {
            tools: vec!["mvn".to_string()],
        })
    }

    /// Deploys the files of a unit's release that its deploy repository
    /// lacks: those its release build installed into the run's local
    /// repository, built first unless an earlier request built it, and the
    /// SHA-1 of a file it holds without one. Each is uploaded on its own,
    /// so that a retry completes a deployment interrupted between modules
    /// or between the files of one; a module that the repository partly
    /// holds is completed only when the files it holds are those this build
    /// made, since files of two builds would make one release. Afterwards
    /// the repository is asked again, and the unit counts as published only
    /// when it holds every file.
    fn publish(&self, request: &PublishRequest) -> Result<Published, AdapterError> {
        let root = &request.root;
        let unit = &request.package.unit;
        let version = &request.package.version;
        let file = join(&unit.path, POM);
        let analysis = Analysis::of(root, &request.units, &request.released)?;
        let workspace = Workspace::of(&request.work);
        let outcome = |registry: String, outcome| {
            Ok(Published {
                unit: unit.name.clone(),
                registry,
                outcome,
                dist_tag: None,
            })
        };
        let failed = |registry: String, message: String| {
            outcome(registry, PublishOutcome::Failed { message })
        };
        let staged = match workspace.stage(root, &analysis, unit, version, &request.released)? {
            Ok(staged) => staged,
            Err(message) => return failed("its Maven deploy repository".to_string(), message),
        };
        let registry = staged.deployment.describe();
        let holdings = match staged.holdings(root, version, &file, true)? {
            Ok(holdings) => holdings,
            Err(message) => return failed(registry, message),
        };
        if holdings.iter().all(Holding::complete) {
            return outcome(
                registry,
                PublishOutcome::AlreadyPresent {
                    message: format!("it holds every file of {version}"),
                },
            );
        }
        let names = |files: &[File]| {
            files
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mixed: Vec<String> = holdings
            .iter()
            .filter(|holding| !holding.missing.is_empty())
            .filter_map(|holding| {
                let mut found = Vec::new();
                if !holding.differing.is_empty() {
                    found.push(format!(
                        "{}, which differ from this build's",
                        names(&holding.differing)
                    ));
                }
                if !holding.unconfirmed.is_empty() {
                    found.push(format!(
                        "{}, which sign files this build does not match",
                        names(&holding.unconfirmed)
                    ));
                }
                (!found.is_empty()).then(|| {
                    format!(
                        "module `{}:{}` holds {}",
                        holding.module.group_id,
                        holding.module.artifact_id,
                        found.join(", and ")
                    )
                })
            })
            .collect();
        let corrupt: Vec<String> = holdings
            .iter()
            .filter(|holding| !holding.corrupt.is_empty())
            .map(|holding| {
                format!(
                    "module `{}:{}` holds {} with a SHA-1 that does not match",
                    holding.module.group_id,
                    holding.module.artifact_id,
                    names(&holding.corrupt)
                )
            })
            .collect();
        if !corrupt.is_empty() {
            return failed(
                registry,
                format!(
                    "{}, which consumers that check checksums refuse, and nothing was \
                     uploaded. A repository that refuses replacements keeps the wrong SHA-1: \
                     delete the file and its checksums there and retry the job, or release a \
                     new version",
                    corrupt.join("; ")
                ),
            );
        }
        if !mixed.is_empty() {
            return failed(
                registry,
                format!(
                    "{}; the build is not reproducible, so the files it lacks would mix two \
                     builds in one release, and nothing was uploaded. Make the build \
                     reproducible, with `project.build.outputTimestamp`, so that a retry can \
                     complete a deployment, and release a new version",
                    mixed.join("; ")
                ),
            );
        }
        let mut problem = None;
        for holding in holdings.iter().filter(|holding| !holding.complete()) {
            let uploads: Vec<Upload> = holding
                .missing
                .iter()
                .cloned()
                .map(Upload::File)
                .chain(
                    holding
                        .unchecked
                        .iter()
                        .map(|(file, sha1)| Upload::Checksum(file.clone(), sha1.clone())),
                )
                .collect();
            let deployed = holding.module.deploy(
                root,
                &workspace.local,
                version,
                &uploads,
                &file,
                Some(&workspace.plugins),
            )?;
            // The kept copy of a file whose SHA-1 went up lacks it.
            for (unchecked, _) in &holding.unchecked {
                holding.module.forget(&workspace.held, version, unchecked);
            }
            if let Err(message) = deployed {
                problem = Some(message);
                break;
            }
        }
        // The repository's answer, not Maven's output, says what it holds.
        let after = match staged.holdings(root, version, &file, false)? {
            Ok(after) => after,
            Err(message) => return failed(registry, message),
        };
        let incomplete: Vec<String> = after
            .iter()
            .filter(|holding| !holding.complete())
            .map(Holding::describe)
            .collect();
        match (incomplete.is_empty(), problem) {
            (true, None) => outcome(registry, PublishOutcome::Published),
            (true, Some(message)) => outcome(
                registry,
                PublishOutcome::AlreadyPresent {
                    message: format!("it holds every file, although {message}"),
                },
            ),
            (false, problem) => failed(
                registry,
                format!(
                    "{}; {}. Retry the job: it deploys only the missing files. A file the \
                     repository refuses must be fixed there, or the release replaced by a new \
                     version",
                    problem.unwrap_or_else(|| "the deployment left files out".to_string()),
                    incomplete.join("; ")
                ),
            ),
        }
    }
}

/// Where a run keeps its release builds: the local repository they
/// install into, and a record of the units built there.
struct Workspace {
    local: PathBuf,
    built: PathBuf,
    /// The plugins of the questions to deploy repositories.
    plugins: PathBuf,
    /// What the deploy repositories answered, kept for the run's later
    /// rounds, so that each held file is downloaded once.
    held: PathBuf,
}

impl Workspace {
    fn of(dir: &Path) -> Workspace {
        Workspace {
            local: dir.join("maven-repository"),
            built: dir.join("maven-built"),
            plugins: dir.join("maven-plugins"),
            held: dir.join("maven-held"),
        }
    }

    /// The release of `unit` at `version`, built with one Maven invocation
    /// into the local repository unless it was built there before, with
    /// the files it installed; an `Err` inside says why it cannot be.
    fn stage(
        &self,
        root: &Path,
        analysis: &Analysis,
        unit: &UnitRef,
        version: &str,
        released: &[VersionWrite],
    ) -> Result<Result<Staged, String>, AdapterError> {
        let file = join(&unit.path, POM);
        let properties = match analysis.build_properties(&unit.name, version, released, true) {
            Ok(properties) => properties,
            Err(err) => return Ok(Err(err.message)),
        };
        let error = |err: std::io::Error| {
            AdapterError::new(&file, format!("cannot record the release build: {err}"))
        };
        std::fs::create_dir_all(&self.built).map_err(error)?;
        // Later rounds of the run read the deployment the first one found.
        let kept = self.built.join(format!("{}@{version}.xml", unit.name));
        let deployment =
            match Deployment::resolve(root, unit, &properties, Some(&self.local), Some(&kept))? {
                Ok(deployment) => deployment,
                Err(message) => return Ok(Err(message)),
            };
        let marker = self.built.join(format!("{}@{version}", unit.name));
        if !marker.exists() {
            let build = Build {
                root,
                unit,
                properties: &properties,
                local: &self.local,
            };
            if let Err(message) = build.run(&["install"])? {
                return Ok(Err(format!("the release build failed: {message}")));
            }
            std::fs::write(&marker, "").map_err(error)?;
        }
        let mut files = Vec::new();
        for module in &deployment.modules {
            let installed = module.installed(&self.local, version);
            if installed.is_empty() {
                return Ok(Err(format!(
                    "the release build installed no file of module `{}:{}` at {version}",
                    module.group_id, module.artifact_id
                )));
            }
            files.push(installed);
        }
        Ok(Ok(Staged {
            deployment,
            files: Some(files),
            local: Some(self.local.clone()),
            plugins: Some(self.plugins.clone()),
            held: Some(self.held.clone()),
        }))
    }
}

/// A unit's deployment at a version, with the files its release build
/// installed for each module, where it was built.
struct Staged {
    deployment: Deployment,
    files: Option<Vec<Vec<File>>>,
    /// The local repository the build installed into.
    local: Option<PathBuf>,
    plugins: Option<PathBuf>,
    /// Where the run keeps the repositories' answers.
    held: Option<PathBuf>,
}

impl Staged {
    /// What the repository holds, and with `compare`, which of the files
    /// it holds differ from the build's.
    fn holdings(
        &self,
        root: &Path,
        version: &str,
        file: &str,
        compare: bool,
    ) -> Result<Result<Vec<Holding<'_>>, String>, AdapterError> {
        self.deployment.holdings(
            root,
            version,
            self.files.as_deref(),
            file,
            Places {
                built: self.local.as_deref().filter(|_| compare),
                plugins: self.plugins.as_deref(),
                held: self.held.as_deref(),
            },
        )
    }
}

/// Whether a POM is a unit's aggregator: its version is `${revision}` and it
/// defines `revision` itself.
fn is_unit_root(pom: &Pom) -> bool {
    pom.version.as_deref() == Some("${revision}") && pom.property(REVISION).is_some()
}

/// The files of a unit's aggregator and of the modules below it, the
/// aggregator first, as far as the repository holds them.
fn tree(poms: &BTreeMap<String, Pom>, root: &Pom) -> Vec<String> {
    let mut files = vec![root.file.clone()];
    let mut index = 0;
    while index < files.len() {
        if let Some(pom) = poms.get(&files[index]) {
            for (_, module) in &pom.modules {
                if let Some(module) = module
                    && !files.contains(module)
                {
                    files.push(module.clone());
                }
            }
        }
        index += 1;
    }
    files
}

/// The snapshot that follows the release of `version`: the next patch's,
/// or for a prerelease its final version's.
fn next_snapshot(version: &str, file: &str) -> Result<String, AdapterError> {
    let version = Version::parse(version).map_err(|err| {
        AdapterError::new(
            file,
            format!("`{version}` is not a semantic version: {err}"),
        )
    })?;
    let mut next = Version::new(version.major, version.minor, version.patch);
    if version.pre.is_empty() {
        next.patch += 1;
    }
    Ok(format!("{next}{SNAPSHOT}"))
}

/// Whether `revision` is a semantic version followed by `-SNAPSHOT`, such as
/// `1.4.1-SNAPSHOT`.
fn is_snapshot_revision(revision: &str) -> bool {
    revision
        .strip_suffix(SNAPSHOT)
        .and_then(|base| Version::parse(base).ok())
        .is_some_and(|base| base.pre.is_empty() && base.build.is_empty())
}

/// The highest version without a prerelease part among `versions`, which
/// Maven lists; versions that are not semantic versions are left out.
fn highest_final(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .filter_map(|version| Version::parse(version).ok())
        .filter(|version| version.pre.is_empty())
        .max()
        .map(|version| version.to_string())
}

fn is_property(location: &Location) -> bool {
    location.keys.len() == 2 && location.keys[0] == "properties"
}

fn property_location(file: &str, name: &str) -> Location {
    Location {
        file: file.to_string(),
        keys: vec!["properties".to_string(), name.to_string()],
    }
}

/// A unit as the analysis knows it.
struct MavenUnit {
    name: String,
    /// The aggregator POM.
    file: String,
    package: String,
    revision: String,
    /// The files of its POMs, the aggregator first.
    files: Vec<String>,
}

/// What a unit's dependent breaks of the adapter's rules, and the property
/// value that fixes it, when one does.
struct Finding {
    units: Vec<String>,
    message: String,
    fix: Option<(Location, String)>,
}

/// The units of the repository, the references to them and what breaks the
/// adapter's rules, as the POMs say without running Maven.
struct Analysis {
    units: Vec<MavenUnit>,
    edges: Vec<Edge>,
    findings: Vec<Finding>,
    /// Every property through which a POM refers to a unit, with the units
    /// it refers to.
    properties: BTreeMap<Location, BTreeSet<String>>,
}

impl Analysis {
    /// The analysis of the units `requested`, whose released versions
    /// `released` names, as the manifest holds them.
    fn of(
        root: &Path,
        requested: &[UnitRef],
        released: &[VersionWrite],
    ) -> Result<Analysis, AdapterError> {
        let pom::Scan { mut poms, broken } = if requested.is_empty() {
            pom::Scan::default()
        } else {
            pom::scan(root)?
        };
        // A POM that cannot be read fails only a unit it belongs to.
        let readable = |file: &str| -> Result<(), AdapterError> {
            match broken.get(file) {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        };
        // A unit listed by hand may lie where git ignores files.
        for unit in requested {
            let file = join(&unit.path, POM);
            readable(&file)?;
            if let std::collections::btree_map::Entry::Vacant(entry) = poms.entry(file) {
                let pom = pom::read(root, entry.key())?;
                entry.insert(pom);
            }
        }

        let mut units = Vec::new();
        let mut owner: BTreeMap<String, String> = BTreeMap::new();
        for unit in requested {
            let file = join(&unit.path, POM);
            let aggregator = &poms[&file];
            if aggregator.version.as_deref() != Some("${revision}") {
                return Err(AdapterError::new(
                    &file,
                    format!(
                        "unit `{}` sets its version to `{}`, but a Maven unit's aggregator sets \
                         it to `${{revision}}`",
                        unit.name,
                        aggregator.version.as_deref().unwrap_or("nothing")
                    ),
                ));
            }
            let revision = aggregator.property(REVISION).ok_or_else(|| {
                AdapterError::new(
                    &file,
                    format!(
                        "unit `{}` defines no `revision` property, which holds its version",
                        unit.name
                    ),
                )
            })?;
            if !is_snapshot_revision(revision) {
                return Err(AdapterError::new(
                    &file,
                    format!(
                        "unit `{}` sets `revision` to `{revision}`, but a Maven unit's revision is \
                         a semantic version followed by `-SNAPSHOT`, such as `1.0.0-SNAPSHOT`",
                        unit.name
                    ),
                ));
            }
            let group_id = aggregator.effective_group_id().ok_or_else(|| {
                AdapterError::new(&file, format!("unit `{}` has no `groupId`", unit.name))
            })?;
            let artifact_id = aggregator.artifact_id.as_deref().ok_or_else(|| {
                AdapterError::new(&file, format!("unit `{}` has no `artifactId`", unit.name))
            })?;
            let files = tree(&poms, aggregator);
            for module in &files {
                readable(module)?;
                let Some(pom) = poms.get(module) else {
                    return Err(AdapterError::new(
                        &file,
                        format!(
                            "unit `{}` aggregates {module}, which does not exist or git ignores",
                            unit.name
                        ),
                    ));
                };
                if let Some(other) = owner.insert(module.clone(), unit.name.clone()) {
                    return Err(AdapterError::new(
                        module,
                        format!(
                            "is a module of both unit `{other}` and unit `{}`; a module belongs \
                             to one unit",
                            unit.name
                        ),
                    ));
                }
                if *module == file {
                    continue;
                }
                if pom.version.as_deref().is_some_and(|v| v != "${revision}")
                    || pom.property(REVISION).is_some()
                {
                    return Err(AdapterError::new(
                        module,
                        format!(
                            "is a module of unit `{}`, whose modules share its version: give it no \
                             `version` of its own, or `${{revision}}`, and no `revision` property",
                            unit.name
                        ),
                    ));
                }
                // A module without a version of its own takes its parent's,
                // which must be the unit's.
                if pom.version.is_none() && !inherits_revision(&poms, pom, &files) {
                    let parent = pom
                        .parent
                        .as_ref()
                        .map_or("no parent".to_string(), |parent| {
                            format!(
                                "its parent `{}`",
                                coordinates(
                                    parent.group_id.as_deref(),
                                    parent.artifact_id.as_deref().unwrap_or_default()
                                )
                            )
                        });
                    return Err(AdapterError::new(
                        module,
                        format!(
                            "is a module of unit `{}`, but takes its version from {parent}, \
                             outside the unit; give it the version `${{revision}}`, or a parent \
                             of the unit",
                            unit.name
                        ),
                    ));
                }
            }
            for (module, target) in poms
                .get(&file)
                .into_iter()
                .flat_map(|pom| pom.modules.iter())
            {
                if target.is_none() {
                    return Err(AdapterError::new(
                        &file,
                        format!(
                            "the module `{module}` of unit `{}` leaves the repository",
                            unit.name
                        ),
                    ));
                }
            }
            units.push(MavenUnit {
                name: unit.name.clone(),
                file: file.clone(),
                package: coordinates(Some(group_id), artifact_id),
                revision: revision.to_string(),
                files,
            });
        }

        let index = Index::new(&poms, &units);
        let mut analysis = Analysis {
            units,
            edges: Vec::new(),
            findings: Vec::new(),
            properties: BTreeMap::new(),
        };
        let mut out_of_step = BTreeSet::new();
        for pom in poms.values() {
            let dependent = owner.get(&pom.file).cloned();
            if let Some(parent) = &pom.parent
                && let (Some(group_id), Some(artifact_id)) =
                    (parent.group_id.as_deref(), parent.artifact_id.as_deref())
                && let Some(producer) = index.unit_of(group_id, artifact_id)
                && dependent.as_deref() != Some(producer)
            {
                analysis.edges.push(Edge {
                    dependent_path: pom.dir.clone(),
                    dependent_unit: dependent.clone(),
                    dependency: producer.to_string(),
                    kind: EdgeKind::Runtime,
                    reference: parent.version.clone().unwrap_or_default(),
                    range: None,
                    follows: false,
                    published_first: true,
                    location: Location {
                        file: pom.file.clone(),
                        keys: vec!["parent".to_string()],
                    },
                });
                if let Some(dependent) = &dependent {
                    analysis.findings.push(Finding {
                        units: vec![dependent.clone()],
                        message: format!(
                            "{}: unit `{dependent}` inherits from `{group_id}:{artifact_id}` of \
                             unit `{producer}`, but Maven 3 takes no property for a parent's \
                             version; a unit inherits only from its own POMs or from POMs outside \
                             the repository",
                            pom.file
                        ),
                        fix: None,
                    });
                }
            }
            for reference in &pom.references {
                let Some(group_id) = index.group_id(pom, reference.group_id.as_deref()) else {
                    continue;
                };
                let Some(producer) = index.unit_of(&group_id, &reference.artifact_id) else {
                    continue;
                };
                if dependent.as_deref() == Some(producer) {
                    continue;
                }
                let target = coordinates(Some(&group_id), &reference.artifact_id);
                let reference_location = Location {
                    file: pom.file.clone(),
                    keys: reference.keys.clone(),
                };
                let mut edge = Edge {
                    dependent_path: pom.dir.clone(),
                    dependent_unit: dependent.clone(),
                    dependency: producer.to_string(),
                    kind: reference.kind,
                    reference: reference.version.clone().unwrap_or_default(),
                    range: None,
                    follows: false,
                    published_first: reference.published_first,
                    location: reference_location,
                };
                // The dependent's last release names the producer's released
                // version exactly, as its release build passed it on.
                let exact = released
                    .iter()
                    .find(|write| write.unit == producer)
                    .map(|write| format!("[{}]", write.version));
                let revision = analysis.unit(producer)?.revision.clone();
                let revision = &revision;
                let literal = |found: &str| Finding {
                    units: dependent.iter().cloned().collect(),
                    message: format!(
                        "{}: `{target}` of unit `{producer}` has the version `{found}`; a unit \
                         refers to another through a version property, such as \
                         `${{{producer}.version}}`, kept equal to its revision",
                        pom.file
                    ),
                    fix: None,
                };
                match reference.version.as_deref() {
                    // The version is managed, where another reference names it.
                    None => edge.follows = true,
                    Some(version) if OWN_VERSION.contains(&version) => {
                        if dependent.is_some() {
                            analysis.findings.push(literal(version));
                        }
                    }
                    Some(version) => match property_name(version) {
                        Some(name) => match index.property(pom, name) {
                            Some((definer, value)) => {
                                let location = property_location(&definer.file, name);
                                edge.follows = true;
                                edge.range = exact;
                                edge.location = location.clone();
                                analysis
                                    .properties
                                    .entry(location.clone())
                                    .or_default()
                                    .insert(producer.to_string());
                                if value != revision && out_of_step.insert(location.clone()) {
                                    analysis.findings.push(Finding {
                                        units: dependent.iter().cloned().collect(),
                                        message: format!(
                                            "{}: the property `{name}` is `{value}`, but it \
                                             refers to unit `{producer}`, whose revision is \
                                             `{revision}`; keep it equal to the revision, as \
                                             `cutwater version` does",
                                            definer.file
                                        ),
                                        fix: Some((location, revision.clone())),
                                    });
                                }
                            }
                            None => {
                                if dependent.is_some() {
                                    analysis.findings.push(Finding {
                                        units: dependent.iter().cloned().collect(),
                                        message: format!(
                                            "{}: `{target}` of unit `{producer}` has the version \
                                             `{version}`, but neither this POM nor its parents in \
                                             the repository define `{name}`",
                                            pom.file
                                        ),
                                        fix: None,
                                    });
                                }
                            }
                        },
                        None => {
                            if dependent.is_some() {
                                analysis.findings.push(literal(version));
                            }
                        }
                    },
                }
                analysis.edges.push(edge);
            }
        }
        for (location, producers) in &analysis.properties {
            if producers.len() > 1 {
                let names: Vec<String> = producers.iter().map(|name| format!("`{name}`")).collect();
                analysis.findings.push(Finding {
                    units: producers.iter().cloned().collect(),
                    message: format!(
                        "{}: the property `{}` refers to the units {}, which have versions of \
                         their own; give each its own property",
                        location.file,
                        location.keys[1],
                        names.join(", ")
                    ),
                    fix: None,
                });
            }
        }
        Ok(analysis)
    }

    /// The indices of `queries` in the order of publication between their
    /// units, and otherwise in their own order.
    fn order(&self, queries: &[VersionQuery]) -> Vec<usize> {
        let mut graph = petgraph::graph::DiGraph::<usize, ()>::new();
        let nodes: Vec<_> = (0..queries.len())
            .map(|index| graph.add_node(index))
            .collect();
        let position = |name: &str| queries.iter().position(|query| query.unit.name == name);
        for edge in self.edges.iter().filter(|edge| edge.orders_publication()) {
            if let (Some(dependent), Some(dependency)) = (
                edge.dependent_unit.as_deref().and_then(position),
                position(&edge.dependency),
            ) && dependent != dependency
            {
                graph.update_edge(nodes[dependency], nodes[dependent], ());
            }
        }
        match petgraph::algo::toposort(&graph, None) {
            Ok(sorted) => sorted.into_iter().map(|node| graph[node]).collect(),
            Err(_) => (0..queries.len()).collect(),
        }
    }

    fn unit(&self, name: &str) -> Result<&MavenUnit, AdapterError> {
        self.units
            .iter()
            .find(|unit| unit.name == name)
            .ok_or_else(|| AdapterError::new(POM, format!("unknown unit `{name}`")))
    }

    fn versions(&self) -> Vec<UnitVersion> {
        self.units
            .iter()
            .map(|unit| UnitVersion {
                name: unit.name.clone(),
                package: unit.package.clone(),
                version: unit.revision.clone(),
                holds_release: false,
                location: property_location(&unit.file, REVISION),
            })
            .collect()
    }

    /// The properties of a build of `unit` at `version`: `revision`, and
    /// each of its producer properties with the producer's released version
    /// from `released`. A producer without one fails when `strict`, and
    /// otherwise keeps its committed value.
    fn build_properties(
        &self,
        unit: &str,
        version: &str,
        released: &[VersionWrite],
        strict: bool,
    ) -> Result<Vec<(String, String)>, AdapterError> {
        let file = &self.unit(unit)?.file;
        let mut properties = BTreeMap::new();
        properties.insert(REVISION.to_string(), version.to_string());
        for edge in &self.edges {
            if edge.dependent_unit.as_deref() != Some(unit) || !is_property(&edge.location) {
                continue;
            }
            let producer = released.iter().find(|write| write.unit == edge.dependency);
            match producer {
                Some(producer) => {
                    properties.insert(edge.location.keys[1].clone(), producer.version.clone());
                }
                None if strict => {
                    return Err(AdapterError::new(
                        file,
                        format!(
                            "unit `{unit}` depends on unit `{}`, which has no released version \
                             to build against",
                            edge.dependency
                        ),
                    ));
                }
                None => {}
            }
        }
        Ok(properties.into_iter().collect())
    }
}

/// Whether `pom`, which sets no version, inherits `${revision}` from a
/// parent among the unit's POMs `files`.
fn inherits_revision(poms: &BTreeMap<String, Pom>, pom: &Pom, files: &[String]) -> bool {
    pom.parent.as_ref().is_some_and(|parent| {
        parent.version.as_deref() == Some("${revision}")
            && parent.file.as_ref().is_some_and(|file| {
                files.contains(file)
                    && poms
                        .get(file)
                        .is_some_and(|candidate| candidate.artifact_id == parent.artifact_id)
            })
    })
}

/// The name of the property `${name}` that `version` consists of.
fn property_name(version: &str) -> Option<&str> {
    let name = version.strip_prefix("${")?.strip_suffix('}')?;
    (!name.is_empty() && !name.contains(['$', '{', '}'])).then_some(name)
}

/// Lookups across the repository's POMs.
struct Index<'a> {
    poms: &'a BTreeMap<String, Pom>,
    /// The unit of each module, by `groupId:artifactId`.
    modules: BTreeMap<String, String>,
}

impl<'a> Index<'a> {
    fn new(poms: &'a BTreeMap<String, Pom>, units: &[MavenUnit]) -> Index<'a> {
        let mut modules = BTreeMap::new();
        for unit in units {
            for file in &unit.files {
                let pom = &poms[file];
                if let (Some(group_id), Some(artifact_id)) =
                    (pom.effective_group_id(), pom.artifact_id.as_deref())
                {
                    modules.insert(coordinates(Some(group_id), artifact_id), unit.name.clone());
                }
            }
        }
        Index { poms, modules }
    }

    fn unit_of(&self, group_id: &str, artifact_id: &str) -> Option<&str> {
        self.modules
            .get(&coordinates(Some(group_id), artifact_id))
            .map(String::as_str)
    }

    /// A reference's groupId as Maven resolves it, where the POMs say.
    fn group_id(&self, pom: &Pom, group_id: Option<&str>) -> Option<String> {
        let group_id = group_id?;
        match group_id {
            "${project.groupId}" | "${pom.groupId}" | "${groupId}" => {
                pom.effective_group_id().map(str::to_string)
            }
            "${project.parent.groupId}" => pom.parent.as_ref()?.group_id.clone(),
            _ => match property_name(group_id) {
                Some(name) => self.property(pom, name).map(|(_, value)| value.to_string()),
                None => (!group_id.contains("${")).then(|| group_id.to_string()),
            },
        }
    }

    /// The POM of the repository that `pom` inherits from.
    fn parent(&self, pom: &Pom) -> Option<&'a Pom> {
        let parent = pom.parent.as_ref()?;
        let matches = |candidate: &Pom| {
            candidate.artifact_id == parent.artifact_id
                && (parent.group_id.is_none()
                    || candidate.effective_group_id() == parent.group_id.as_deref())
        };
        parent
            .file
            .as_ref()
            .and_then(|file| self.poms.get(file))
            .filter(|candidate| matches(candidate))
            .or_else(|| {
                self.poms
                    .values()
                    .find(|candidate| candidate.file != pom.file && matches(candidate))
            })
    }

    /// Where the property `name` that `pom` sees is defined, in it or the
    /// POMs of the repository it inherits from, with its value.
    fn property<'p>(&self, pom: &'p Pom, name: &str) -> Option<(&'p Pom, &'p str)>
    where
        'a: 'p,
    {
        let mut current = pom;
        let mut seen = BTreeSet::new();
        loop {
            if let Some(value) = current.property(name) {
                return Some((current, value));
            }
            if !seen.insert(current.file.clone()) {
                return None;
            }
            current = self.parent(current)?;
        }
    }
}

/// Property values to set, by file.
#[derive(Default)]
struct Edits {
    files: BTreeMap<String, BTreeMap<String, String>>,
}

impl Edits {
    fn set(&mut self, file: &str, name: &str, value: &str) -> Result<(), AdapterError> {
        let values = self.files.entry(file.to_string()).or_default();
        match values.get(name) {
            Some(other) if other != value => Err(AdapterError::new(
                file,
                format!("the property `{name}` would be both `{other}` and `{value}`"),
            )),
            _ => {
                values.insert(name.to_string(), value.to_string());
                Ok(())
            }
        }
    }

    fn apply(self, root: &Path) -> Result<Writes, AdapterError> {
        let mut files = Vec::new();
        for (file, values) in self.files {
            let text = std::fs::read_to_string(root.join(&file))
                .map_err(|err| AdapterError::new(&file, format!("cannot read it: {err}")))?;
            let content = pom::set_properties(&file, &text, &values)?;
            if content != text {
                files.push(FileWrite {
                    path: file,
                    content,
                });
            }
        }
        Ok(Writes { files })
    }
}

#[cfg(test)]
mod tests;
