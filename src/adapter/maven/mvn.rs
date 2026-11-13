//! The Maven invocations of the adapter: the effective deployment of a
//! unit, the release builds, and the questions to and deployments into its
//! deploy repository. Plugins are named with their versions, so that the
//! answers do not depend on what the environment's settings resolve.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use roxmltree::{Document, Node};

use super::super::{AdapterError, UnitRef, join, run_tool, stream_tool};
use super::pom::{POM, child, field, named, text_of};

const HELP_PLUGIN: &str = "org.apache.maven.plugins:maven-help-plugin:3.5.1";
const DEPENDENCY_PLUGIN: &str = "org.apache.maven.plugins:maven-dependency-plugin:3.8.1";
const DEPLOY_PLUGIN: &str = "org.apache.maven.plugins:maven-deploy-plugin:3.1.4";

/// The plugins the adapter runs.
#[cfg(test)]
pub const PLUGINS: [&str; 3] = [HELP_PLUGIN, DEPENDENCY_PLUGIN, DEPLOY_PLUGIN];

/// The extensions of detached signatures and attestations. The default
/// `aether.checksums.omitChecksumsForExtensions` of the Maven Resolver of
/// Maven 3.9.7 to 3.9.9 names `.asc` and `.sigstore`, later ones
/// `.sigstore.json` too, and Maven neither uploads nor asks for checksums
/// of those; requiring none for all three costs nothing where an older
/// Maven uploaded one.
const DETACHED: [&str; 3] = [".asc", ".sigstore", ".sigstore.json"];

/// The extensions of the files Maven Resolver uploads no checksums of,
/// [`DETACHED`] and the checksums, so that an uploaded checksum gets none
/// of its own.
const CHECKSUMS_WITHOUT_CHECKSUMS: &str = "-Daether.checksums.omitChecksumsForExtensions=\
                                           .asc,.sigstore,.sigstore.json,.sha1,.md5,.sha256,.sha512";

/// The version that the first line of `mvn --version` names, as in
/// `Apache Maven 3.9.16 (…)`.
pub fn maven_version(output: &str) -> Result<semver::Version, String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Apache Maven "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|version| semver::Version::parse(version).ok())
        .ok_or_else(|| {
            format!(
                "cannot tell the version of Maven from `mvn --version`: {}",
                output.lines().next().unwrap_or_default().trim()
            )
        })
}

/// Whether the adapter works with Maven `version`: 3.9 or a later Maven 3,
/// whose `maven.repo.local.tail`, reading of `.mvn/maven.config` and
/// checksum defaults it relies on.
pub fn supported(version: &semver::Version) -> Result<(), String> {
    if version.major == 3 && version.minor >= 9 {
        Ok(())
    } else {
        Err(format!(
            "Maven units need Maven 3.9 or a later Maven 3, but the `mvn` on the PATH is Maven \
             {version}"
        ))
    }
}

/// Checks once per run, before the first Maven step, that the `mvn` on the
/// `PATH` is a Maven the adapter works with.
fn require_maven(root: &Path, file: &str) -> Result<(), AdapterError> {
    static CHECKED: std::sync::OnceLock<Result<(), AdapterError>> = std::sync::OnceLock::new();
    CHECKED
        .get_or_init(|| {
            let mut command = mvn(root);
            command.arg("--version");
            let output = run_tool(&mut command, "mvn", file, "building Maven units")?;
            maven_version(&printed(&output))
                .and_then(|version| supported(&version))
                .map_err(|problem| AdapterError::new(file, problem))
        })
        .clone()
}

/// Maven in batch mode, without download progress, in `dir`.
fn mvn(dir: &Path) -> Command {
    let mut command = Command::new("mvn");
    command
        .args(["--batch-mode", "--no-transfer-progress"])
        .current_dir(dir);
    command
}

/// Everything Maven printed.
fn printed(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// What Maven's output says went wrong: its error lines, without the advice
/// it adds to every failure, or the end of its output without any.
pub fn errors(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix("[ERROR]"))
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("To see the full stack trace")
                && !line.starts_with("Re-run Maven using")
                && !line.starts_with("For more information about the errors")
                && !line.starts_with("[Help ")
                && !line.starts_with("After correcting the problems")
                && !line.starts_with("mvn <args> -rf")
        })
        .collect();
    if lines.is_empty() {
        let all: Vec<&str> = output.lines().collect();
        all[all.len().saturating_sub(20)..].join("\n")
    } else {
        lines.join("\n")
    }
}

/// A repository that modules deploy to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Repository {
    pub id: String,
    pub url: String,
}

impl Repository {
    /// A repository as the deploy plugin's `alt*DeploymentRepository`
    /// properties give it: `id::url`, or `id::layout::url`.
    fn parse_alternative(value: &str) -> Option<Repository> {
        let parts: Vec<&str> = value.split("::").collect();
        let (id, url) = match parts.as_slice() {
            [id, url] | [id, _, url] => (*id, *url),
            _ => return None,
        };
        (!id.is_empty() && !url.is_empty()).then(|| Repository {
            id: id.to_string(),
            url: url.to_string(),
        })
    }

    pub fn describe(&self) -> String {
        format!("the Maven repository `{}` at {}", self.id, self.url)
    }

    /// A directory name of the repository, for what a run keeps of it.
    fn directory(&self) -> String {
        self.id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
}

/// A file of a module's release, as a Maven repository holds it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct File {
    /// The file's name, `<artifactId>-<version>[-<classifier>].<extension>`.
    pub name: String,
    pub classifier: Option<String>,
    pub extension: String,
}

impl File {
    /// The file `name` of `artifact_id` at `version`; `None` for a name of
    /// another shape. A classifier ends at the first dot, so that an
    /// extension such as `tar.gz` or `jar.asc` stays whole.
    pub fn parse(name: &str, artifact_id: &str, version: &str) -> Option<File> {
        let rest = name.strip_prefix(&format!("{artifact_id}-{version}"))?;
        let (classifier, extension) = match rest.strip_prefix('.') {
            Some(extension) => (None, extension),
            None => {
                let (classifier, extension) = rest.strip_prefix('-')?.split_once('.')?;
                (Some(classifier), extension)
            }
        };
        let valid =
            !extension.is_empty() && classifier.is_none_or(|classifier| !classifier.is_empty());
        valid.then(|| File {
            name: name.to_string(),
            classifier: classifier.map(str::to_string),
            extension: extension.to_string(),
        })
    }

