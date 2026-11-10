//! The commit a merge request pipeline compares HEAD with: the target
//! branch commit of a merged result, or the merge base of the merge
//! request's diff base.

use anyhow::{Result, bail};

use crate::git::Git;

/// The merge request's diff base.
const DIFF_BASE_VARIABLE: &str = "CI_MERGE_REQUEST_DIFF_BASE_SHA";
/// The target branch commit a merged results or merge train pipeline merged
/// the merge request onto; set only in those pipelines.
const TARGET_VARIABLE: &str = "CI_MERGE_REQUEST_TARGET_BRANCH_SHA";

/// The commit HEAD is compared with.
pub struct Base {
    pub commit: String,
    /// Where the commit comes from, for the output.
    pub origin: String,
}

impl Base {
    /// `--base`'s merge base with HEAD; in a merged results or merge train
    /// pipeline, the target branch commit the pipeline merged onto; else the
    /// merge base of the merge request's diff base with HEAD.
    pub fn resolve(git: &Git, flag: Option<&str>) -> Result<Base> {
        let variable = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        if let Some(rev) = flag {
            return merge_base(git, rev, "--base");
        }
        if let Some(target) = variable(TARGET_VARIABLE) {
            // HEAD is a commit that GitLab made on top of this one; a
            // fast-forward merge train may have rebased several commits onto
            // it, so it need not be HEAD's first parent.
            let Some(commit) = git.commit(&target)? else {
                bail!(missing(git, &target, TARGET_VARIABLE)?);
            };
            return Ok(Base {
                commit,
                origin: format!(
                    "the target branch commit of this merged result, {TARGET_VARIABLE}"
                ),
            });
        }
        if let Some(sha) = variable(DIFF_BASE_VARIABLE) {
            return merge_base(git, &sha, DIFF_BASE_VARIABLE);
        }
        bail!(
            "no base to compare with: run this in a merge request pipeline, which sets \
             {DIFF_BASE_VARIABLE}, or pass `--base <commit>`"
        )
    }
}

fn merge_base(git: &Git, rev: &str, source: &str) -> Result<Base> {
    let Some(commit) = git.commit(rev)? else {
        // Only a full commit name can be missing from a shallow clone; any
        // other revision is one the repository does not have.
        let full_name = matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit());
        if !full_name {
            bail!(
                "`{rev}` ({source}) names no commit of this repository; pass a commit, branch or \
                 tag"
            );
        }
        bail!(missing(git, rev, source)?);
    };
    let Some(base) = git.merge_base(&commit, "HEAD")? else {
        bail!(
            "the history in this clone does not connect `{rev}` ({source}) with HEAD.{} Raise \
             the clone depth, GIT_DEPTH in GitLab CI, so that the clone reaches their merge base.",
            shallow_note(git)?
        );
    };
    Ok(Base {
        commit: base,
        origin: format!("the merge base of {source} with HEAD"),
    })
}

fn missing(git: &Git, rev: &str, source: &str) -> Result<String> {
    Ok(format!(
        "this clone lacks the commit `{rev}` ({source}).{} Raise the clone depth, GIT_DEPTH in \
         GitLab CI, so that the clone reaches it, or fetch that commit.",
        shallow_note(git)?
    ))
}

fn shallow_note(git: &Git) -> Result<&'static str> {
    Ok(if git.is_shallow()? {
        " This is a shallow clone."
    } else {
        ""
    })
}
