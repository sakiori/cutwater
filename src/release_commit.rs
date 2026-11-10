//! Release commits: the commit on the default branch's first-parent history
//! whose change from its first parent last changed a unit's entry in the
//! version manifest. In a shallow clone the history is deepened, from the
//! default branch on `origin`, only as far as the units' release commits
//! lie.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::git::Git;
use crate::manifest::{MANIFEST_FILE, Manifest};

/// How many commits the first deepening of a shallow clone fetches; each
/// further one fetches twice as many as the one before.
const FIRST_DEEPENING: usize = 50;

/// The release commit of each of `units`, all of which have an entry in the
/// manifest of HEAD, whose first-parent history is that of the default
/// branch `branch`.
pub fn find(git: &Git, branch: &str, units: &[String]) -> Result<BTreeMap<String, String>> {
    let mut deepening = FIRST_DEEPENING;
    loop {
        let walk = walk(git, units)?;
        if walk.unresolved.is_empty() {
            return Ok(walk.found);
        }
        if !walk.reached_boundary {
            bail!(
                "the first-parent history of HEAD never adds the {MANIFEST_FILE} entries of {}",
                walk.unresolved.join(", ")
            );
        }
        let boundary = shallow_commits(git)?;
        git.run(&[
            "fetch",
            "--no-tags",
            "--quiet",
            &format!("--deepen={deepening}"),
            "origin",
            &format!("refs/heads/{branch}"),
        ])
        .with_context(|| {
            format!(
                "this shallow clone does not reach the release commits of {}, and deepening it \
                 from `{branch}` on `origin` failed",
                walk.unresolved.join(", ")
            )
        })?;
        if shallow_commits(git)? == boundary {
            let reason = if git.is_ancestor("HEAD", "FETCH_HEAD")? {
                "deepening it did not reach further back".to_string()
            } else {
                format!(
                    "HEAD is no longer in the history of `{branch}` on `origin`, so deepening \
                     from that branch cannot reach HEAD's history; the branch was rewritten \
                     since this pipeline started"
                )
            };
            bail!(
                "this shallow clone does not reach the release commits of {}, and {reason}",
                walk.unresolved.join(", ")
            );
        }
        deepening = deepening.saturating_mul(2);
    }
}

struct Walk {
    found: BTreeMap<String, String>,
    unresolved: Vec<String>,
    /// Whether the walk stopped at the boundary of a shallow clone.
    reached_boundary: bool,
}

/// Walks the commits of HEAD's first-parent history that change the
/// manifest, newest first, until every unit's release commit is found or
/// the clone's history ends.
fn walk(git: &Git, units: &[String]) -> Result<Walk> {
    let shallow = shallow_commits(git)?;
    // With `--first-parent`, git compares a merge with its first parent
    // only, so a merge that brought the manifest in from its second parent
    // is listed.
    let listing = git.run(&[
        "log",
        "--first-parent",
        "--format=%H %P",
        "HEAD",
        "--",
        MANIFEST_FILE,
    ])?;
    let mut found = BTreeMap::new();
    let mut unresolved: Vec<String> = units.to_vec();
    let mut reached_boundary = false;
    for line in listing.lines() {
        let mut ids = line.split_whitespace();
        let Some(commit) = ids.next() else {
            continue;
        };
        if shallow.iter().any(|boundary| boundary == commit) {
            reached_boundary = true;
            break;
        }
        let after = manifest_at(git, commit)?;
        let before = match ids.next() {
            Some(parent) => manifest_at(git, parent)?,
            None => Manifest::default(),
        };
        unresolved.retain(|unit| {
            let changed = after.get(unit) != before.get(unit);
            if changed {
                found.insert(unit.clone(), commit.to_string());
            }
            !changed
        });
        if unresolved.is_empty() {
            break;
        }
    }
    Ok(Walk {
        found,
        unresolved,
        reached_boundary,
    })
}

fn manifest_at(git: &Git, commit: &str) -> Result<Manifest> {
    match git.file_at(commit, MANIFEST_FILE)? {
        Some(text) => Manifest::parse(&text)
            .with_context(|| format!("{MANIFEST_FILE} in the commit {commit}")),
        None => Ok(Manifest::default()),
    }
}

/// The commits at the boundary of a shallow clone, whose parents the clone
/// lacks.
fn shallow_commits(git: &Git) -> Result<Vec<String>> {
    if !git.is_shallow()? {
        return Ok(Vec::new());
    }
    let path = git.run(&["rev-parse", "--git-path", "shallow"])?;
    let path = git.root().join(path.trim());
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "cannot read the shallow clone's boundary, {}",
            path.display()
        )
    })?;
    Ok(text.lines().map(str::to_string).collect())
}