    fn pom(artifact_id: &str, version: &str) -> File {
        File {
            name: format!("{artifact_id}-{version}.pom"),
            classifier: None,
            extension: "pom".to_string(),
        }
    }

    fn is_pom(&self) -> bool {
        self.classifier.is_none() && self.extension == "pom"
    }

    /// The name of the file this one signs or attests, when it is a detached
    /// signature or attestation.
    pub fn signed(&self) -> Option<&str> {
        let mut extensions = DETACHED;
        // The longest first, so that `.sigstore.json` is not taken for
        // `.json`.
        extensions.sort_by_key(|extension| std::cmp::Reverse(extension.len()));
        extensions
            .iter()
            .find_map(|extension| self.name.strip_suffix(extension))
    }
}

/// A module that a unit deploys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub group_id: String,
    pub artifact_id: String,
    pub repository: Repository,
}

/// What a deploy repository holds of a module's release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holding<'a> {
    pub module: &'a Module,
    pub present: Vec<File>,
    pub missing: Vec<File>,
    /// Files it holds without their SHA-1, with the SHA-1 of what it holds.
    pub unchecked: Vec<(File, String)>,
    /// Files whose SHA-1 there does not match what it holds, which a
    /// repository that refuses replacements keeps wrong.
    pub corrupt: Vec<File>,
    /// Files it holds whose bytes differ from those a build installed, but
    /// for detached signatures, which differ whenever they are made again.
    pub differing: Vec<File>,
    /// Detached signatures it holds of a file that it lacks or holds with
    /// other bytes than the build's, so that nothing confirms they sign
    /// what the build made.
    pub unconfirmed: Vec<File>,
}

impl Holding<'_> {
    /// Whether the repository holds every file and its SHA-1.
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.unchecked.is_empty() && self.corrupt.is_empty()
    }

    /// The module, what the repository holds of it and what it lacks.
    pub fn describe(&self) -> String {
        let names = |files: &mut dyn Iterator<Item = &File>| {
            files
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let present = if self.present.is_empty() {
            "nothing".to_string()
        } else {
            names(&mut self.present.iter())
        };
        let mut lacks = Vec::new();
        if !self.missing.is_empty() {
            lacks.push(names(&mut self.missing.iter()));
        }
        if !self.unchecked.is_empty() {
            lacks.push(format!(
                "the SHA-1 of {}",
                names(&mut self.unchecked.iter().map(|(file, _)| file))
            ));
        }
        if !self.corrupt.is_empty() {
            lacks.push(format!(
                "a SHA-1 that matches {}",
                names(&mut self.corrupt.iter())
            ));
        }
        format!(
            "module `{}` has {present} but lacks {}",
            self.module.name(),
            lacks.join(" and ")
        )
    }
}

/// A file to upload: one a build installed, or the SHA-1 of a file the
/// repository holds without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upload {
    File(File),
    Checksum(File, String),
}

/// Where a run keeps what its questions use, each optional.
#[derive(Debug, Clone, Copy, Default)]
pub struct Places<'a> {
    /// The local repository of the build, whose files the held ones are
    /// compared with.
    pub built: Option<&'a Path>,
    /// The plugins of the questions, resolved once.
    pub plugins: Option<&'a Path>,
    /// The answers, kept for the run's later rounds instead of an empty
    /// local repository for each.
    pub held: Option<&'a Path>,
}

/// The modules a unit deploys at a version, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub modules: Vec<Module>,
}

impl Deployment {
    /// The modules that the unit deploys and their deploy repositories,
    /// from the effective POMs of the unit's reactor with `properties`, the
    /// released version as `revision` among them, which one Maven
    /// invocation writes, resolving from the local repository `local` when
    /// one is given; with `keep`, the file that keeps them for later rounds
    /// of the same run, read instead where it exists. An `Err` inside says
    /// why they are unknown.
    pub fn resolve(
        root: &Path,
        unit: &UnitRef,
        properties: &[(String, String)],
        local: Option<&Path>,
        keep: Option<&Path>,
    ) -> Result<Result<Deployment, String>, AdapterError> {
        let file = join(&unit.path, POM);
        require_maven(root, &file)?;
        if let Some(kept) = keep
            && let Ok(text) = std::fs::read_to_string(kept)
        {
            return Ok(Deployment::parse(&text, &unit.name));
        }
        let scratch = scratch_dir(&file)?;
        let output_file = scratch.path().join("effective-pom.xml");
        let mut command = mvn(&root.join(&unit.path));
        if let Some(local) = local {
            command.arg(format!("-Dmaven.repo.local={}", local.display()));
        }
        for (name, value) in properties {
            command.arg(format!("-D{name}={value}"));
        }
        command
            .arg(format!("{HELP_PLUGIN}:effective-pom"))
            .arg(format!("-Doutput={}", output_file.display()));
        let output = run_tool(&mut command, "mvn", &file, "finding the deploy repository")?;
        if !output.status.success() {
            return Ok(Err(format!(
                "Maven cannot read the effective POMs of unit `{}`: {}",
                unit.name,
                errors(&printed(&output))
            )));
        }
        let text = std::fs::read_to_string(&output_file).map_err(|err| {
            AdapterError::new(&file, format!("cannot read Maven's effective POMs: {err}"))
        })?;
        if let Some(kept) = keep {
            std::fs::write(kept, &text).map_err(|err| {
                AdapterError::new(&file, format!("cannot keep Maven's effective POMs: {err}"))
            })?;
        }
        Ok(Deployment::parse(&text, &unit.name))
    }

    /// The deployment that the effective POMs `text` describe.
    pub fn parse(text: &str, unit: &str) -> Result<Deployment, String> {
        let document =
            Document::parse(text).map_err(|err| format!("Maven's effective POMs: {err}"))?;
        let top = document.root_element();
        let projects: Vec<Node> = match top.tag_name().name() {
            "projects" => named(top, "project").collect(),
            _ => vec![top],
        };
        let mut modules = Vec::new();
        for project in projects {
            let group_id = field(project, "groupId").unwrap_or_default();
            let artifact_id = field(project, "artifactId").unwrap_or_default();
            if skips_deployment(project) {
                continue;
            }
            let property = |name: &str| {
                child(project, "properties")
                    .and_then(|properties| field(properties, name))
                    .filter(|value| !value.is_empty())
            };
            let repository = property("altReleaseDeploymentRepository")
                .or_else(|| property("altDeploymentRepository"))
                .and_then(|value| Repository::parse_alternative(&value))
                .or_else(|| {
                    let repository = child(project, "distributionManagement")
                        .and_then(|management| child(management, "repository"))?;
                    Some(Repository {
                        id: field(repository, "id")?,
                        url: field(repository, "url")?,
                    })
                });
            let Some(repository) = repository else {
                return Err(format!(
                    "module `{group_id}:{artifact_id}` of unit `{unit}` names no repository to \
                     deploy releases to: give it a `distributionManagement` repository, or set \
                     `altReleaseDeploymentRepository`"
                ));
            };
            modules.push(Module {
                group_id,
                artifact_id,
                repository,
            });
        }
        if modules.is_empty() {
            return Err(format!("unit `{unit}` deploys no module"));
        }
        Ok(Deployment { modules })
    }

