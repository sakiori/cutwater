//! Temporary git repositories and the tool run in them.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// The GitLab CI variables the tool reads, which the tests set themselves.
const CI_VARIABLES: [&str; 8] = [
    "CI_MERGE_REQUEST_DIFF_BASE_SHA",
    "CI_MERGE_REQUEST_TARGET_BRANCH_SHA",
    "CI_MERGE_REQUEST_SOURCE_BRANCH_SHA",
    "CI_MERGE_REQUEST_EVENT_TYPE",
    "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME",
    "CI_MERGE_REQUEST_SOURCE_PROJECT_ID",
    "CI_MERGE_REQUEST_PROJECT_ID",
    "CI_PROJECT_ID",
];

/// A git repository in a temporary directory, on the branch `main`, with
/// git's user and system configuration kept out.
pub struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    pub fn new() -> Repo {
        let repo = Repo {
            dir: tempfile::tempdir().unwrap(),
        };
        repo.git(&["init", "--quiet"]);
        repo.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        repo
    }

    /// A repository with `config` as `.cutwater/config.toml` and the given
    /// files, committed on `main`.
    pub fn with(config: &str, files: &[(&str, &str)]) -> Repo {
        let repo = Repo::new();
        repo.write(".cutwater/config.toml", config);
        for (path, content) in files {
            repo.write(path, content);
        }
        repo.commit("initial");
        repo
    }

    /// A clone of `origin`, made with the extra `git clone` arguments.
    pub fn clone_of(origin: &Repo, args: &[&str]) -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("file://{}", origin.path().display());
        let mut clone = vec!["clone", "--quiet"];
        clone.extend_from_slice(args);
        clone.extend([url.as_str(), dir.path().to_str().unwrap()]);
        origin.git(&clone);
        Repo { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, path: &str, content: &str) {
        let path = self.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.path().join(path)).unwrap()
    }

    pub fn remove(&self, path: &str) {
        std::fs::remove_file(self.path().join(path)).unwrap();
    }

    /// The files directly in `dir`, by name.
    pub fn list(&self, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.path().join(dir))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = isolated(Command::new("git"), self.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Commits every change and returns the commit's name.
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--allow-empty", "--message", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// The tool, run in the repository with no CI variables set.
    pub fn cutwater(&self) -> assert_cmd::Command {
        self.cutwater_in(self.path())
    }

    pub fn cutwater_in(&self, dir: &Path) -> assert_cmd::Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("cutwater"));
        for variable in CI_VARIABLES {
            cmd.env_remove(variable);
        }
        assert_cmd::Command::from_std(isolated(cmd, dir))
    }
}

fn isolated(mut cmd: Command, dir: &Path) -> Command {
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}
