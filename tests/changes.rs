//! The merge base with the default branch, from which `add` preselects units.

mod common;

use common::Repo;
use cutwater::changes::merge_base_with_branch;
use cutwater::git::Git;

#[test]
fn upstream_of_the_default_branch_wins_over_a_stale_local_branch() {
    let origin = Repo::with("", &[("a", "1")]);
    let local = Repo::clone_of(&origin, &[]);
    origin.write("a", "2");
    let advanced = origin.commit("advance main");
    local.git(&["fetch", "--quiet"]);
    local.git(&["checkout", "--quiet", "-b", "feature", "origin/main"]);
    local.write("b", "1");
    local.commit("feature");
    let git = Git::discover(local.path()).unwrap();
    assert_eq!(
        merge_base_with_branch(&git, "main").unwrap(),
        Some(advanced)
    );
}

#[test]
fn remote_tracking_branch_without_a_local_one() {
    let origin = Repo::with("", &[("a", "1")]);
    let main = origin.git(&["rev-parse", "HEAD"]);
    let local = Repo::clone_of(&origin, &[]);
    local.git(&["checkout", "--quiet", "-b", "feature"]);
    local.git(&["branch", "--quiet", "--delete", "main"]);
    local.write("b", "1");
    local.commit("feature");
    let git = Git::discover(local.path()).unwrap();
    assert_eq!(merge_base_with_branch(&git, "main").unwrap(), Some(main));
}

#[test]
fn local_branch_without_upstream_and_no_branch_at_all() {
    let repo = Repo::with("", &[("a", "1")]);
    let main = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "--quiet", "-b", "feature"]);
    repo.write("b", "1");
    repo.commit("feature");
    let git = Git::discover(repo.path()).unwrap();
    assert_eq!(merge_base_with_branch(&git, "main").unwrap(), Some(main));
    assert_eq!(merge_base_with_branch(&git, "trunk").unwrap(), None);
}