    /// The deploy repositories, as messages name them.
    pub fn describe(&self) -> String {
        let mut repositories: Vec<&Repository> = Vec::new();
        for module in &self.modules {
            if !repositories.contains(&&module.repository) {
                repositories.push(&module.repository);
            }
        }
        repositories
            .iter()
            .map(|repository| repository.describe())
            .collect::<Vec<_>>()
            .join(" and ")
    }

    /// What each module's deploy repository holds of `version`: of the
    /// files a release build installed, `files` in the order of the
    /// modules, or else of each module's POM, with one Maven invocation per
    /// repository, [`Query`], in the [`Places`] of the run. An `Err` inside
    /// says why a repository could not tell.
    pub fn holdings(
        &self,
        root: &Path,
        version: &str,
        files: Option<&[Vec<File>]>,
        unit_file: &str,
        places: Places,
    ) -> Result<Result<Vec<Holding<'_>>, String>, AdapterError> {
        let Places {
            built,
            plugins,
            held,
        } = places;
        let expected: Vec<Vec<File>> = self
            .modules
            .iter()
            .enumerate()
            .map(|(index, module)| match files {
                Some(files) => files[index].clone(),
                None => vec![File::pom(&module.artifact_id, version)],
            })
            .collect();
        let mut holdings: Vec<Holding> = self
            .modules
            .iter()
            .map(|module| Holding {
                module,
                present: Vec::new(),
                missing: Vec::new(),
                unchecked: Vec::new(),
                corrupt: Vec::new(),
                differing: Vec::new(),
                unconfirmed: Vec::new(),
            })
            .collect();
        let mut repositories: Vec<&Repository> = Vec::new();
        for module in &self.modules {
            if !repositories.contains(&&module.repository) {
                repositories.push(&module.repository);
            }
        }
        for repository in repositories {
            let members: Vec<usize> = (0..self.modules.len())
                .filter(|&index| self.modules[index].repository == *repository)
                .collect();
            let asked: Vec<(usize, Asked)> = members
                .iter()
                .flat_map(|&index| {
                    let module = &self.modules[index];
                    expected[index].iter().map(move |file| {
                        (
                            index,
                            Asked {
                                group_id: &module.group_id,
                                artifact_id: &module.artifact_id,
                                version,
                                file,
                            },
                        )
                    })
                })
                .collect();
            let query = Query::new(root, repository, unit_file, plugins, held)?;
            let questions: Vec<Asked> = asked.iter().map(|(_, asked)| *asked).collect();
            let saved = match query.ask(&questions)? {
                Ok(saved) => saved,
                Err(problem) => {
                    return Ok(Err(format!(
                        "Maven could not ask {} for {version}: {problem}",
                        repository.url
                    )));
                }
            };
            for &index in &members {
                let answers: Vec<(File, Option<PathBuf>)> = asked
                    .iter()
                    .zip(&saved)
                    .filter(|((owner, _), _)| *owner == index)
                    .map(|((_, asked), saved)| (asked.file.clone(), saved.clone()))
                    .collect();
                holdings[index] =
                    holding(&self.modules[index], version, answers, built).map_err(|err| {
                        AdapterError::new(unit_file, format!("cannot read an answer: {err}"))
                    })?;
            }
        }
        Ok(Ok(holdings))
    }
}

/// What the repository holds of `module`'s files at `version`, from the
/// `answers` to the questions: each file with the copy Maven saved of it,
/// `None` where the repository lacks it. Detached signatures need no SHA-1,
/// as Maven never asks for one; where the build's local repository `built`
/// is given, the held files are compared with the build's, a detached
/// signature by the file it signs.
pub fn holding<'a>(
    module: &'a Module,
    version: &str,
    answers: Vec<(File, Option<PathBuf>)>,
    built: Option<&Path>,
) -> std::io::Result<Holding<'a>> {
    let mut holding = Holding {
        module,
        present: Vec::new(),
        missing: Vec::new(),
        unchecked: Vec::new(),
        corrupt: Vec::new(),
        differing: Vec::new(),
        unconfirmed: Vec::new(),
    };
    for (file, saved) in answers {
        let Some(path) = saved else {
            holding.missing.push(file);
            continue;
        };
        if file.signed().is_none() {
            let held = sha1(&path)?;
            if let Some(built) = built {
                let own = module.installed_dir(built, version).join(&file.name);
                if sha1(&own).ok().as_ref() != Some(&held) {
                    holding.differing.push(file.clone());
                }
            }
            let mut checksum = path.into_os_string();
            checksum.push(".sha1");
            // Maven saves the repository's SHA-1 even where it does not
            // match, when its checksum policy only warns.
            match std::fs::read_to_string(&checksum) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    holding.unchecked.push((file.clone(), held));
                }
                Err(err) => return Err(err),
                Ok(stored) => {
                    let stored = stored.split_whitespace().next().unwrap_or_default();
                    if !stored.eq_ignore_ascii_case(&held) {
                        holding.corrupt.push(file.clone());
                    }
                }
            }
        }
        holding.present.push(file);
    }
    let lacking = |name: &str| {
        holding
            .missing
            .iter()
            .chain(&holding.differing)
            .any(|file| file.name == name)
    };
    let unconfirmed: Vec<File> = holding
        .present
        .iter()
        .filter(|file| file.signed().is_some_and(lacking))
        .cloned()
        .collect();
    holding.unconfirmed = unconfirmed;
    Ok(holding)
}

impl Module {
    fn name(&self) -> String {
        format!("{}:{}", self.group_id, self.artifact_id)
    }

