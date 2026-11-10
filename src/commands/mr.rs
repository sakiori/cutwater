//! `mr`: the release merge request. It applies the plan of the pending
//! changesets to the default branch's tip, as the forge reports it, in a
//! scratch working tree; writes the result as one commit on top of the tip
//! onto the release branch through the forge; and creates, updates or
//! closes the release merge request. It never merges, approves, assigns or
//! queues a merge request, and leaves a queued one as it is while changesets
//! are pending.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use super::Repository;
use super::status::print_plan;
use super::version;
use crate::changelog;
use crate::cli::MrArgs;
use crate::config::Config;
use crate::forge::gitlab::{Connection, GitLab};
use crate::forge::{FileAction, FileChange, Forge, MergeRequest, MergeSettings, NewCommit};
use crate::git::Git;
use crate::plan::Plan;

/// The variables GitLab CI sets for the branch of a branch pipeline.
const COMMIT_BRANCH_VARIABLE: &str = "CI_COMMIT_BRANCH";
const GITLAB_CI_VARIABLE: &str = "GITLAB_CI";

pub fn run(args: &MrArgs) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let git = Git::discover(&cwd)?;
    let config = Config::load(git.root())?;
    check_pipeline(&config.default_branch)?;
    let forge = GitLab::connect(Connection::from_env(&config.gitlab)?)?;
    release(&git, &config.default_branch, &forge, args.dry_run)
}

/// Refuses a GitLab CI pipeline of anything but the default branch.
fn check_pipeline(default_branch: &str) -> Result<()> {
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if variable(GITLAB_CI_VARIABLE).is_none() {
        return Ok(());
    }
    match variable(COMMIT_BRANCH_VARIABLE) {
        Some(branch) if branch == default_branch => Ok(()),
        Some(branch) => bail!(
            "`mr` runs in pipelines of the default branch `{default_branch}`, but this pipeline \
             is of `{branch}`"
        ),
        None => bail!(
            "`mr` runs in pipelines of the default branch `{default_branch}`, but this pipeline \
             is not a branch pipeline"
        ),
    }
}

