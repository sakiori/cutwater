//! The files a change touches, compared with a base commit.

use anyhow::Result;

use crate::git::Git;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
}

/// A file, by its path relative to the repository root, that differs from
/// the base commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub status: Status,
}

/// The files that differ between the commits `base` and `head`. A renamed
/// file is a deletion and an addition.
pub fn between(git: &Git, base: &str, head: &str) -> Result<Vec<ChangedFile>> {
    let files = diff(git, &[base, head])?;
    Ok(sorted(files))
}

/// The files of the working tree, untracked ones included, that differ from
/// commit `base`.
pub fn since(git: &Git, base: &str) -> Result<Vec<ChangedFile>> {
    let mut files = diff(git, &[base])?;
    let untracked = git.run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
    files.extend(
        untracked
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| ChangedFile {
                path: path.to_string(),
                status: Status::Added,
            }),
    );
    Ok(sorted(files))
}

fn diff(git: &Git, revisions: &[&str]) -> Result<Vec<ChangedFile>> {
    // Paths relative to the root whatever diff.relative says.
    let mut args = vec![
        "-c",
        "diff.relative=false",
        "diff",
        "--name-status",
        "--no-renames",
        "--no-ext-diff",
        "-z",
    ];
    args.extend_from_slice(revisions);
    args.push("--");
    let output = git.run(&args)?;
    let mut files = Vec::new();
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    while let (Some(code), Some(path)) = (fields.next(), fields.next()) {
        let status = match code {
            "A" => Status::Added,
            "D" => Status::Deleted,
            _ => Status::Modified,
        };
        files.push(ChangedFile {
            path: path.to_string(),
            status,
        });
    }
    Ok(files)
}

/// The files by path, each path once.
fn sorted(mut files: Vec<ChangedFile>) -> Vec<ChangedFile> {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|later, earlier| later.path == earlier.path);
    files
}

/// The merge base of `HEAD` with the default branch `branch`: its upstream
/// when the local branch has one, else the local branch, else the only
/// remote-tracking branch of that name. `None` when there is no such branch
/// or no common history.
pub fn merge_base_with_branch(git: &Git, branch: &str) -> Result<Option<String>> {
    let Some(tip) = branch_tip(git, branch)? else {
        return Ok(None);
    };
    git.merge_base(&tip, "HEAD")
}

fn branch_tip(git: &Git, branch: &str) -> Result<Option<String>> {
    if let Some(upstream) = git.commit(&format!("{branch}@{{upstream}}"))? {
        return Ok(Some(upstream));
    }
    if let Some(local) = git.commit(&format!("refs/heads/{branch}"))? {
        return Ok(Some(local));
    }
    let remote = git.run(&[
        "for-each-ref",
        "--format=%(objectname)",
        &format!("refs/remotes/*/{branch}"),
    ])?;
    let mut tips = remote.lines();
    Ok(match (tips.next(), tips.next()) {
        (Some(tip), None) => Some(tip.to_string()),
        _ => None,
    })
}