    /// Forgets the copy of `file` at `version` that the run keeps under
    /// `held`, so that the next round asks the repository for it again.
    pub fn forget(&self, held: &Path, version: &str, file: &File) {
        let dir = self.installed_dir(&held.join(self.repository.directory()), version);
        for suffix in ["", ".sha1", ".md5"] {
            let _ = std::fs::remove_file(dir.join(format!("{}{suffix}", file.name)));
        }
    }

    fn installed_dir(&self, local: &Path, version: &str) -> PathBuf {
        local
            .join(self.group_id.replace('.', "/"))
            .join(&self.artifact_id)
            .join(version)
    }

    /// The files of the module at `version` that a build installed into
    /// the local repository `local`: its POM, main artifact and attached
    /// artifacts, without checksums and the local repository's own files.
    pub fn installed(&self, local: &Path, version: &str) -> Vec<File> {
        let Ok(entries) = std::fs::read_dir(self.installed_dir(local, version)) else {
            return Vec::new();
        };
        let mut files: Vec<File> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let own = name == "_remote.repositories"
                    || name.starts_with("maven-metadata")
                    || name.ends_with(".lastUpdated")
                    || [".sha1", ".md5", ".sha256", ".sha512"]
                        .iter()
                        .any(|suffix| name.ends_with(suffix));
                if own {
                    return None;
                }
                File::parse(&name, &self.artifact_id, version)
            })
            .collect();
        files.sort();
        files
    }

    /// The versions the module's deploy repository lists, from the
    /// metadata Maven fetches into an empty local repository to resolve
    /// its latest release.
    pub fn versions(
        &self,
        root: &Path,
        file: &str,
        plugins: Option<&Path>,
    ) -> Result<Result<Vec<String>, String>, AdapterError> {
        let query = Query::new(root, &self.repository, file, plugins, None)?;
        let artifact = format!("{}:{}:RELEASE:pom", self.group_id, self.artifact_id);
        let output = query.get(&artifact)?;
        let text = printed(&output);
        if let Some(line) = text
            .lines()
            .find(|line| line.contains("Could not transfer metadata"))
        {
            return Ok(Err(format!(
                "Maven could not read the versions of `{}` from {}: {}",
                self.name(),
                self.repository.url,
                line.trim_start_matches("[WARNING]").trim()
            )));
        }
        let dir = query
            .local
            .path()
            .join(self.group_id.replace('.', "/"))
            .join(&self.artifact_id);
        let mut versions = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Ok(versions));
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("maven-metadata") && name.ends_with(".xml")) {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).map_err(|err| {
                AdapterError::new(file, format!("cannot read Maven's metadata {name}: {err}"))
            })?;
            match metadata_versions(&text) {
                Ok(listed) => versions.extend(listed),
                Err(problem) => return Ok(Err(problem)),
            }
        }
        Ok(Ok(versions))
    }

    /// Uploads `uploads` of the module's release, which a build installed
    /// into `local`, one at a time with the deploy plugin's `deploy-file`,
    /// the POM last; an `Err` inside holds Maven's errors. Each upload of
    /// another file names the installed POM and the `pom` packaging, so that
    /// the plugin uploads that file alone and takes no POM from a JAR, and a
    /// checksum goes up without a checksum of its own.
    pub fn deploy(
        &self,
        root: &Path,
        local: &Path,
        version: &str,
        uploads: &[Upload],
        unit_file: &str,
        plugins: Option<&Path>,
    ) -> Result<Result<(), String>, AdapterError> {
        let query = Query::new(root, &self.repository, unit_file, plugins, None)?;
        // Copies outside the local repository, from which the plugin does
        // not deploy, and next to which it writes a POM it takes from a JAR.
        let staged = scratch_dir(unit_file)?;
        let installed = self.installed_dir(local, version);
        let error = |name: &str, err: std::io::Error| {
            AdapterError::new(
                unit_file,
                format!("cannot copy {name} for its deployment: {err}"),
            )
        };
        let copy = |file: &File| -> Result<PathBuf, AdapterError> {
            let path = staged.path().join(&file.name);
            std::fs::copy(installed.join(&file.name), &path)
                .map_err(|err| error(&file.name, err))?;
            Ok(path)
        };
        let pom = copy(&File::pom(&self.artifact_id, version))?;
        let is_pom = |upload: &&Upload| matches!(upload, Upload::File(file) if file.is_pom());
        let mut ordered: Vec<&Upload> = uploads.iter().filter(|upload| !is_pom(upload)).collect();
        ordered.extend(uploads.iter().filter(is_pom));
        for upload in ordered {
            let mut command = query.command();
            command
                .arg(format!("{DEPLOY_PLUGIN}:deploy-file"))
                .arg(format!("-DgroupId={}", self.group_id))
                .arg(format!("-DartifactId={}", self.artifact_id))
                .arg(format!("-Dversion={version}"))
                .arg(format!("-DrepositoryId={}", self.repository.id))
                .arg(format!("-Durl={}", self.repository.url))
                .arg("-Dpackaging=pom");
            let (name, file, extension) = match upload {
                Upload::File(file) if file.is_pom() => {
                    command.arg(format!("-Dfile={}", pom.display()));
                    (file.name.clone(), file, None)
                }
                Upload::File(file) => {
                    command.arg(format!("-Dfile={}", copy(file)?.display()));
                    (file.name.clone(), file, Some(file.extension.clone()))
                }
                Upload::Checksum(file, sha1) => {
                    let name = format!("{}.sha1", file.name);
                    let path = staged.path().join(&name);
                    std::fs::write(&path, sha1).map_err(|err| error(&name, err))?;
                    command
                        .arg(format!("-Dfile={}", path.display()))
                        .arg(CHECKSUMS_WITHOUT_CHECKSUMS);
                    (name, file, Some(format!("{}.sha1", file.extension)))
                }
            };
            if let Some(extension) = extension {
                command
                    .arg(format!("-DpomFile={}", pom.display()))
                    .arg(format!("-Dextension={extension}"));
                if let Some(classifier) = &file.classifier {
                    command.arg(format!("-Dclassifier={classifier}"));
                }
            }
            let output = stream_tool(&mut command, "mvn", unit_file, "deploying the release")?;
            if !output.status.success() {
                return Ok(Err(format!(
                    "deploying {name} of `{}` failed: {}",
                    self.name(),
                    errors(&printed(&output))
                )));
            }
        }
        Ok(Ok(()))
    }

    /// What is wrong with the POM the release build installed for the
    /// module, which is the POM it deploys: a parent, dependency or managed
    /// dependency, also of a profile, or its own version, that is a
    /// snapshot or holds a property reference.
    pub fn check_installed(&self, local: &Path, version: &str) -> Vec<String> {
        let name = self.name();
        let path = self
            .installed_dir(local, version)
            .join(format!("{}-{version}.pom", self.artifact_id));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return vec![format!(
                "the release build installed no POM of module `{name}` at {version}"
            )];
        };
        installed_problems(&name, &text)
    }
}

