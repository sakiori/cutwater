//! `version`: apply the plan to the working tree. It writes the released
//! versions into the manifest and through the adapters, rewrites the
//! references whose ranges no longer accept them, prepends the changelogs,
//! records prereleases, deletes the applied changesets and refreshes the
//! lockfiles.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use super::status::{print_plan, print_prerelease};
use super::{Planned, Repository};
use crate::adapter::{
    self, AdapterKind, Lockfile, LockfileRequest, ReferenceWrite, VersionWrite, WriteRequest,
};
use crate::changelog;
use crate::cli::VersionArgs;
use crate::manifest::MANIFEST_FILE;
use crate::prerelease::PRE_FILE;

pub fn run(args: &VersionArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let planned = repo.plan()?;
    if !planned.plan.pending {
        if planned.changesets.is_empty() {
            crate::outln!("No pending changesets: nothing to release.")?;
        } else {
            crate::outln!(
                "The pending changesets release no unit: nothing to release. They are applied \
                 with the next release."
            )?;
        }
        if !planned.plan.removed.is_empty() {
            crate::outln!(
                "Units no longer in the repository, whose manifest entries the next release \
                 removes: {}",
                planned.plan.removed.join(", ")
            )?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    let changes = changes(&repo, &planned)?;
    print_prerelease(planned.pre.as_ref())?;
    print_plan(&planned.plan)?;
    if args.dry_run {
        print_files(
            "Would write",
            &changes.writes.keys().cloned().collect::<Vec<_>>(),
        )?;
        print_files("Would delete", &changes.deletions)?;
        print_lockfiles("Would refresh", &changes.lockfiles)?;
        return Ok(ExitCode::SUCCESS);
    }
    write(repo.git.root(), &changes, "cutwater version")?;
    print_files("Wrote", &changes.writes.keys().cloned().collect::<Vec<_>>())?;
    print_files("Deleted", &changes.deletions)?;
    print_lockfiles("Refreshed", &changes.lockfiles)?;
    Ok(ExitCode::SUCCESS)
}

/// Applies the changes to the working tree at `root` for `command`. When a
/// step fails, it puts every file back.
pub fn write(root: &Path, changes: &Changes, command: &str) -> Result<()> {
    for lockfile in &changes.lockfiles {
        let tool = &lockfile.command[0];
        if which::which(tool).is_err() {
            bail!(
                "{}: refreshing it needs {tool}, which is not on the PATH; install {tool} and \
                 run `{command}` again",
                lockfile.path
            );
        }
    }
    let mut journal = Journal::default();
    if let Err(err) = apply(root, changes, &mut journal) {
        return Err(match journal.restore(root) {
            Ok(()) => err.context(format!("the working tree is as it was before `{command}`")),
            Err(restore) => err.context(format!(
                "restoring the working tree failed too, so it is partly changed: {restore:#}"
            )),
        });
    }
    Ok(())
}

/// Writes the files, refreshes the lockfiles and deletes the applied files
/// last, recording each file's content first.
fn apply(root: &Path, changes: &Changes, journal: &mut Journal) -> Result<()> {
    for (file, content) in &changes.writes {
        journal.keep(root, file)?;
        journal.create_dirs(root, file)?;
        std::fs::write(root.join(file), content).with_context(|| format!("cannot write {file}"))?;
    }
    for lockfile in &changes.lockfiles {
        journal.keep(root, &lockfile.path)?;
        adapter::refresh(root, lockfile)?;
    }
    for file in &changes.deletions {
        journal.keep(root, file)?;
        std::fs::remove_file(root.join(file)).with_context(|| format!("cannot delete {file}"))?;
    }
    Ok(())
}

/// The content files had before `version` changed them, and the
/// directories it created, to restore them when a later step fails.
#[derive(Default)]
struct Journal {
    /// Each file's content, `None` for a file that did not exist.
    originals: Vec<(String, Option<Vec<u8>>)>,
    /// The directories created, in the order of creation.
    created: Vec<std::path::PathBuf>,
}

impl Journal {
    fn keep(&mut self, root: &Path, file: &str) -> Result<()> {
        if self.originals.iter().any(|(kept, _)| kept == file) {
            return Ok(());
        }
        let original = match std::fs::read(root.join(file)) {
            Ok(content) => Some(content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err).with_context(|| format!("cannot read {file}")),
        };
        self.originals.push((file.to_string(), original));
        Ok(())
    }

    /// Creates the missing directories above `file`, recording them.
    fn create_dirs(&mut self, root: &Path, file: &str) -> Result<()> {
        let path = root.join(file);
        let mut missing: Vec<&Path> = path
            .ancestors()
            .skip(1)
            .take_while(|dir| !dir.exists())
            .collect();
        missing.reverse();
        for dir in missing {
            std::fs::create_dir(dir)
                .with_context(|| format!("cannot create the directory of {file}"))?;
            self.created.push(dir.to_path_buf());
        }
        Ok(())
    }

    /// Puts every recorded file back, newest change first, and removes the
    /// directories it created.
    fn restore(&self, root: &Path) -> Result<()> {
        let mut failures = Vec::new();
        for (file, original) in self.originals.iter().rev() {
            let path = root.join(file);
            let restored = match original {
                Some(content) => std::fs::write(&path, content),
                None => match std::fs::remove_file(&path) {
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    other => other,
                },
            };
            if let Err(err) = restored {
                failures.push(format!("{file}: {err}"));
            }
        }
        for dir in self.created.iter().rev() {
            if let Err(err) = std::fs::remove_dir(dir) {
                failures.push(format!("{}: {err}", dir.display()));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("{}", failures.join("; "))
        }
    }
}

/// What applying the plan changes in the working tree.
pub struct Changes {
    /// The new content of files, by path relative to the repository root.
    pub writes: BTreeMap<String, String>,
    /// The files to delete.
    pub deletions: Vec<String>,
    /// The lockfiles to refresh after the writes.
    pub lockfiles: Vec<Lockfile>,
}

/// The changes that apply the plan, computed without writing anything.
pub fn changes(repo: &Repository, planned: &Planned) -> Result<Changes> {
    let root = repo.git.root();
    let plan = &planned.plan;
    let mut writes = BTreeMap::new();
    let mut lockfiles = Vec::new();
    for kind in AdapterKind::ALL {
        let units = repo.unit_refs(kind);
        let versions: Vec<VersionWrite> = plan
            .releases
            .iter()
            .filter(|release| release.adapter == kind)
            .map(|release| VersionWrite {
                unit: release.unit.clone(),
                version: release.next.to_string(),
            })
            .collect();
        let references: Vec<ReferenceWrite> = plan
            .references
            .iter()
            .filter(|update| units.iter().any(|unit| unit.name == update.edge.dependency))
            .map(|update| ReferenceWrite {
                edge: update.edge.clone(),
                version: update.version.to_string(),
            })
            .collect();
        if versions.is_empty() && references.is_empty() {
            continue;
        }
        let adapter = kind.adapter();
        let written = adapter.write(&WriteRequest {
            root: root.to_path_buf(),
            units: units.clone(),
            versions,
            references,
        })?;
        if written.files.is_empty() {
            continue;
        }
        for file in written.files {
            writes.insert(file.path, file.content);
        }
        lockfiles.extend(
            adapter
                .lockfiles(&LockfileRequest {
                    root: root.to_path_buf(),
                    units,
                })?
                .lockfiles,
        );
    }

    for release in &plan.releases {
        let unit = repo
            .units
            .get(&release.unit)
            .expect("a released unit exists");
        let file = if unit.path == "." {
            "CHANGELOG.md".to_string()
        } else {
            format!("{}/CHANGELOG.md", unit.path)
        };
        let existing = match writes.get(&file) {
            Some(content) => Some(content.clone()),
            None => read_if_present(root, &file)?,
        };
        let dependencies: Vec<(String, String)> = release
            .dependencies
            .iter()
            .map(|(name, version)| (name.clone(), version.to_string()))
            .collect();
        let section = changelog::section(
            &release.next.to_string(),
            release.dependency_bump,
            &release.entries,
            &dependencies,
        );
        writes.insert(
            file,
            changelog::prepend(existing.as_deref(), &release.unit, &section),
        );
    }

    let mut deletions: Vec<String> = planned
        .changesets
        .iter()
        .map(|changeset| changeset.file.clone())
        .collect();
    let manifest = plan.manifest.render();
    if read_if_present(root, MANIFEST_FILE)?.as_deref() != Some(manifest.as_str()) {
        writes.insert(MANIFEST_FILE.to_string(), manifest);
    }
    match &plan.pre {
        Some(state) => {
            let text = state.render();
            if read_if_present(root, PRE_FILE)?.as_deref() != Some(text.as_str()) {
                writes.insert(PRE_FILE.to_string(), text);
            }
        }
        None if root.join(PRE_FILE).exists() => deletions.push(PRE_FILE.to_string()),
        None => {}
    }
    deletions.sort();
    Ok(Changes {
        writes,
        deletions,
        lockfiles,
    })
}

fn read_if_present(root: &Path, file: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(root.join(file)) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot read {file}")),
    }
}

fn print_files(heading: &str, files: &[String]) -> std::io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    crate::outln!("{heading}:")?;
    for file in files {
        crate::outln!("  {file}")?;
    }
    Ok(())
}

fn print_lockfiles(heading: &str, lockfiles: &[Lockfile]) -> std::io::Result<()> {
    if lockfiles.is_empty() {
        return Ok(());
    }
    crate::outln!("{heading}:")?;
    for lockfile in lockfiles {
        crate::outln!("  {} (`{}`)", lockfile.path, lockfile.command.join(" "))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_puts_files_back_and_removes_created_directories() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::write(root.join("kept.txt"), "before").unwrap();
        std::fs::write(root.join("gone.txt"), "deleted").unwrap();
        let mut journal = Journal::default();
        for file in ["kept.txt", "new/deep/file.txt", "gone.txt"] {
            journal.keep(root, file).unwrap();
        }
        journal.create_dirs(root, "new/deep/file.txt").unwrap();
        std::fs::write(root.join("kept.txt"), "after").unwrap();
        std::fs::write(root.join("new/deep/file.txt"), "created").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();

        journal.restore(root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("kept.txt")).unwrap(),
            "before"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("gone.txt")).unwrap(),
            "deleted"
        );
        assert!(!root.join("new").exists());
    }
}
