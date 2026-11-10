//! Release commits on the default branch's first-parent history, with
//! merge commits and in shallow clones.

mod common;

use common::Repo;
use cutwater::git::Git;
use cutwater::release_commit;

fn manifest(entries: &[(&str, &str)]) -> String {
    let mut text = String::from("[units]\n");
    for (unit, version) in entries {
        text.push_str(&format!("{unit} = {version}\n"));
    }
    text
}

fn find(repo: &Repo, units: &[&str]) -> Vec<(String, String)> {
    let git = Git::discover(repo.path()).unwrap();
    let units: Vec<String> = units.iter().map(ToString::to_string).collect();
    release_commit::find(&git, "main", &units)
        .unwrap()
        .into_iter()
        .collect()
}

fn pair(unit: &str, commit: &str) -> (String, String) {
    (unit.to_string(), commit.to_string())
}

#[test]
fn merge_commit_not_the_release_branch_commit() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[(
            ".cutwater/versions.toml",
            &manifest(&[
                ("api", "\"1.4.2\""),
                ("core", "{ version = \"1.0.0\", seeded = true }"),
            ]),
        )],
    );
    let adoption = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "--quiet", "-b", "cutwater/release"]);
    repo.write(
        ".cutwater/versions.toml",
        &manifest(&[
            ("api", "\"1.4.3\""),
            ("core", "{ version = \"1.0.0\", seeded = true }"),
        ]),
    );
    let bot = repo.commit("Release");
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write("README.md", "later work\n");
    repo.commit("work landed before the release");
    repo.git(&[
        "merge",
        "--quiet",
        "--no-ff",
        "--no-edit",
        "cutwater/release",
    ]);
    let merge = repo.git(&["rev-parse", "HEAD"]);
    assert_ne!(merge, bot);
    // A later merge of a branch that did not touch the manifest, from a base
    // before the release.
    repo.git(&["checkout", "--quiet", "-b", "feature", &adoption]);
    repo.write("src.txt", "feature\n");
    repo.commit("feature");
    repo.git(&["checkout", "--quiet", "main"]);
    repo.git(&["merge", "--quiet", "--no-ff", "--no-edit", "feature"]);

    assert_eq!(
        find(&repo, &["api", "core"]),
        [pair("api", &merge), pair("core", &adoption)]
    );
}

#[test]
fn fast_forward_release_and_later_releases_of_other_units() {
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[(
            ".cutwater/versions.toml",
            &manifest(&[("api", "\"1.4.2\""), ("core", "\"2.0.0\"")]),
        )],
    );
    repo.write(
        ".cutwater/versions.toml",
        &manifest(&[("api", "\"1.4.3\""), ("core", "\"2.0.0\"")]),
    );
    let api = repo.commit("Release api");
    repo.write(
        ".cutwater/versions.toml",
        &manifest(&[("api", "\"1.4.3\""), ("core", "\"2.1.0\"")]),
    );
    let core = repo.commit("Release core");
    repo.write("README.md", "after\n");
    repo.commit("after");

    assert_eq!(
        find(&repo, &["api", "core"]),
        [pair("api", &api), pair("core", &core)]
    );
}

#[test]
fn shallow_clone_is_deepened_to_the_release_commit() {
    let origin = Repo::with(
        "[npm]\ndiscover = true\n",
        &[(
            ".cutwater/versions.toml",
            &manifest(&[("api", "\"1.0.0\"")]),
        )],
    );
    for _ in 0..120 {
        origin.git(&["commit", "--quiet", "--allow-empty", "--message", "older"]);
    }
    origin.write(
        ".cutwater/versions.toml",
        &manifest(&[("api", "\"1.1.0\"")]),
    );
    let release = origin.commit("Release");
    for _ in 0..60 {
        origin.git(&["commit", "--quiet", "--allow-empty", "--message", "later"]);
    }
    let clone = Repo::clone_of(&origin, &["--depth=3", "--branch=main"]);
    assert_eq!(clone.git(&["rev-parse", "--is-shallow-repository"]), "true");

    assert_eq!(find(&clone, &["api"]), [pair("api", &release)]);
    // Deepened only as far as needed: the older history stays out of reach.
    assert_eq!(clone.git(&["rev-parse", "--is-shallow-repository"]), "true");
}

#[test]
fn rewritten_branch_stops_the_deepening() {
    let origin = Repo::with(
        "[npm]\ndiscover = true\n",
        &[(
            ".cutwater/versions.toml",
            &manifest(&[("api", "\"1.0.0\"")]),
        )],
    );
    for _ in 0..5 {
        origin.git(&["commit", "--quiet", "--allow-empty", "--message", "later"]);
    }
    let clone = Repo::clone_of(&origin, &["--depth=2", "--branch=main"]);
    origin.git(&["checkout", "--quiet", "--orphan", "rewritten"]);
    origin.commit("rewritten history");
    origin.git(&["branch", "--quiet", "--force", "main", "rewritten"]);

    let git = Git::discover(clone.path()).unwrap();
    let message = format!(
        "{:#}",
        release_commit::find(&git, "main", &["api".to_string()]).unwrap_err()
    );
    assert!(
        message.contains("HEAD is no longer in the history of `main` on `origin`"),
        "{message}"
    );
}