/// The problems of the POM `text` that module `name` would deploy.
pub fn installed_problems(name: &str, text: &str) -> Vec<String> {
    let document = match Document::parse(text) {
        Ok(document) => document,
        Err(err) => return vec![format!("the POM module `{name}` would deploy: {err}")],
    };
    let project = document.root_element();
    let mut versions: Vec<(String, String)> = Vec::new();
    if let Some(version) = field(project, "version") {
        versions.push(("its own version".to_string(), version));
    }
    if let Some(parent) = child(project, "parent") {
        versions.push((
            format!(
                "the parent `{}:{}`",
                field(parent, "groupId").unwrap_or_default(),
                field(parent, "artifactId").unwrap_or_default()
            ),
            field(parent, "version").unwrap_or_default(),
        ));
    }
    let dependencies = |node: Node, place: &str, versions: &mut Vec<(String, String)>| {
        let lists = [
            (child(node, "dependencies"), "dependency"),
            (
                child(node, "dependencyManagement").and_then(|node| child(node, "dependencies")),
                "managed dependency",
            ),
        ];
        for (list, kind) in lists {
            for dependency in list.into_iter().flat_map(|list| named(list, "dependency")) {
                if let Some(version) = field(dependency, "version") {
                    versions.push((
                        format!(
                            "the {kind} `{}:{}`{place}",
                            field(dependency, "groupId").unwrap_or_default(),
                            field(dependency, "artifactId").unwrap_or_default()
                        ),
                        version,
                    ));
                }
            }
        }
    };
    dependencies(project, "", &mut versions);
    for profile in child(project, "profiles")
        .into_iter()
        .flat_map(|profiles| named(profiles, "profile"))
    {
        let id = field(profile, "id").unwrap_or_default();
        dependencies(profile, &format!(" of the profile `{id}`"), &mut versions);
    }
    let mut problems = Vec::new();
    for (what, version) in versions {
        if version.contains("${") {
            problems.push(format!(
                "module `{name}` would deploy a POM in which {what} has the version `{version}`, \
                 an unresolved property; configure the flatten plugin to interpolate \
                 `dependencies` and `dependencyManagement`"
            ));
        } else if version.ends_with("-SNAPSHOT") {
            problems.push(format!(
                "module `{name}` would deploy a POM in which {what} has the snapshot version \
                 `{version}`"
            ));
        }
    }
    problems
}

/// The release build of a unit: one Maven invocation in the unit's
/// directory, with the released versions on the command line and the
/// job's own local repository, its output shown as it runs.
pub struct Build<'a> {
    pub root: &'a Path,
    pub unit: &'a UnitRef,
    /// `revision` and the producer properties, with released versions.
    pub properties: &'a [(String, String)],
    pub local: &'a Path,
}

impl Build<'_> {
    /// Runs the build with `args`, such as the phase; an `Err` inside holds
    /// Maven's errors.
    pub fn run(&self, args: &[&str]) -> Result<Result<(), String>, AdapterError> {
        let file = join(&self.unit.path, POM);
        require_maven(self.root, &file)?;
        let mut command = mvn(&self.root.join(&self.unit.path));
        command.arg(format!("-Dmaven.repo.local={}", self.local.display()));
        for (name, value) in self.properties {
            command.arg(format!("-D{name}={value}"));
        }
        command.args(args);
        let output = stream_tool(&mut command, "mvn", &file, "building the release")?;
        Ok(if output.status.success() {
            Ok(())
        } else {
            Err(errors(&printed(&output)))
        })
    }
}

/// The versions that Maven metadata lists.
pub fn metadata_versions(text: &str) -> Result<Vec<String>, String> {
    let document = Document::parse(text).map_err(|err| format!("Maven's metadata: {err}"))?;
    Ok(child(document.root_element(), "versioning")
        .and_then(|versioning| child(versioning, "versions"))
        .into_iter()
        .flat_map(|versions| named(versions, "version"))
        .filter_map(text_of)
        .collect())
}

/// Whether the output `text` of Maven's questions tells that the files it
/// did not save are absent from the repository: `answers` name each file
/// asked about, in Maven's notation and in the order of the reactor's
/// projects, with whether Maven saved it, and each unsaved one must be named
/// as not found there. An `Err` with the errors that matter when Maven could
/// not tell, as when the repository is unreachable or denies access.
pub fn absence(text: &str, answers: &[(String, bool)]) -> Result<(), String> {
    let not_found = |artifact: &String| {
        text.contains(&format!("Could not find artifact {artifact} in "))
            || text.contains(&format!("{artifact} was not found in "))
    };
    let answered = !text.contains("Could not transfer")
        && answers
            .iter()
            .filter(|(_, saved)| !saved)
            .all(|(artifact, _)| not_found(artifact));
    if answered {
        return Ok(());
    }
    // `list-classes` fails for every saved file that is no JAR, which says
    // nothing about the repository.
    let all = errors(text);
    let relevant: Vec<&str> = all
        .lines()
        .filter(|line| {
            !answers.iter().enumerate().any(|(index, (_, saved))| {
                *saved && line.contains(&format!("project file-{index}:"))
            })
        })
        .collect();
    Err(if relevant.is_empty() {
        all
    } else {
        relevant.join("\n")
    })
}

/// The SHA-1 of the file `path`, in lowercase hex, as Maven repositories
/// keep it, read in a stream.
fn sha1(path: &Path) -> std::io::Result<String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Maven for one deploy repository, isolated from everything else the
/// environment configures: it runs in throwaway projects outside the
/// repository, [`query_pom`], with an empty local repository or the one the
/// run keeps for the repository's answers, the [`Isolation`] settings, an
/// empty global settings file, and neither `MAVEN_ARGS` nor `.mavenrc`, but
/// with the settings-security file the build's command line names. Given a
/// directory for plugins, it resolves the plugins it runs into that
/// directory once, and from there on reads them from it as a tail of the
/// local repository, which then holds nothing but what the deploy
/// repository answered.
struct Query {
    project: tempfile::TempDir,
    local: Local,
    settings: tempfile::TempDir,
    security: Option<PathBuf>,
    repository: Repository,
    /// The tail of the local repository: the plugins resolved ahead, or
    /// an empty directory, in place of any tail the environment sets.
    tail: Tail,
}