/// Brings the release merge request up to date with the default branch's
/// tip; with `dry_run`, only says what it would do.
pub fn release(
    git: &Git,
    default_branch: &str,
    forge: &dyn Forge,
    dry_run: bool,
) -> Result<ExitCode> {
    let settings = forge.merge_settings()?;
    let tip = forge.branch(default_branch)?.with_context(|| {
        format!("the project has no branch `{default_branch}`, the configured default branch")
    })?;
    crate::outln!("The tip of `{default_branch}` is {}.", tip.id)?;
    let scratch = Scratch::checkout(git, default_branch, &tip.id)?;
    let repo = Repository::open_with(scratch.tree.clone())?;
    let planned = repo.plan()?;
    let config = &repo.config;
    let open = forge.open_merge_requests(&config.release_branch, default_branch)?;
    let merge_request = match open.as_slice() {
        [] => None,
        [merge_request] => Some(merge_request),
        several => bail!(
            "several merge requests from `{}` into `{default_branch}` are open: {}; close all \
             but one",
            config.release_branch,
            several
                .iter()
                .map(|merge_request| format!("!{}", merge_request.number))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    if !planned.plan.pending {
        if planned.changesets.is_empty() {
            crate::outln!("No pending changesets: nothing to release.")?;
        } else {
            crate::outln!("The pending changesets release no unit: nothing to release.")?;
        }
        return match merge_request {
            Some(merge_request) => close(forge, merge_request, &tip.id, dry_run),
            None => Ok(ExitCode::SUCCESS),
        };
    }
    print_plan(&planned.plan)?;
    let warning = merge_warning(&settings);
    if let Some(warning) = &warning {
        eprintln!("warning: {warning}");
    }

    let queue = match merge_request {
        Some(merge_request) => queued(forge, &settings, merge_request, &config.gitlab.merge_queue)?
            .map(|reason| (merge_request, reason)),
        None => None,
    };
    if let Some((merge_request, reason)) = queue {
        crate::outln!(
            "The release merge request !{} {reason}, so it is left as it is: changing it would \
             take it out of the queue. The first `mr` after it leaves the queue updates it. {}",
            merge_request.number,
            merge_request.url
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    let changes = version::changes(&repo, &planned)?;
    let targets = scratch.targets(&changes)?;
    version::write(scratch.path(), &changes, "cutwater mr")?;
    let staged = scratch.stage(&targets)?;
    let actions = staged.actions;
    let unchanged = match forge.branch(&config.release_branch)? {
        Some(head) => match head.parents.as_slice() {
            [parent] if *parent == tip.id || !settings.requires_tip() => {
                holds_release(git, &config.release_branch, &head.id, parent, &staged.files)?
            }
            _ => false,
        },
        None => false,
    };
    let title = config.release_title.as_str();
    let description = description(&planned.plan, warning.as_deref());
    let commit_message = commit_message(title, &planned.plan);

    let branch = &config.release_branch;
    if unchanged {
        crate::outln!("The release branch `{branch}` already holds this release.")?;
    } else if dry_run {
        crate::outln!(
            "Would write the release commit onto `{branch}`, on top of {}:",
            tip.id
        )?;
        for action in &actions {
            crate::outln!("  {} {}", action.verb(), action.path)?;
        }
    } else {
        let commit = forge.write_commit(&NewCommit {
            branch,
            parent: &tip.id,
            message: &commit_message,
            actions: &actions,
        })?;
        crate::outln!(
            "Wrote the release commit {commit} onto `{branch}`, on top of {}.",
            tip.id
        )?;
    }

    match merge_request {
        None if dry_run => crate::outln!("Would create the release merge request `{title}`.")?,
        None => {
            let created =
                forge.create_merge_request(branch, default_branch, title, &description)?;
            crate::outln!(
                "Created the release merge request !{}: {}",
                created.number,
                created.url
            )?;
        }
        // GitLab may drop trailing whitespace from a description it stores.
        Some(merge_request)
            if merge_request.title == title
                && merge_request.description.trim_end() == description.trim_end() =>
        {
            crate::outln!(
                "The release merge request !{} is up to date: {}",
                merge_request.number,
                merge_request.url
            )?;
        }
        Some(merge_request) if dry_run => crate::outln!(
            "Would update the title and description of the release merge request !{}: {}",
            merge_request.number,
            merge_request.url
        )?,
        Some(merge_request) => {
            forge.edit_merge_request(merge_request.number, title, &description)?;
            crate::outln!(
                "Updated the release merge request !{}: {}",
                merge_request.number,
                merge_request.url
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Closes the release merge request, as nothing is pending at `tip`.
fn close(
    forge: &dyn Forge,
    merge_request: &MergeRequest,
    tip: &str,
    dry_run: bool,
) -> Result<ExitCode> {
    if dry_run {
        crate::outln!(
            "Would close the release merge request !{}: {}",
            merge_request.number,
            merge_request.url
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    forge.close_merge_request(
        merge_request.number,
        &format!(
            "Closed by cutwater: no changeset is pending at {tip}, so there is nothing to \
             release. The next changeset opens a new release merge request."
        ),
    )?;
    crate::outln!(
        "Closed the release merge request !{}: nothing is pending. {}",
        merge_request.number,
        merge_request.url
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Why the merge request is queued, `None` when it is not: set to merge
/// automatically, assigned to a merge queue account, or in a merge train.
fn queued(
    forge: &dyn Forge,
    settings: &MergeSettings,
    merge_request: &MergeRequest,
    merge_queue: &[String],
) -> Result<Option<String>> {
    if merge_request.auto_merge {
        return Ok(Some("is set to merge automatically".to_string()));
    }
    let account = merge_request.assignees.iter().find(|assignee| {
        merge_queue
            .iter()
            .any(|account| account.eq_ignore_ascii_case(assignee))
    });
    if let Some(account) = account {
        return Ok(Some(format!(
            "is assigned to the merge queue account `{account}`"
        )));
    }
    if settings.merge_trains && forge.in_merge_train(merge_request)? {
        return Ok(Some("is in a merge train".to_string()));
    }
    Ok(None)
}

/// A file's mode and blob after a change, `None` when the change deletes
/// it.
type After = Option<(String, String)>;

/// The files a change touches, each with what it is afterwards.
type ChangedFiles = BTreeMap<String, After>;

/// A file of a change, with its status letter.
#[derive(Debug, PartialEq, Eq)]
struct RawChange {
    path: String,
    status: char,
    after: After,
}

/// The files of `git diff-index` or `git diff-tree` output in the raw
/// format, `-z --no-abbrev`.
fn raw_changes(listing: &str) -> Vec<RawChange> {
    let mut changes = Vec::new();
    let mut fields = listing.split('\0').filter(|field| !field.is_empty());
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, mode, _, blob, status] = parts.as_slice() else {
            continue;
        };
        let status = status.chars().next().unwrap_or('M');
        let after = (status != 'D').then(|| (mode.to_string(), blob.to_string()));
        changes.push(RawChange {
            path: path.to_string(),
            status,
            after,
        });
    }
    changes
}

/// Whether the release branch's commit `head`, whose only parent is
/// `parent`, changes the same files to the same modes and contents as the
/// applied plan, `files`. Fetches the commits the clone lacks; a `head`
/// that `origin`'s branch no longer points at is not the release.
fn holds_release(
    git: &Git,
    branch: &str,
    head: &str,
    parent: &str,
    files: &ChangedFiles,
) -> Result<bool> {
    if git.commit(head)?.is_none() || git.commit(parent)?.is_none() {
        // Since the forge named `head`, the branch may have been deleted or
        // moved past it; the forced write replaces whatever it holds.
        let listed = git
            .run(&["ls-remote", "origin", &format!("refs/heads/{branch}")])
            .with_context(|| format!("cannot look up `{branch}` on `origin`"))?;
        if listed.split_whitespace().next() != Some(head) {
            return Ok(false);
        }
    }
    fetch(git, branch, &[head, parent])?;
    let listing = git.run(&[
        "diff-tree",
        "-r",
        "--no-renames",
        "-z",
        "--no-abbrev",
        parent,
        head,
    ])?;
    let released: ChangedFiles = raw_changes(&listing)
        .into_iter()
        .map(|change| (change.path, change.after))
        .collect();
    Ok(&released == files)
}

/// Makes sure the clone holds `commits`: the tip of `branch` on `origin`
/// followed by its first parents. Fetches the branch, in a shallow clone
/// only as deep as the missing commits reach, then each commit the branch
/// no longer leads to.
fn fetch(git: &Git, branch: &str, commits: &[&str]) -> Result<()> {
    let mut missing = Vec::new();
    for (index, commit) in commits.iter().enumerate() {
        if git.commit(commit)?.is_none() {
            missing.push(index);
        }
    }
    let Some(&deepest) = missing.last() else {
        return Ok(());
    };
    let shallow = git.is_shallow()?;
    let fetch = |what: &str, depth: usize| {
        let depth = format!("--depth={depth}");
        let mut args = vec!["fetch", "--no-tags", "--quiet"];
        if shallow {
            args.push(&depth);
        }
        args.extend(["origin", what]);
        git.run(&args)
            .with_context(|| format!("cannot fetch {what} from `origin`"))
    };
    fetch(&format!("refs/heads/{branch}"), deepest + 1)?;
    for commit in commits {
        if git.commit(commit)?.is_none() {
            fetch(commit, 1)?;
        }
    }
    Ok(())
}

/// The warning for merge settings under which a merge request pipeline does
/// not test the tree that lands.
fn merge_warning(settings: &MergeSettings) -> Option<String> {
    (!settings.tests_landing_tree()).then(|| {
        "the project merges with merge commits and runs neither merged results pipelines nor \
         merge trains, so the release merge request's pipeline does not check the tree that \
         lands. Turn on fast-forward merges, semi-linear history, merged results pipelines or \
         merge trains."
            .to_string()
    })
}

/// The release merge request's description: every released unit's next
/// version and changelog entry, in the order of publication.
fn description(plan: &Plan, warning: Option<&str>) -> String {
    let mut text = String::from(
        "This merge request releases the pending changesets. Merging it is the release \
         decision: the units below are released from the commit that lands. cutwater keeps it \
         up to date with the default branch and never merges it.\n",
    );
    if let Some(warning) = warning {
        text.push_str(&format!("\n> **Warning:** {warning}\n"));
    }
    for release in &plan.releases {
        let current = match &release.current {
            Some(current) => current.to_string(),
            None => "new".to_string(),
        };
        let dependencies: Vec<(String, String)> = release
            .dependencies
            .iter()
            .map(|(name, version)| (name.clone(), version.to_string()))
            .collect();
        text.push_str(&format!(
            "\n## {} {current} → {}\n\n{}\n",
            release.unit,
            release.next,
            changelog::body(release.dependency_bump, &release.entries, &dependencies)
        ));
    }
    text
}

/// The release commit's message: the title, then each release.
fn commit_message(title: &str, plan: &Plan) -> String {
    let mut message = format!("{title}\n");
    for release in &plan.releases {
        message.push_str(&format!("\n{} {}", release.unit, release.next));
    }
    message.push('\n');
    message
}

/// A linked working tree of a commit in a temporary directory, removed
/// again when dropped.
struct Scratch {
    tree: Git,
    path: PathBuf,
    _dir: ScratchDir,
}

/// The file of the working tree at `tree`, canonically `root`, that a
/// write to `path` reaches through symbolic links, relative to the tree.
fn target(root: &Path, tree: &Path, path: &str) -> Result<String> {
    let full = tree.join(path);
    let link = std::fs::symlink_metadata(&full).is_ok_and(|meta| meta.file_type().is_symlink());
    let link_target = || {
        std::fs::read_link(&full)
            .map(|target| target.display().to_string())
            .unwrap_or_default()
    };
    // The deepest existing directory or file, and the part below it.
    let mut existing = full.as_path();
    let mut below = Vec::new();
    while !existing.exists() {
        if link && existing == full {
            bail!(
                "{path} is a symbolic link to {}, which does not resolve, so the release commit \
                 cannot carry what `mr` writes to it; make it a file or point it at one in the \
                 repository",
                link_target()
            );
        }
        below.push(
            existing
                .file_name()
                .context("a path below the working tree")?,
        );
        existing = existing.parent().context("a path below the working tree")?;
    }
    let resolved = std::fs::canonicalize(existing)
        .with_context(|| format!("cannot resolve {path} in the scratch working tree"))?;
    let Ok(inside) = resolved.strip_prefix(root) else {
        let points = if link {
            format!("is a symbolic link to {}", link_target())
        } else {
            format!("resolves to {}", resolved.display())
        };
        bail!(
            "{path} {points}, outside the repository, so the release commit cannot carry what \
             `mr` writes to it; make it a file or point it at one in the repository"
        );
    };
    let mut parts: Vec<String> = inside
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.extend(
        below
            .iter()
            .rev()
            .map(|part| part.to_string_lossy().into_owned()),
    );
    Ok(parts.join("/"))
}

/// The temporary directory of a scratch working tree. Dropping it removes
/// the working tree and the clone's administrative entry of it, as `git
/// worktree remove`, which needs git 2.17, does.
struct ScratchDir {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let admin = own_admin_entry(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
        if let Some(admin) = admin {
            let _ = std::fs::remove_dir_all(admin);
        }
    }
}

/// The clone's administrative entry of the linked working tree at `tree`:
/// the directory its `.git` file names, provided that the entry's `gitdir`
/// file names that `.git` file in turn.
fn own_admin_entry(tree: &Path) -> Option<PathBuf> {
    let dot_git = tree.join(".git");
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let admin = tree.join(text.strip_prefix("gitdir:")?.trim());
    let back = std::fs::read_to_string(admin.join("gitdir")).ok()?;
    let back = admin.join(back.trim());
    let same = std::fs::canonicalize(&back).ok()? == std::fs::canonicalize(&dot_git).ok()?;
    same.then_some(admin)
}

/// The applied plan, staged in the scratch working tree.
struct Staged {
    actions: Vec<FileAction>,
    files: ChangedFiles,
}

impl Scratch {
    /// Checks out `commit`, the tip of `branch` on `origin`, fetching it
    /// when the clone lacks it.
    fn checkout(git: &Git, branch: &str, commit: &str) -> Result<Scratch> {
        fetch(git, branch, &[commit])?;
        let dir = tempfile::Builder::new()
            .prefix("cutwater-mr-")
            .tempdir()
            .context("cannot create a scratch directory")?;
        let path = dir.path().join("tree");
        let scratch_dir = ScratchDir {
            path: path.clone(),
            _dir: dir,
        };
        git.add_worktree(&path, commit)?;
        Ok(Scratch {
            tree: Git::discover_made(&path)?,
            path,
            _dir: scratch_dir,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The files the changes write or delete, as git tracks them: a file
    /// written through a symbolic link is the link's target. Refuses a
    /// target outside the working tree, before anything is written there.
    fn targets(&self, changes: &version::Changes) -> Result<Vec<String>> {
        let root =
            std::fs::canonicalize(&self.path).context("cannot resolve the scratch working tree")?;
        let written = changes
            .writes
            .keys()
            .chain(changes.lockfiles.iter().map(|lockfile| &lockfile.path));
        let mut targets = Vec::new();
        for path in written {
            targets.push(target(&root, &self.path, path)?);
        }
        targets.extend(changes.deletions.iter().cloned());
        Ok(targets)
    }

    /// Stages the files of the applied plan and returns their changes as
    /// file actions, with the content git would commit, and as changed
    /// files.
    fn stage(&self, paths: &[String]) -> Result<Staged> {
        let mut add = vec!["add", "--all", "--"];
        add.extend(paths.iter().map(String::as_str));
        self.tree.run(&add)?;
        let listing = self.tree.run(&[
            "diff-index",
            "--cached",
            "-r",
            "--no-renames",
            "-z",
            "--no-abbrev",
            "HEAD",
        ])?;
        let mut staged = Staged {
            actions: Vec::new(),
            files: ChangedFiles::new(),
        };
        for RawChange {
            path,
            status,
            after,
        } in raw_changes(&listing)
        {
            let change = match (&after, status) {
                (None, _) => FileChange::Delete,
                (Some((_, blob)), status) => {
                    let content = self.tree.bytes(&["cat-file", "blob", blob])?;
                    if status == 'A' {
                        FileChange::Create(content)
                    } else {
                        FileChange::Update(content)
                    }
                }
            };
            staged.actions.push(FileAction {
                path: path.clone(),
                change,
            });
            staged.files.insert(path, after);
        }
        Ok(staged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::MergeMethod;

    fn settings(method: MergeMethod, merged_results: bool, merge_trains: bool) -> MergeSettings {
        MergeSettings {
            method,
            merged_results,
            merge_trains,
        }
    }

    #[test]
    fn merge_warning_only_where_the_landing_tree_goes_untested() {
        use MergeMethod::*;
        let cases = [
            (settings(Merge, false, false), true),
            (settings(Merge, true, false), false),
            (settings(Merge, false, true), false),
            (settings(SemiLinear, false, false), false),
            (settings(FastForward, false, false), false),
        ];
        for (settings, warns) in cases {
            assert_eq!(merge_warning(&settings).is_some(), warns, "{settings:?}");
        }
    }

    #[test]
    fn own_admin_entry_only_where_it_names_the_tree_back() {
        let root = tempfile::tempdir().unwrap();
        let tree = root.path().join("tree");
        let admin = root.path().join("clone/.git/worktrees/tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::create_dir_all(&admin).unwrap();
        assert_eq!(own_admin_entry(&tree), None);
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(admin.join("gitdir"), "/elsewhere/.git\n").unwrap();
        assert_eq!(own_admin_entry(&tree), None);
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", tree.join(".git").display()),
        )
        .unwrap();
        assert_eq!(own_admin_entry(&tree), Some(admin));
    }

    #[test]
    fn raw_changes_keep_mode_and_blob_and_mark_deletions() {
        let zero = "0".repeat(40);
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let listing = format!(
            ":100644 100644 {a} {b} M\0package.json\0\
             :000000 100755 {zero} {b} A\0bin/run\0\
             :100644 000000 {a} {zero} D\0.changeset/x.md\0"
        );
        let change = |path: &str, status, after: Option<(&str, &str)>| RawChange {
            path: path.to_string(),
            status,
            after: after.map(|(mode, blob)| (mode.to_string(), blob.to_string())),
        };
        assert_eq!(
            raw_changes(&listing),
            [
                change("package.json", 'M', Some(("100644", &b))),
                change("bin/run", 'A', Some(("100755", &b))),
                change(".changeset/x.md", 'D', None),
            ]
        );
    }
}
