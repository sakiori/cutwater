//! `publish`: publication from the release commit. In the pipeline of a
//! unit's release commit it publishes, in the order of publication, what
//! the unit's registry lacks, uploads the unit's release assets, and then
//! tags the release commit and creates the GitLab release of every unit or
//! fixed group whose members are all published. In the pipeline of any
//! other commit it publishes and tags nothing, and checks that the earlier
//! releases were published.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

use super::Repository;
use super::check::disagreements;
use crate::adapter::{
    AdapterKind, PublishOutcome, PublishRequest, PublishedRequest, ToolsRequest, UnitRef,
    VersionQuery,
};
use crate::assets::{self, Asset};
use crate::changelog;
use crate::cli::PublishArgs;
use crate::forge::gitlab::{BotCommand, Connection, GitLab};
use crate::forge::{CreateAccess, Forge, NewRelease, ReleaseLink};
use crate::manifest::{MANIFEST_FILE, Manifest};
use crate::plan;
use crate::release_commit;

/// The access level that creating tags and releases needs.
const DEVELOPER: u32 = 30;
/// The highest role of a member, which a group share or a group membership
/// can give.
const OWNER: u32 = 50;
/// The access level of a protected tag rule for the instance's
/// administrators.
const ADMINISTRATOR: u32 = 60;

/// How long `publish` waits between two questions to a registry.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

pub fn run(args: &PublishArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    super::check_pipeline("publish", &repo.config.default_branch)?;
    let output = Output { json: args.json };
    let mut run = Run::new(&repo, args.dry_run)?;
    if run.released.is_empty() {
        output.say("No unit has a released version to publish.")?;
        return run.finish(&output);
    }
    let forge = GitLab::connect(Connection::from_env(
        &repo.config.gitlab,
        BotCommand::Publish,
    )?)?;
    let clock = SystemClock::new();
    if let Err(err) = run.publish(&forge, &clock, &output) {
        if crate::output::is_broken_pipe(&err) {
            return Err(err);
        }
        run.fail(Severity::Trouble, format!("{err:#}"));
    }
    run.finish(&output)
}

/// Where the account of a run goes: standard output, or standard error
/// when standard output carries the JSON record.
struct Output {
    json: bool,
}

impl Output {
    fn say(&self, line: &str) -> Result<()> {
        if self.json {
            eprintln!("{line}");
        } else {
            crate::outln!("{line}")?;
        }
        Ok(())
    }
}