enum Tail {
    Plugins(PathBuf),
    Empty(tempfile::TempDir),
}

impl Tail {
    fn path(&self) -> &Path {
        match self {
            Tail::Plugins(path) => path,
            Tail::Empty(dir) => dir.path(),
        }
    }
}

/// The local repository of the answers: empty for one question, or kept
/// for a run, where a file the repository held stays and is not asked for
/// again, and a missing one is asked for again each round.
enum Local {
    Empty(tempfile::TempDir),
    Kept(PathBuf),
}

impl Local {
    fn path(&self) -> &Path {
        match self {
            Local::Empty(dir) => dir.path(),
            Local::Kept(path) => path,
        }
    }
}

/// A file of a module's release, as Maven names an artifact.
#[derive(Clone, Copy)]
struct Asked<'a> {
    group_id: &'a str,
    artifact_id: &'a str,
    version: &'a str,
    file: &'a File,
}

impl Asked<'_> {
    /// The file as Maven's messages name it.
    fn notation(&self) -> String {
        let classifier = self
            .file
            .classifier
            .as_ref()
            .map(|classifier| format!(":{classifier}"))
            .unwrap_or_default();
        format!(
            "{}:{}:{}{classifier}:{}",
            self.group_id, self.artifact_id, self.file.extension, self.version
        )
    }

    /// The file as the dependency plugin's `artifact` parameter takes it.
    fn coordinates(&self) -> String {
        let classifier = self
            .file
            .classifier
            .as_ref()
            .map(|classifier| format!(":{classifier}"))
            .unwrap_or_default();
        format!(
            "{}:{}:{}:{}{classifier}",
            self.group_id, self.artifact_id, self.version, self.file.extension
        )
    }
}

/// A project of its own, `artifact`, whose only enabled repository is the
/// one asked about, its id and URL interpolated from the command line.
/// Central's artifacts are turned off, so that a public artifact of the
/// same name cannot answer; the [`Isolation`] settings add no other
/// repository and send no question about this one to a mirror. With `file`,
/// its `validate` phase resolves that file alone with the dependency
/// plugin's `list-classes`, which asks Maven Resolver for the artifact
/// without reading its POM; the goal fails afterwards for a file that is no
/// JAR, which changes nothing about what it resolved. `modules` are the
/// projects it aggregates.
fn query_pom(artifact: &str, file: Option<&str>, modules: &str) -> String {
    let build = file
        .map(|file| {
            format!(
                r#"  <build>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-dependency-plugin</artifactId>
        <version>{}</version>
        <executions>
          <execution>
            <id>ask</id>
            <phase>validate</phase>
            <goals><goal>list-classes</goal></goals>
            <configuration><artifact>{}</artifact></configuration>
          </execution>
        </executions>
      </plugin>
    </plugins>
  </build>
"#,
                DEPENDENCY_PLUGIN.rsplit(':').next().unwrap_or_default(),
                escape(file)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>cutwater</groupId>
  <artifactId>{artifact}</artifactId>
  <version>0</version>
  <packaging>pom</packaging>
{modules}  <repositories>
    <repository>
      <id>${{cutwater.repository.id}}</id>
      <url>${{cutwater.repository.url}}</url>
      <releases><enabled>true</enabled></releases>
      <snapshots><enabled>false</enabled></snapshots>
    </repository>
    <repository>
      <id>central</id>
      <url>https://repo.maven.apache.org/maven2</url>
      <releases><enabled>false</enabled></releases>
      <snapshots><enabled>false</enabled></snapshots>
    </repository>
  </repositories>
{build}</project>
"#
    )
}

impl Query {
    /// Maven for `repository`, as the build of the unit whose POM is `file`
    /// would reach it, with the plugins in `plugins` and the answers kept
    /// in `kept`, where given.
    fn new(
        root: &Path,
        repository: &Repository,
        file: &str,
        plugins: Option<&Path>,
        kept: Option<&Path>,
    ) -> Result<Query, AdapterError> {
        let error = |err: std::io::Error| {
            AdapterError::new(
                file,
                format!("cannot create a throwaway project to ask the deploy repository: {err}"),
            )
        };
        require_maven(root, file)?;
        // The build runs in the unit's directory, from which Maven reads its
        // command line.
        let dir = root.join(file.rsplit_once('/').map_or(".", |(dir, _)| dir));
        let options = Options::of(&dir, &std::env::var("MAVEN_ARGS").unwrap_or_default());
        let project = scratch_dir(file)?;
        std::fs::write(
            project.path().join(POM),
            query_pom("repository-query", None, ""),
        )
        .map_err(error)?;
        let settings = scratch_dir(file)?;
        let isolation = Isolation::read(&dir, &repository.id, &options)?;
        std::fs::write(settings.path().join("settings.xml"), isolation.render()).map_err(error)?;
        std::fs::write(settings.path().join("global.xml"), "<settings/>\n").map_err(error)?;
        let local = match kept {
            Some(kept) => Local::Kept(kept.join(repository.directory())),
            None => Local::Empty(scratch_dir(file)?),
        };
        let query = Query {
            project,
            local,
            settings,
            security: options.security,
            repository: repository.clone(),
            tail: Tail::Empty(scratch_dir(file)?),
        };
        let Some(plugins) = plugins else {
            return Ok(query);
        };
        let ready = plugins.join(".resolved");
        if !ready.exists() {
            let mut command = query.with_local(plugins, query.tail.path());
            command
                .arg(format!("{DEPENDENCY_PLUGIN}:help"))
                .arg(format!("{DEPLOY_PLUGIN}:help"));
            let output = run_tool(&mut command, "mvn", file, "asking the deploy repository")?;
            // Without the plugins resolved ahead, each question resolves them.
            if !output.status.success() {
                return Ok(query);
            }
            std::fs::write(&ready, "").map_err(error)?;
        }
        Ok(Query {
            tail: Tail::Plugins(plugins.to_path_buf()),
            ..query
        })
    }

    fn command(&self) -> Command {
        self.with_local(self.local.path(), self.tail.path())
    }

    /// Maven with the query's settings and project, the local repository
    /// `local` and its tail `tail`.
    fn with_local(&self, local: &Path, tail: &Path) -> Command {
        let mut command = mvn(self.project.path());
        command
            .env_remove("MAVEN_ARGS")
            .env("MAVEN_SKIP_RC", "true")
            .arg("--settings")
            .arg(self.settings.path().join("settings.xml"))
            .arg("--global-settings")
            .arg(self.settings.path().join("global.xml"))
            .arg(format!("-Dmaven.repo.local={}", local.display()))
            .arg(format!("-Dmaven.repo.local.tail={}", tail.display()))
            .arg(format!("-Dcutwater.repository.id={}", self.repository.id))
            .arg(format!("-Dcutwater.repository.url={}", self.repository.url));
        if let Some(security) = &self.security {
            command.arg(format!("-Dsettings.security={}", security.display()));
        }
        command
    }

    fn get(&self, artifact: &str) -> Result<Output, AdapterError> {
        let mut command = self.command();
        command
            .arg(format!("{DEPENDENCY_PLUGIN}:get"))
            .arg(format!("-Dartifact={artifact}"))
            .arg("-Dtransitive=false");
        run_tool(&mut command, "mvn", POM, "asking the deploy repository")
    }

    /// Asks for every file of `asked` with one Maven invocation: a reactor
    /// of one project per file, run to the end however many fail, each of
    /// which saves its file into the local repository, with its SHA-1 where
    /// the repository has one, if the repository holds it. Whether each is
    /// there, or Maven's errors when it could not tell.
    fn ask(&self, asked: &[Asked]) -> Result<Result<Vec<Option<PathBuf>>, String>, AdapterError> {
        let error = |err: std::io::Error| {
            AdapterError::new(POM, format!("cannot write the questions: {err}"))
        };
        let dir = self.project.path();
        for entry in std::fs::read_dir(dir).map_err(error)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(path).map_err(error)?;
            }
        }
        let mut modules = String::new();
        for (index, asked) in asked.iter().enumerate().skip(1) {
            let name = format!("file-{index}");
            std::fs::create_dir(dir.join(&name)).map_err(error)?;
            std::fs::write(
                dir.join(&name).join(POM),
                query_pom(&name, Some(&asked.coordinates()), ""),
            )
            .map_err(error)?;
            modules.push_str(&format!("    <module>{name}</module>\n"));
        }
        let first = asked.first().map(Asked::coordinates);
        let modules = if modules.is_empty() {
            String::new()
        } else {
            format!("  <modules>\n{modules}  </modules>\n")
        };
        std::fs::write(
            dir.join(POM),
            query_pom("file-0", first.as_deref(), &modules),
        )
        .map_err(error)?;
        let mut command = self.command();
        command.args(["--fail-at-end", "validate"]);
        if matches!(self.local, Local::Kept(_)) {
            // A file the repository lacked last round is asked for again.
            command.arg("--update-snapshots");
        }
        let output = run_tool(&mut command, "mvn", POM, "asking the deploy repository")?;
        let saved: Vec<Option<PathBuf>> = asked
            .iter()
            .map(|asked| {
                let path = self
                    .local
                    .path()
                    .join(asked.group_id.replace('.', "/"))
                    .join(asked.artifact_id)
                    .join(asked.version)
                    .join(&asked.file.name);
                path.is_file().then_some(path)
            })
            .collect();
        let answers: Vec<(String, bool)> = asked
            .iter()
            .zip(&saved)
            .map(|(asked, saved)| (asked.notation(), saved.is_some()))
            .collect();
        Ok(absence(&printed(&output), &answers).map(|()| saved))
    }
}

/// `value` as XML character data.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The settings that reach one deploy repository: its `<server>` entry
/// and those of the mirrors, kept as written so that `${env.…}` references
/// and passwords encrypted with the user's settings-security still work,
/// the proxies, and the mirrors, none of which mirrors the deploy
/// repository, so that they only serve Maven's plugins; no profile and no
/// other repository.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Isolation {
    repository: String,
    /// Every server entry, by id, as written.
    servers: Vec<(String, String)>,
    proxies: Vec<String>,
    /// The mirrors, by id, each excluding the repository.
    mirrors: Vec<(String, String)>,
}

impl Isolation {
    /// The isolation of `repository` from the settings that Maven reads for
    /// a build in `dir`, by `options`.
    fn read(dir: &Path, repository: &str, options: &Options) -> Result<Isolation, AdapterError> {
        let mut isolation = Isolation {
            repository: repository.to_string(),
            ..Isolation::default()
        };
        let user = options.user.clone().or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".m2/settings.xml"))
        });
        let global = options.global.clone().or_else(|| {
            let mvn = std::fs::canonicalize(which::which("mvn").ok()?).ok()?;
            Some(mvn.parent()?.parent()?.join("conf/settings.xml"))
        });
        for path in [user, global].into_iter().flatten() {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let other = Isolation::parse(&text, repository).map_err(|problem| {
                let shown = path.strip_prefix(dir).unwrap_or(&path);
                AdapterError::new(shown.display().to_string(), problem)
            })?;
            isolation.merge(other);
        }
        Ok(isolation)
    }

    /// The parts of the settings `text` that can reach `repository`.
    pub fn parse(text: &str, repository: &str) -> Result<Isolation, String> {
        let document =
            Document::parse(text).map_err(|err| format!("is not Maven settings: {err}"))?;
        let settings = document.root_element();
        let raw = |node: Node| text[node.range()].to_string();
        let section = |name: &str, item: &'static str| -> Vec<Node> {
            child(settings, name)
                .into_iter()
                .flat_map(move |list| named(list, item))
                .collect()
        };
        let servers = section("servers", "server")
            .into_iter()
            .filter_map(|server| Some((field(server, "id")?, raw(server))))
            .collect();
        let proxies = section("proxies", "proxy").into_iter().map(raw).collect();
        let mut mirrors = Vec::new();
        for mirror in section("mirrors", "mirror") {
            let (Some(id), Some(of)) = (field(mirror, "id"), child(mirror, "mirrorOf")) else {
                continue;
            };
            // Maven takes the first pattern that names the repository, so the
            // exclusion comes first.
            let value = text_of(of).unwrap_or_default();
            let start = mirror.range().start;
            let range = of.range();
            let mut text = raw(mirror);
            text.replace_range(
                range.start - start..range.end - start,
                &format!("<mirrorOf>!{repository},{value}</mirrorOf>"),
            );
            mirrors.push((id, text));
        }
        Ok(Isolation {
            repository: repository.to_string(),
            servers,
            proxies,
            mirrors,
        })
    }

    /// Adds `other`, whose entries come later, as global settings come after
    /// user settings: the first entry of an id counts.
    fn merge(&mut self, other: Isolation) {
        for (id, server) in other.servers {
            if !self.servers.iter().any(|(known, _)| *known == id) {
                self.servers.push((id, server));
            }
        }
        self.proxies.extend(other.proxies);
        for (id, mirror) in other.mirrors {
            if !self.mirrors.iter().any(|(known, _)| *known == id) {
                self.mirrors.push((id, mirror));
            }
        }
    }

    /// The settings: the server entries of the repository and of the
    /// mirrors, through which Maven resolves its plugins, the proxies and
    /// the mirrors.
    pub fn render(&self) -> String {
        let list = |name: &str, items: Vec<&str>| {
            if items.is_empty() {
                String::new()
            } else {
                format!("  <{name}>\n    {}\n  </{name}>\n", items.join("\n    "))
            }
        };
        let reached =
            |id: &str| id == self.repository || self.mirrors.iter().any(|(mirror, _)| mirror == id);
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <settings xmlns=\"http://maven.apache.org/SETTINGS/1.0.0\">\n{}{}{}</settings>\n",
            list(
                "servers",
                self.servers
                    .iter()
                    .filter(|(id, _)| reached(id))
                    .map(|(_, server)| server.as_str())
                    .collect()
            ),
            list("proxies", self.proxies.iter().map(String::as_str).collect()),
            list(
                "mirrors",
                self.mirrors
                    .iter()
                    .map(|(_, mirror)| mirror.as_str())
                    .collect()
            )
        )
    }
}