/// A clock that waits.
pub trait Clock {
    /// The time since an arbitrary start.
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

struct SystemClock {
    start: Instant,
}

impl SystemClock {
    fn new() -> SystemClock {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Asks `listed` until it says yes, every [`POLL_INTERVAL`], for at most
/// `timeout`; whether it said yes, and its last problem.
pub fn wait_listed(
    clock: &dyn Clock,
    timeout: Duration,
    mut listed: impl FnMut() -> Result<bool, String>,
) -> (bool, Option<String>) {
    let deadline = clock.now() + timeout;
    loop {
        let problem = match listed() {
            Ok(true) => return (true, None),
            Ok(false) => None,
            Err(err) => Some(err),
        };
        let now = clock.now();
        if now >= deadline {
            return (false, problem);
        }
        clock.sleep(POLL_INTERVAL.min(deadline - now));
    }
}

/// The record of a run, as `--json` prints it.
#[derive(Debug, Default, Serialize)]
pub struct Record {
    /// The commit of the pipeline.
    pub commit: String,
    pub dry_run: bool,
    pub units: Vec<UnitRecord>,
    pub assets: Vec<AssetRecord>,
    pub tags: Vec<TagRecord>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct UnitRecord {
    pub unit: String,
    pub adapter: AdapterKind,
    pub version: String,
    pub release_commit: String,
    /// The registry asked or published to; `None` when a tag and its
    /// release proved the publication.
    pub registry: Option<String>,
    /// The tag of the unit, or of its fixed group.
    pub tag: String,
    pub state: UnitState,
    /// The npm distribution tag it was published under.
    pub dist_tag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitState {
    /// This run published it.
    Published,
    /// It was published before: the registry lists it, or its tag and
    /// release exist.
    Present,
    /// The registry lacks it; a dry run would publish it.
    WouldPublish,
    /// The registry lacks it, and only its release commit's pipeline
    /// publishes it.
    Missing,
    /// Publishing it failed.
    Failed,
    /// Not published, because a unit it depends on was not.
    Blocked,
    /// Its registry could not say whether it holds it.
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct AssetRecord {
    pub unit: String,
    pub package: String,
    pub version: String,
    pub file: String,
    pub sha256: String,
    pub url: String,
    pub state: AssetState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetState {
    Uploaded,
    Present,
    WouldUpload,
}

#[derive(Debug, Serialize)]
pub struct TagRecord {
    pub tag: String,
    pub commit: String,
    pub units: Vec<String>,
    pub state: TagState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagState {
    /// This run created the tag and its release.
    Created,
    /// The tag existed; this run created its release.
    ReleaseCreated,
    /// The tag and its release existed.
    Present,
    /// A dry run would create the tag and its release.
    WouldCreate,
    /// A dry run would create the release of the existing tag.
    WouldCreateRelease,
    /// Not every unit of the tag is published, so it is not created yet.
    Incomplete,
}

/// A unit whose manifest entry is a released version.
struct Released {
    name: String,
    path: String,
    adapter: AdapterKind,
    version: String,
    release_commit: String,
    owner: usize,
}

/// What a tag and its release stand for: one unit, or a fixed group.
struct Owner {
    name: String,
    group: bool,
    tag: String,
    /// The members, in the order of publication.
    members: Vec<usize>,
    release_commit: String,
    /// The commit the tag points at, when it exists.
    tagged: Option<String>,
    has_release: bool,
}

impl Owner {
    fn proven(&self) -> bool {
        self.tagged.as_deref() == Some(self.release_commit.as_str()) && self.has_release
    }
}

/// A unit whose release commit is HEAD.
#[derive(Debug, Serialize)]
pub struct ReleasedHere {
    pub name: String,
    pub version: String,
    pub adapter: AdapterKind,
    /// The tag of the unit's release, or of its fixed group's.
    pub tag: String,
}

/// HEAD and the units whose release commit it is, in the order of
/// publication: the units the pipeline of HEAD publishes. Versions seeded
/// by `init` have no release commit.
pub fn released_here(repo: &Repository) -> Result<(String, Vec<ReleasedHere>)> {
    let run = Run::new(repo, true)?;
    let units = run
        .released
        .iter()
        .filter(|unit| unit.release_commit == run.head)
        .map(|unit| ReleasedHere {
            name: unit.name.clone(),
            version: unit.version.clone(),
            adapter: unit.adapter,
            tag: run.owners[unit.owner].tag.clone(),
        })
        .collect();
    Ok((run.head.clone(), units))
}

/// How bad a failure is: a broken rule exits with 1, a failure to reach a
/// verdict or to carry it out with 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Severity {
    Violation,
    Trouble,
}

struct Run<'r> {
    repo: &'r Repository,
    dry_run: bool,
    head: String,
    timeout: Duration,
    released: Vec<Released>,
    owners: Vec<Owner>,
    /// The units each unit depends on for publication.
    dependencies: BTreeMap<String, BTreeSet<String>>,
    /// The units of earlier releases whose registries lack their released
    /// version, or could not say.
    unavailable: BTreeSet<String>,
    record: Record,
    severity: Option<Severity>,
}

impl<'r> Run<'r> {
    fn new(repo: &'r Repository, dry_run: bool) -> Result<Run<'r>> {
        let git = &repo.git;
        let head = git
            .commit("HEAD")?
            .context("the repository has no commit at HEAD")?;
        let manifest = Manifest::load(git.root())?;
        let inspected = repo.inspect()?;
        let order = plan::plannable(&repo.units, &inspected.declared, &inspected.edges)?;
        let mut dependencies: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for edge in &inspected.edges {
            if let Some(dependent) = &edge.dependent_unit
                && edge.orders_publication()
                && *dependent != edge.dependency
            {
                dependencies
                    .entry(dependent.clone())
                    .or_default()
                    .insert(edge.dependency.clone());
            }
        }
        let names: Vec<String> = order
            .iter()
            .filter(|unit| manifest.get(unit).is_some_and(|entry| !entry.seeded))
            .cloned()
            .collect();
        let mut run = Run {
            repo,
            dry_run,
            head: head.clone(),
            timeout: Duration::from_secs(repo.config.publish.registry_timeout),
            released: Vec::new(),
            owners: Vec::new(),
            dependencies,
            unavailable: BTreeSet::new(),
            record: Record {
                commit: head,
                dry_run,
                ..Record::default()
            },
            severity: None,
        };
        if names.is_empty() {
            return Ok(run);
        }
        let disagreeing = disagreements(&manifest, &inspected.declared);
        if let Some(disagreement) = disagreeing
            .iter()
            .find(|disagreement| names.contains(&disagreement.unit))
        {
            anyhow::bail!(
                "unit `{}`: {MANIFEST_FILE} has {}, but {} has {}; publish releases only what \
                 `cutwater version` wrote",
                disagreement.unit,
                disagreement.manifest,
                disagreement.location,
                disagreement.declared
            );
        }
        let commits = release_commit::find(git, &repo.config.default_branch, &names)?;
        for name in &names {
            let unit = repo.units.get(name).expect("a released unit is a unit");
            let version = manifest
                .get(name)
                .expect("a released unit has an entry")
                .version
                .to_string();
            let (owner_name, group, template) = match repo.units.group_of(name) {
                Some(group) => (group.name.clone(), true, &group.tag),
                None => (
                    name.clone(),
                    false,
                    unit.tag.as_ref().expect("a unit outside groups has a tag"),
                ),
            };
            let owner = match run
                .owners
                .iter()
                .position(|owner| owner.group == group && owner.name == owner_name)
            {
                Some(owner) => owner,
                None => {
                    run.owners.push(Owner {
                        tag: template.render(&owner_name, &version),
                        name: owner_name,
                        group,
                        members: Vec::new(),
                        release_commit: String::new(),
                        tagged: None,
                        has_release: false,
                    });
                    run.owners.len() - 1
                }
            };
            run.owners[owner].members.push(run.released.len());
            run.released.push(Released {
                name: name.clone(),
                path: unit.path.clone(),
                adapter: unit.adapter,
                version,
                release_commit: commits[name].clone(),
                owner,
            });
        }
        for index in 0..run.owners.len() {
            let mut newest = String::new();
            for &member in &run.owners[index].members {
                let commit = &run.released[member].release_commit;
                if newest.is_empty() || git.is_ancestor(&newest, commit)? {
                    newest = commit.clone();
                }
            }
            run.owners[index].release_commit = newest;
        }
        Ok(run)
    }

    fn fail(&mut self, severity: Severity, message: String) {
        self.record.errors.push(message);
        self.severity = self.severity.max(Some(severity));
    }

    fn warn(&mut self, message: String) {
        self.record.warnings.push(message);
    }

    fn unit_ref(&self, unit: &Released) -> UnitRef {
        UnitRef {
            name: unit.name.clone(),
            path: unit.path.clone(),
        }
    }

    fn unit_refs(&self, kind: AdapterKind) -> Vec<UnitRef> {
        self.repo.unit_refs(kind)
    }

    fn publish(&mut self, forge: &dyn Forge, clock: &dyn Clock, output: &Output) -> Result<()> {
        output.say(&format!("Publishing from {}.", self.head))?;
        for owner in &mut self.owners {
            owner.tagged = forge.tag(&owner.tag)?;
            owner.has_release = owner.tagged.is_some() && forge.has_release(&owner.tag)?;
        }
        let mut own = Vec::new();
        let mut earlier = Vec::new();
        let mut own_misplaced = false;
        for index in 0..self.owners.len() {
            let owner = &self.owners[index];
            let is_own = owner.release_commit == self.head;
            if is_own {
                own.push(index);
            } else {
                earlier.push(index);
            }
            let Some(tagged) = owner.tagged.as_ref() else {
                continue;
            };
            if *tagged == owner.release_commit {
                continue;
            }
            own_misplaced |= is_own;
            let message = format!(
                "the tag `{}` points at {tagged}, but the release commit of {} is {}; tags are \
                 never moved, so delete or rename the tag `{}` on GitLab if it is wrong",
                owner.tag,
                describe(owner),
                owner.release_commit,
                owner.tag
            );
            self.fail(Severity::Violation, message);
        }
        self.check_earlier(forge, &earlier)?;
        if own_misplaced {
            output.say("Nothing was published, uploaded or tagged.")?;
        } else {
            self.release(forge, clock, output, &own)?;
        }
        Ok(())
    }

    /// Checks the releases of earlier commits: their registries must hold
    /// them once their release commits' pipelines finished.
    fn check_earlier(&mut self, forge: &dyn Forge, earlier: &[usize]) -> Result<()> {
        let mut asked = Vec::new();
        for &owner in earlier {
            if self.owners[owner].proven() {
                for member in self.owners[owner].members.clone() {
                    self.push_unit(member, None, UnitState::Present, None);
                }
            } else {
                asked.extend(self.owners[owner].members.iter().copied());
            }
        }
        let answers = self.ask_registries(&asked)?;
        let mut pipelines = BTreeMap::new();
        for (member, answer) in asked.iter().copied().zip(answers) {
            let unit = &self.released[member];
            let commit = unit.release_commit.clone();
            let pipeline = match pipelines.get(&commit) {
                Some(pipeline) => pipeline,
                None => {
                    let found = forge.latest_pipeline(&commit, &self.repo.config.default_branch)?;
                    pipelines.entry(commit.clone()).or_insert(found)
                }
            };
            let retry = match pipeline {
                Some(pipeline) => format!(
                    "retry the `publish` job of pipeline #{} of that commit: {}",
                    pipeline.id, pipeline.url
                ),
                None => "GitLab has no pipeline of that commit whose `publish` job could \
                         publish it"
                    .to_string(),
            };
            let finished = pipeline.as_ref().is_none_or(|pipeline| pipeline.finished());
            let label = format!("`{}` {}", unit.name, unit.version);
            let tag = self.owners[unit.owner].tag.clone();
            let (state, registry) = match answer {
                Answer::Listed(registry) => {
                    self.warn(format!(
                        "{label} is published, but its release commit {commit} has no tag `{tag}` \
                         with a release yet; {retry}"
                    ));
                    (UnitState::Present, registry)
                }
                Answer::Unlisted(registry) if finished => {
                    self.fail(
                        Severity::Violation,
                        format!(
                            "{label} is missing from {registry}, although the pipeline of its \
                             release commit {commit} finished; {retry}"
                        ),
                    );
                    (UnitState::Missing, registry)
                }
                Answer::Unlisted(registry) => {
                    self.warn(format!(
                        "{label} is not yet in {registry}; the pipeline of its release commit \
                         {commit} still runs and publishes it. If it fails, {retry}"
                    ));
                    (UnitState::Missing, registry)
                }
                Answer::Unknown(registry, problem) => {
                    self.fail(
                        Severity::Trouble,
                        format!("cannot tell whether {registry} holds {label}: {problem}"),
                    );
                    (UnitState::Unknown, registry)
                }
            };
            if matches!(state, UnitState::Missing | UnitState::Unknown) {
                self.unavailable.insert(self.released[member].name.clone());
            }
            self.push_unit(member, Some(registry), state, None);
        }
        Ok(())
    }

    /// Publishes, uploads and tags the releases of this pipeline's commit.
    fn release(
        &mut self,
        forge: &dyn Forge,
        clock: &dyn Clock,
        output: &Output,
        own: &[usize],
    ) -> Result<()> {
        let mut open = Vec::new();
        for &owner in own {
            if self.owners[owner].proven() {
                for member in self.owners[owner].members.clone() {
                    self.push_unit(member, None, UnitState::Present, None);
                }
                self.record.tags.push(TagRecord {
                    tag: self.owners[owner].tag.clone(),
                    commit: self.head.clone(),
                    units: self.member_names(owner),
                    state: TagState::Present,
                });
            } else {
                open.push(owner);
            }
        }
        if open.is_empty() {
            output.say("Everything this commit releases is published and tagged.")?;
            return Ok(());
        }

        let members: Vec<usize> = self
            .released
            .iter()
            .enumerate()
            .filter(|(_, unit)| open.contains(&unit.owner))
            .map(|(index, _)| index)
            .collect();
        let earlier_errors = self.record.errors.len();
        self.check_tools(&members);
        self.check_access(forge, &open)?;
        let mut notes = BTreeMap::new();
        for &owner in &open {
            match self.notes(owner) {
                Ok(text) => {
                    notes.insert(owner, text);
                }
                Err(err) => self.fail(Severity::Violation, format!("{err:#}")),
            }
        }
        let mut uploads: BTreeMap<usize, Vec<(Asset, bool)>> = BTreeMap::new();
        for &member in &members {
            match self.check_assets(forge, member) {
                Ok(assets) => {
                    uploads.insert(member, assets);
                }
                Err(err) => self.fail(Severity::Violation, format!("{err:#}")),
            }
        }
        // A release links each file under its name, which tools such as mise
        // take as the downloaded file's name, so a name serves one file.
        for &owner in &open {
            let mut named: BTreeMap<&str, &str> = BTreeMap::new();
            let mut clashes = Vec::new();
            for &member in &self.owners[owner].members {
                for (asset, _) in uploads.get(&member).into_iter().flatten() {
                    let unit = self.released[member].name.as_str();
                    if let Some(other) = named.insert(&asset.name, unit) {
                        clashes.push(format!(
                            "the release assets of `{other}` and `{unit}` are both named `{}`, \
                             but the release `{}` links each file under its name; rename one",
                            asset.name, self.owners[owner].tag
                        ));
                    }
                }
            }
            for clash in clashes {
                self.fail(Severity::Violation, clash);
            }
        }
        let answers = self.ask_registries(&members)?;
        let mut states: BTreeMap<usize, (UnitState, Option<String>, Option<String>)> =
            BTreeMap::new();
        for (&member, answer) in members.iter().zip(answers) {
            let own_commit = self.released[member].release_commit == self.head;
            let unit = &self.released[member];
            let state = match answer {
                Answer::Listed(registry) => (UnitState::Present, Some(registry), None),
                Answer::Unlisted(registry) if own_commit => {
                    (UnitState::WouldPublish, Some(registry), None)
                }
                Answer::Unlisted(registry) => {
                    let message = format!(
                        "`{}` {} is missing from {registry}, but only the pipeline of its \
                         release commit {} publishes it",
                        unit.name, unit.version, unit.release_commit
                    );
                    self.fail(Severity::Violation, message);
                    (UnitState::Missing, Some(registry), None)
                }
                Answer::Unknown(registry, problem) => {
                    let message = format!(
                        "cannot tell whether {registry} holds `{}` {}: {problem}",
                        unit.name, unit.version
                    );
                    self.fail(Severity::Trouble, message);
                    (UnitState::Unknown, Some(registry), None)
                }
            };
            states.insert(member, state);
        }
        for (&member, (state, registry, _)) in &states {
            if *state == UnitState::Present {
                let unit = &self.released[member];
                output.say(&format!(
                    "`{}` {} is in {} already.",
                    unit.name,
                    unit.version,
                    registry.as_deref().unwrap_or_default()
                ))?;
            }
        }
        if self.record.errors.len() > earlier_errors {
            output.say("Nothing was published, uploaded or tagged.")?;
            for (member, (state, registry, dist_tag)) in states {
                self.push_unit(member, registry, state, dist_tag);
            }
            return Ok(());
        }

        // What the registries lack: earlier releases that are missing, and
        // the units of this run that fail or wait for them.
        let mut unpublished = self.unavailable.clone();
        for &member in &members {
            let (state, registry, _) = states[&member].clone();
            if state != UnitState::WouldPublish {
                continue;
            }
            let name = self.released[member].name.clone();
            let blocker = self
                .dependencies
                .get(&name)
                .and_then(|dependencies| dependencies.intersection(&unpublished).next())
                .cloned();
            if let Some(blocker) = blocker {
                let reason = if self.unavailable.contains(&blocker) {
                    let dependency = self
                        .released
                        .iter()
                        .find(|unit| unit.name == blocker)
                        .expect("an unavailable unit is released");
                    format!(
                        "`{blocker}` {}, which it depends on, is missing from its registry; \
                         retry this job once the pipeline of its release commit {} published it",
                        dependency.version, dependency.release_commit
                    )
                } else {
                    format!("`{blocker}`, which it depends on, is not published")
                };
                self.fail(
                    Severity::Trouble,
                    format!("`{name}` is not published, because {reason}"),
                );
                unpublished.insert(name);
                states.insert(member, (UnitState::Blocked, registry, None));
                continue;
            }
            if self.dry_run {
                output.say(&format!(
                    "Would publish `{name}` {} to {}.",
                    self.released[member].version,
                    registry.as_deref().unwrap_or_default()
                ))?;
                continue;
            }
            let state = self.publish_unit(clock, output, member)?;
            if state.0 != UnitState::Published {
                unpublished.insert(name);
            }
            states.insert(member, state);
        }
        for (&member, (state, registry, dist_tag)) in &states {
            self.push_unit(member, registry.clone(), *state, dist_tag.clone());
        }

        for &owner in &open {
            let complete = self.owners[owner].members.iter().all(|member| {
                matches!(
                    states[member].0,
                    UnitState::Published | UnitState::Present | UnitState::WouldPublish
                )
            });
            if !complete {
                let tag = self.owners[owner].tag.clone();
                output.say(&format!(
                    "`{tag}` is not created, because not every unit of it is published."
                ))?;
                self.record.tags.push(TagRecord {
                    tag,
                    commit: self.head.clone(),
                    units: self.member_names(owner),
                    state: TagState::Incomplete,
                });
                continue;
            }
            if let Err(err) = self.tag(forge, output, owner, &notes[&owner], &uploads) {
                self.fail(Severity::Trouble, format!("{err:#}"));
            }
        }
        Ok(())
    }

    /// Publishes one unit and waits for its registry to list it.
    fn publish_unit(
        &mut self,
        clock: &dyn Clock,
        output: &Output,
        member: usize,
    ) -> Result<(UnitState, Option<String>, Option<String>)> {
        let unit = &self.released[member];
        let label = format!("`{}` {}", unit.name, unit.version);
        let adapter = unit.adapter.adapter();
        let query = VersionQuery {
            unit: self.unit_ref(unit),
            version: unit.version.clone(),
        };
        let published = match adapter.publish(&PublishRequest {
            root: self.repo.git.root().to_path_buf(),
            units: self.unit_refs(unit.adapter),
            package: query.clone(),
        }) {
            Ok(published) => published,
            Err(err) => {
                self.fail(Severity::Trouble, format!("cannot publish {label}: {err}"));
                return Ok((UnitState::Failed, None, None));
            }
        };
        let registry = Some(published.registry.clone());
        let dist_tag = published.dist_tag.clone();
        let failed = |run: &mut Self, message: String| {
            run.fail(Severity::Trouble, message);
            Ok((UnitState::Failed, registry.clone(), dist_tag.clone()))
        };
        match &published.outcome {
            PublishOutcome::Published => {}
            PublishOutcome::AlreadyPresent { message } => {
                output.say(&format!(
                    "{} answers that it holds {label} already: {message}",
                    published.registry
                ))?;
            }
            PublishOutcome::Failed { message } => {
                return failed(
                    self,
                    format!(
                        "cannot publish {label} to {}: {message}",
                        published.registry
                    ),
                );
            }
        }
        let root = self.repo.git.root().to_path_buf();
        let (listed, problem) = wait_listed(clock, self.timeout, || {
            let answers = adapter
                .published(&PublishedRequest {
                    root: root.clone(),
                    queries: vec![query.clone()],
                })
                .map_err(|err| err.to_string())?;
            let answer = answers.units.into_iter().next().ok_or("no answer")?;
            match answer.problem {
                Some(problem) => Err(problem),
                None => Ok(answer.published),
            }
        });
        if !listed {
            let mut message = format!(
                "{} does not list {label} {} seconds after publishing it",
                published.registry,
                self.timeout.as_secs()
            );
            if let Some(problem) = problem {
                message.push_str(&format!("; its last answer: {problem}"));
            }
            message.push_str(
                "; retry the job once the registry lists it, or raise `publish.registry-timeout`",
            );
            return failed(self, message);
        }
        let state = if published.outcome == PublishOutcome::Published {
            let under = dist_tag
                .as_ref()
                .map(|tag| format!(" under the distribution tag `{tag}`"))
                .unwrap_or_default();
            output.say(&format!(
                "Published {label} to {}{under}.",
                published.registry
            ))?;
            UnitState::Published
        } else {
            UnitState::Present
        };
        Ok((state, registry, dist_tag))
    }

    /// Uploads the owner's missing assets and creates its tag and release.
    fn tag(
        &mut self,
        forge: &dyn Forge,
        output: &Output,
        owner: usize,
        notes: &str,
        uploads: &BTreeMap<usize, Vec<(Asset, bool)>>,
    ) -> Result<()> {
        let mut links = Vec::new();
        for &member in &self.owners[owner].members.clone() {
            let unit = self
                .repo
                .units
                .get(&self.released[member].name)
                .expect("a unit");
            let package = unit.registry_name.clone();
            let version = self.released[member].version.clone();
            for (asset, present) in &uploads[&member] {
                let url = forge.package_file_url(&package, &version, &asset.name);
                let state = if *present {
                    AssetState::Present
                } else if self.dry_run {
                    AssetState::WouldUpload
                } else {
                    let content = std::fs::read(self.repo.git.root().join(&asset.path))
                        .with_context(|| format!("cannot read the release asset {}", asset.path))?;
                    if assets::sha256(&content) != asset.sha256 {
                        anyhow::bail!(
                            "the release asset {} changed while `publish` ran",
                            asset.path
                        );
                    }
                    forge.upload_package_file(&package, &version, &asset.name, &content)?;
                    output.say(&format!(
                        "Uploaded {} to the generic package `{package}` {version}.",
                        asset.path
                    ))?;
                    AssetState::Uploaded
                };
                links.push(ReleaseLink {
                    name: asset.name.clone(),
                    url: url.clone(),
                    path: format!("/{}", asset.name),
                });
                self.record.assets.push(AssetRecord {
                    unit: unit.name.clone(),
                    package: package.clone(),
                    version: version.clone(),
                    file: asset.name.clone(),
                    sha256: asset.sha256.clone(),
                    url,
                    state,
                });
            }
        }
        let owner_ref = &self.owners[owner];
        let tag = owner_ref.tag.clone();
        let exists = owner_ref.tagged.is_some();
        let state = match (exists, self.dry_run) {
            (false, true) => TagState::WouldCreate,
            (true, true) => TagState::WouldCreateRelease,
            (exists, false) => {
                forge.create_release(&NewRelease {
                    tag: &tag,
                    commit: &self.head,
                    name: &tag,
                    notes,
                    links: &links,
                })?;
                if exists {
                    TagState::ReleaseCreated
                } else {
                    TagState::Created
                }
            }
        };
        output.say(&match state {
            TagState::WouldCreate => {
                format!("Would tag {} as `{tag}` and create its release.", self.head)
            }
            TagState::WouldCreateRelease => format!("Would create the release of `{tag}`."),
            TagState::ReleaseCreated => format!("Created the release of the existing tag `{tag}`."),
            _ => format!("Tagged {} as `{tag}` and created its release.", self.head),
        })?;
        self.record.tags.push(TagRecord {
            tag,
            commit: self.head.clone(),
            units: self.member_names(owner),
            state,
        });
        Ok(())
    }

    fn member_names(&self, owner: usize) -> Vec<String> {
        self.owners[owner]
            .members
            .iter()
            .map(|&member| self.released[member].name.clone())
            .collect()
    }

    fn push_unit(
        &mut self,
        member: usize,
        registry: Option<String>,
        state: UnitState,
        dist_tag: Option<String>,
    ) {
        let unit = &self.released[member];
        self.record.units.push(UnitRecord {
            unit: unit.name.clone(),
            adapter: unit.adapter,
            version: unit.version.clone(),
            release_commit: unit.release_commit.clone(),
            registry,
            tag: self.owners[unit.owner].tag.clone(),
            state,
            dist_tag,
        });
    }

    /// Asks each unit's registry whether it holds the unit's version.
    fn ask_registries(&mut self, members: &[usize]) -> Result<Vec<Answer>> {
        let mut answers: BTreeMap<usize, Answer> = BTreeMap::new();
        for kind in AdapterKind::ALL {
            let asked: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&member| self.released[member].adapter == kind)
                .collect();
            if asked.is_empty() {
                continue;
            }
            let queries = asked
                .iter()
                .map(|&member| VersionQuery {
                    unit: self.unit_ref(&self.released[member]),
                    version: self.released[member].version.clone(),
                })
                .collect();
            let publications = kind.adapter().published(&PublishedRequest {
                root: self.repo.git.root().to_path_buf(),
                queries,
            })?;
            for (member, publication) in asked.into_iter().zip(publications.units) {
                let answer = match publication.problem {
                    Some(problem) => Answer::Unknown(publication.registry, problem),
                    None if publication.published => Answer::Listed(publication.registry),
                    None => Answer::Unlisted(publication.registry),
                };
                answers.insert(member, answer);
            }
        }
        Ok(members
            .iter()
            .map(|member| answers.remove(member).expect("every unit is asked"))
            .collect())
    }

    /// Fails for each tool that publishing the units runs and the `PATH`
    /// lacks.
    fn check_tools(&mut self, members: &[usize]) {
        for kind in AdapterKind::ALL {
            if !members
                .iter()
                .any(|&member| self.released[member].adapter == kind)
            {
                continue;
            }
            let tools = kind.adapter().publishing_tools(&ToolsRequest {
                root: self.repo.git.root().to_path_buf(),
                units: self.unit_refs(kind),
            });
            match tools {
                Ok(tools) => {
                    for tool in tools.tools {
                        if which::which(&tool).is_err() {
                            self.fail(
                                Severity::Trouble,
                                format!(
                                    "publishing the {kind} units needs {tool}, which is not on \
                                     the PATH"
                                ),
                            );
                        }
                    }
                }
                Err(err) => self.fail(Severity::Trouble, err.to_string()),
            }
        }
    }

    /// Fails when the bot may not create a tag it is about to create, or
    /// releases at all.
    fn check_access(&mut self, forge: &dyn Forge, open: &[usize]) -> Result<()> {
        let bot = forge.bot()?;
        let level = forge.access_level(bot.id)?;
        if level < DEVELOPER && !bot.admin {
            self.fail(
                Severity::Violation,
                format!(
                    "the bot `{}` has the access level {} ({}) in the project, but creating tags, \
                     releases and packages needs Developer (30) or higher",
                    bot.username,
                    level,
                    level_name(level)
                ),
            );
            return Ok(());
        }
        // A release of a protected tag needs the right to create the tag,
        // also where the tag exists.
        let tags: Vec<(String, bool)> = open
            .iter()
            .map(|&owner| {
                let owner = &self.owners[owner];
                (owner.tag.clone(), owner.tagged.is_some())
            })
            .collect();
        let protected = forge.protected_tags()?;
        let mut grants = GroupGrants::default();
        for (tag, exists) in tags {
            let rules: Vec<_> = protected
                .iter()
                .filter(|rule| wildcard_match(&rule.pattern, &tag))
                .collect();
            if rules.is_empty() {
                continue;
            }
            let mut allowed = false;
            let mut unknown = Vec::new();
            for access in rules.iter().flat_map(|rule| &rule.create) {
                allowed |= match *access {
                    CreateAccess::Level(0) | CreateAccess::DeployKey(_) => false,
                    CreateAccess::Level(ADMINISTRATOR) => bot.admin,
                    CreateAccess::Level(required) => level >= required,
                    // GitLab also requires a user named by a rule to be a project member.
                    CreateAccess::User(user) => user == bot.id && level > 0,
                    CreateAccess::Group(group) => match grants.grant(forge, group, bot.id) {
                        Grant::Allowed => true,
                        Grant::Denied => false,
                        Grant::Unknown(why) => {
                            unknown.push(format!("group {group}: {why}"));
                            false
                        }
                    },
                };
            }
            let what = if exists {
                format!("the release of the tag `{tag}`")
            } else {
                format!("the tag `{tag}`")
            };
            if !allowed && !unknown.is_empty() {
                self.fail(
                    Severity::Trouble,
                    format!(
                        "cannot tell whether the bot `{}` may create {what}, which a protected \
                         tag allows members of a group to create: {}",
                        bot.username,
                        unknown.join("; ")
                    ),
                );
                continue;
            }
            if !allowed {
                let patterns: Vec<String> = rules
                    .iter()
                    .map(|rule| format!("`{}` ({})", rule.pattern, describe_rule(&rule.create)))
                    .collect();
                self.fail(
                    Severity::Violation,
                    format!(
                        "the bot `{}`, with the access level {} ({}), may not create {what}, \
                         which the protected tag {} covers; allow it to create these tags, or \
                         raise its role",
                        bot.username,
                        level,
                        level_name(level),
                        patterns.join(" and ")
                    ),
                );
            }
        }
        Ok(())
    }

    /// The release notes of an owner: the changelog entry of its version;
    /// for a fixed group, each distinct entry once, under the names of the
    /// members that have it.
    fn notes(&self, owner: usize) -> Result<String> {
        let owner = &self.owners[owner];
        let mut entries = Vec::new();
        for &member in &owner.members {
            let unit = &self.released[member];
            let file = if unit.path == "." {
                "CHANGELOG.md".to_string()
            } else {
                format!("{}/CHANGELOG.md", unit.path)
            };
            let text = std::fs::read_to_string(self.repo.git.root().join(&file))
                .with_context(|| format!("cannot read {file}, the changelog of `{}`", unit.name))?;
            let entry = changelog::released_entry(&text, &unit.version).with_context(|| {
                format!(
                    "{file} has no entry for {}, which the release notes of `{}` take",
                    unit.version, owner.tag
                )
            })?;
            entries.push((unit.name.clone(), entry));
        }
        let sections: Vec<String> = changelog::shared(entries)
            .into_iter()
            .map(|(names, entry)| {
                if owner.group {
                    format!("## {}\n\n{entry}", names.join(", "))
                } else {
                    entry
                }
            })
            .collect();
        Ok(sections.join("\n\n"))
    }

    /// The unit's release assets, each with whether the generic package
    /// registry holds it already; fails for a file it holds with another
    /// checksum.
    fn check_assets(&self, forge: &dyn Forge, member: usize) -> Result<Vec<(Asset, bool)>> {
        let released = &self.released[member];
        let unit = self.repo.units.get(&released.name).expect("a unit");
        if unit.assets.is_empty() {
            return Ok(Vec::new());
        }
        let found = assets::resolve(self.repo.git.root(), unit)?;
        let existing = forge.package_files(&unit.registry_name, &released.version)?;
        let mut checked = Vec::new();
        for asset in found {
            let same_name: Vec<_> = existing
                .iter()
                .filter(|file| file.name == asset.name)
                .collect();
            if let Some(other) = same_name.iter().find(|file| file.sha256 != asset.sha256) {
                anyhow::bail!(
                    "the generic package `{}` {} holds `{}` with the checksum {}, but {} has \
                     {}; a published file is never replaced",
                    unit.registry_name,
                    released.version,
                    asset.name,
                    other.sha256,
                    asset.path,
                    asset.sha256
                );
            }
            let present = !same_name.is_empty();
            checked.push((asset, present));
        }
        Ok(checked)
    }

    /// Prints the record and returns the exit status.
    fn finish(mut self, output: &Output) -> Result<ExitCode> {
        for warning in &self.record.warnings {
            eprintln!("warning: {warning}");
        }
        for error in &self.record.errors {
            eprintln!("error: {error}");
        }
        if output.json {
            self.record.units.sort_by_key(|unit| {
                self.released
                    .iter()
                    .position(|released| released.name == unit.unit)
            });
            crate::outln!(
                "{}",
                serde_json::to_string_pretty(&self.record).expect("the record serializes")
            )?;
        }
        Ok(match self.severity {
            None => ExitCode::SUCCESS,
            Some(Severity::Violation) => ExitCode::from(1),
            Some(Severity::Trouble) => ExitCode::from(2),
        })
    }
}

/// Whether a protected tag rule that names a group lets the bot create the
/// tag. As GitLab decides it, the project must be shared with exactly that
/// group with the Developer role or higher, a share with a group above it
/// or the group owning the project does not count, and the bot must be a
/// member of the group itself, not through a group above it, with the
/// Developer role or higher.
#[derive(Default)]
struct GroupGrants {
    shares: Option<Result<Vec<crate::forge::GroupShare>, String>>,
    grants: BTreeMap<u64, Grant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Grant {
    Allowed,
    Denied,
    /// GitLab's answers do not tell, for the reason given.
    Unknown(String),
}

impl GroupGrants {
    fn grant(&mut self, forge: &dyn Forge, group: u64, bot: u64) -> Grant {
        if let Some(grant) = self.grants.get(&group) {
            return grant.clone();
        }
        let shares = self
            .shares
            .get_or_insert_with(|| match forge.shared_groups() {
                Ok(Some(shares)) => Ok(shares),
                Ok(None) => Err(
                    "GitLab does not list the groups the project is shared with to \
                             the bot"
                        .to_string(),
                ),
                Err(err) => Err(format!("{err:#}")),
            });
        let grant = match shares {
            Err(why) => Grant::Unknown(why.clone()),
            Ok(shares) => {
                let shared = shares.iter().any(|share| {
                    share.group == group && (DEVELOPER..=OWNER).contains(&share.access)
                });
                if !shared {
                    Grant::Denied
                } else {
                    match forge.direct_group_member(group, bot) {
                        Ok(Some(level)) if (DEVELOPER..=OWNER).contains(&level) => Grant::Allowed,
                        Ok(_) => Grant::Denied,
                        Err(err) => Grant::Unknown(format!("{err:#}")),
                    }
                }
            }
        };
        self.grants.insert(group, grant.clone());
        grant
    }
}

/// A registry's answer about a version.
enum Answer {
    Listed(String),
    Unlisted(String),
    Unknown(String, String),
}

fn describe(owner: &Owner) -> String {
    if owner.group {
        format!("fixed group `{}`", owner.name)
    } else {
        format!("`{}`", owner.name)
    }
}

/// Whether a protected tag's name or wildcard, in which `*` stands for any
/// text, matches `tag`.
fn wildcard_match(pattern: &str, tag: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = tag.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

fn level_name(level: u32) -> &'static str {
    match level {
        0 => "no member",
        1..=9 => "minimal access",
        10..=14 => "Guest",
        15..=19 => "Planner",
        20..=24 => "Reporter",
        25..=29 => "Security Manager",
        30..=39 => "Developer",
        40..=49 => "Maintainer",
        50..=59 => "Owner",
        _ => "administrator",
    }
}

fn describe_rule(create: &[CreateAccess]) -> String {
    let rules: Vec<String> = create
        .iter()
        .map(|access| match access {
            CreateAccess::Level(0) => "no one".to_string(),
            CreateAccess::Level(ADMINISTRATOR) => "administrators".to_string(),
            CreateAccess::Level(level) => format!("{} or higher", level_name(*level)),
            CreateAccess::User(user) => format!("user {user}"),
            CreateAccess::Group(group) => format!(
                "Developers and above of group {group}, where the project is shared with it"
            ),
            CreateAccess::DeployKey(key) => format!("deploy key {key}"),
        })
        .collect();
    if rules.is_empty() {
        "no one may create".to_string()
    } else {
        format!("may be created by {}", rules.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct FakeClock {
        now: Cell<Duration>,
        sleeps: Cell<usize>,
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.now.get()
        }

        fn sleep(&self, duration: Duration) {
            self.now.set(self.now.get() + duration);
            self.sleeps.set(self.sleeps.get() + 1);
        }
    }

    fn clock() -> FakeClock {
        FakeClock {
            now: Cell::new(Duration::from_secs(100)),
            sleeps: Cell::new(0),
        }
    }

    #[test]
    fn waits_until_the_registry_lists_the_version() {
        let clock = clock();
        let mut asked = 0;
        let (listed, problem) = wait_listed(&clock, Duration::from_secs(60), || {
            asked += 1;
            match asked {
                1 => Ok(false),
                2 => Err("timeout".to_string()),
                _ => Ok(true),
            }
        });
        assert!(listed);
        assert_eq!(problem, None);
        assert_eq!(asked, 3);
        assert_eq!(clock.sleeps.get(), 2);
    }

    #[test]
    fn gives_up_after_the_timeout() {
        let clock = clock();
        let mut asked = 0;
        let (listed, problem) = wait_listed(&clock, Duration::from_secs(12), || {
            asked += 1;
            Err("not yet".to_string())
        });
        assert!(!listed);
        assert_eq!(problem.as_deref(), Some("not yet"));
        // At 0, 5, 10 and 12 seconds: the last wait is cut to the deadline.
        assert_eq!(asked, 4);
        assert_eq!(clock.now.get(), Duration::from_secs(112));
    }

    #[test]
    fn listed_at_once_needs_no_wait() {
        let clock = clock();
        let (listed, _) = wait_listed(&clock, Duration::from_secs(1), || Ok(true));
        assert!(listed);
        assert_eq!(clock.sleeps.get(), 0);
    }

    #[test]
    fn protected_tag_wildcards() {
        let cases = [
            ("*", "api-v1.0.0", true),
            ("api-v*", "api-v1.0.0", true),
            ("*-v*", "@acme/core-v1.0.0", true),
            ("v*", "api-v1.0.0", false),
            ("api-v1.0.0", "api-v1.0.0", true),
            ("api-v1.0.0", "api-v1.0.01", false),
            ("*@*.*.*", "@acme/core@1.2.3", true),
            ("*@*.*.*", "@acme/core@1.2", false),
            ("*.*.*", "1.2.3", true),
            ("*.*.*", "1.2.3-rc.1", true),
            ("*.*.*", "v1.2", false),
        ];
        for (pattern, tag, matches) in cases {
            assert_eq!(wildcard_match(pattern, tag), matches, "{pattern} {tag}");
        }
    }
}