/// What the command line of a build in a directory says about Maven's
/// settings: the user and global settings files and the settings-security
/// file, as `MAVEN_ARGS` and the `.mvn/maven.config` that Maven finds from
/// that directory upward name them, relative to the directory.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub user: Option<PathBuf>,
    pub global: Option<PathBuf>,
    pub security: Option<PathBuf>,
}

impl Options {
    /// The options of a build in `dir`, with `MAVEN_ARGS` as `maven_args`.
    /// The shell splits `MAVEN_ARGS` into words, while each line of
    /// `maven.config` is one argument.
    pub fn of(dir: &Path, maven_args: &str) -> Options {
        let command_line: Vec<String> = maven_args.split_whitespace().map(str::to_string).collect();
        let config: Vec<String> = dir
            .ancestors()
            .find(|candidate| candidate.join(".mvn").is_dir())
            .and_then(|base| std::fs::read_to_string(base.join(".mvn/maven.config")).ok())
            .map(|config| {
                config
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Options::parse(dir, &command_line, &config)
    }

    /// The options that the command line `command_line` and `maven.config`'s
    /// arguments `config` give, with paths relative to `dir`, read as Maven
    /// reads them: `-s <file>`, `-s<file>`, `--settings <file>` or
    /// `--settings=<file>`, and so on for `--global-settings` (`-gs`), where
    /// the first occurrence counts, the command line's before
    /// `maven.config`'s; and `-D settings.security=<file>`,
    /// `-Dsettings.security=<file>` or `--define`, where the last occurrence
    /// counts and the command line's wins.
    pub fn parse(dir: &Path, command_line: &[String], config: &[String]) -> Options {
        let first = |arguments: &[String], short: &str, long: &str| {
            Options::values(arguments, short, long).into_iter().next()
        };
        let all: Vec<String> = command_line.iter().chain(config).cloned().collect();
        let security = |arguments: &[String]| {
            Options::values(arguments, "-D", "--define")
                .into_iter()
                .filter_map(|definition| {
                    definition
                        .strip_prefix("settings.security=")
                        .map(str::to_string)
                })
                .next_back()
        };
        Options {
            user: first(&all, "-s", "--settings").map(|path| dir.join(path)),
            global: first(&all, "-gs", "--global-settings").map(|path| dir.join(path)),
            security: security(command_line)
                .or_else(|| security(config))
                .map(|path| dir.join(path)),
        }
    }

    /// The values `arguments` give the option `short` or `long`, in order.
    fn values(arguments: &[String], short: &str, long: &str) -> Vec<String> {
        let mut values = Vec::new();
        let mut index = 0;
        while index < arguments.len() {
            let argument = arguments[index].as_str();
            index += 1;
            // `-gs` is no `-s` with the value `gs…`: no other option starts
            // with `-s`, and `-gs` is asked for on its own.
            if short == "-s" && argument.starts_with("-gs") {
                continue;
            }
            if argument == short || argument == long {
                if let Some(next) = arguments.get(index) {
                    values.push(next.clone());
                    index += 1;
                }
            } else if let Some(value) = argument.strip_prefix(&format!("{long}=")) {
                values.push(value.to_string());
            } else if let Some(value) = argument
                .strip_prefix(short)
                .filter(|value| !value.is_empty() && !argument.starts_with("--"))
            {
                values.push(value.to_string());
            }
        }
        values
    }
}

/// An empty temporary directory outside the repository.
fn scratch_dir(file: &str) -> Result<tempfile::TempDir, AdapterError> {
    tempfile::Builder::new()
        .prefix("cutwater-maven-")
        .tempdir()
        .map_err(|err| {
            AdapterError::new(file, format!("cannot create a temporary directory: {err}"))
        })
}

/// Whether an effective POM skips deployment, through `maven.deploy.skip`
/// or the deploy plugin's `skip`.
fn skips_deployment(project: Node) -> bool {
    let truthy = |value: Option<String>| value.as_deref() == Some("true");
    let property = child(project, "properties").and_then(|node| field(node, "maven.deploy.skip"));
    if truthy(property) {
        return true;
    }
    let plugins = child(project, "build").and_then(|build| child(build, "plugins"));
    plugins
        .into_iter()
        .flat_map(|plugins| named(plugins, "plugin"))
        .filter(|plugin| field(*plugin, "artifactId").as_deref() == Some("maven-deploy-plugin"))
        .any(|plugin| {
            let configured = |node: Node| {
                truthy(child(node, "configuration").and_then(|config| field(config, "skip")))
            };
            configured(plugin)
                || child(plugin, "executions")
                    .into_iter()
                    .flat_map(|executions| named(executions, "execution"))
                    .filter(|execution| {
                        child(*execution, "goals")
                            .into_iter()
                            .flat_map(|goals| named(goals, "goal"))
                            .filter_map(text_of)
                            .any(|goal| goal == "deploy")
                    })
                    .any(configured)
        })
}
