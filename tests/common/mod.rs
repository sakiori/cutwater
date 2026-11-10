//! Temporary git repositories and the tool run in them.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// The GitLab CI variables and tokens the tool reads, which the tests set
/// themselves.
const CI_VARIABLES: [&str; 14] = [
    "CI_MERGE_REQUEST_DIFF_BASE_SHA",
    "CI_MERGE_REQUEST_TARGET_BRANCH_SHA",
    "CI_MERGE_REQUEST_SOURCE_BRANCH_SHA",
    "CI_MERGE_REQUEST_EVENT_TYPE",
    "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME",
    "CI_MERGE_REQUEST_SOURCE_PROJECT_ID",
    "CI_MERGE_REQUEST_PROJECT_ID",
    "CI_PROJECT_ID",
    "GITLAB_CI",
    "CI_COMMIT_BRANCH",
    "CI_SERVER_URL",
    "CI_SERVER_TLS_CA_FILE",
    "CI_JOB_TOKEN",
    "CUTWATER_GITLAB_TOKEN",
];

/// The proxy settings an HTTP client reads, which would send the tests'
/// requests to a local mock server elsewhere.
const PROXY_VARIABLES: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
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
        assert_cmd::Command::from_std(self.cutwater_process(dir))
    }

    /// The tool as a process to spawn, run in `dir` with no CI variables set.
    pub fn cutwater_process(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("cutwater"));
        for variable in CI_VARIABLES.into_iter().chain(PROXY_VARIABLES) {
            cmd.env_remove(variable);
        }
        isolated(cmd, dir)
    }

    /// Runs an ecosystem tool, such as `pnpm` or `cargo`, in the repository,
    /// as the tool runs it, and fails the test when it fails.
    pub fn run(&self, program: &str, args: &[&str]) {
        let out = isolated(Command::new(program), self.path())
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("cannot run {program}, which this test needs: {err}"));
        assert!(
            out.status.success(),
            "{program} {}: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Every file of the working tree but git's own, with its content, to
    /// compare states of the tree.
    pub fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = walk(self.path())
            .into_iter()
            .map(|path| {
                let relative = path.strip_prefix(self.path()).unwrap();
                (
                    relative.to_string_lossy().into_owned(),
                    std::fs::read(&path).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
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
        .env_remove("GIT_INDEX_FILE")
        // npm, pnpm and cargo read no user configuration and stay off the
        // network: the tests' packages depend only on each other.
        .env("npm_config_userconfig", "/dev/null")
        .env("npm_config_offline", "true")
        .env("npm_config_update_notifier", "false")
        .env("CARGO_NET_OFFLINE", "true");
    cmd
}

/// Registries that hold the given versions, answered by `npm` and `cargo`
/// stand-ins put first on the `PATH`: `npm view <name> versions --json`
/// and `cargo info <name>[@<version>]` answer as the real tools do, and
/// every other command goes to the real tool. Each query is logged.
pub struct Registry {
    dir: tempfile::TempDir,
}

impl Registry {
    pub fn new(published: &[(&str, &[&str])]) -> Registry {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        for (name, versions) in published {
            let mut lines = versions.join("\n");
            lines.push('\n');
            std::fs::write(data.join(name.replace('/', "%")), lines).unwrap();
        }
        let data = data.display();
        let log = dir.path().join("queries").display().to_string();
        let real = |tool: &str| which::which(tool).unwrap().display().to_string();
        let npm = format!(
            r#"#!/bin/sh
if [ "$1" = view ]; then
  echo "npm $*" >> '{log}'
  file="{data}/$(printf '%s' "$2" | tr / %)"
  if [ -f "$file" ]; then
    printf '['; sep=''
    while read -r version; do printf '%s"%s"' "$sep" "$version"; sep=','; done < "$file"
    printf ']\n'; exit 0
  fi
  printf '{{"error": {{"code": "E404", "summary": "Not Found"}}}}\n'
  echo 'npm error code E404' >&2; exit 1
fi
exec '{}' "$@"
"#,
            real("npm")
        );
        let cargo = format!(
            r#"#!/bin/sh
if [ "$1" = info ]; then
  echo "cargo $*" >> '{log}'
  spec=$2; name=${{spec%%@*}}; version=
  case "$spec" in *@*) version=${{spec#*@}};; esac
  file="{data}/$name"
  missing() {{ echo "error: could not find \`$spec\` in registry \`https://github.com/rust-lang/crates.io-index\`" >&2; exit 101; }}
  [ -f "$file" ] || missing
  if [ -n "$version" ]; then
    grep -qx "$version" "$file" || missing
  else
    version=$(tail -n 1 "$file")
  fi
  printf '%s\nversion: %s\n' "$name" "$version"; exit 0
fi
exec '{}' "$@"
"#,
            real("cargo")
        );
        for (tool, script) in [("npm", npm), ("cargo", cargo)] {
            let path = bin.join(tool);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        Registry { dir }
    }

    /// The `PATH` with the stand-ins first.
    pub fn path(&self) -> std::ffi::OsString {
        let mut paths = vec![self.dir.path().join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).unwrap()
    }

    /// The queries the stand-ins answered, one per line.
    pub fn queries(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("queries")).unwrap_or_default()
    }
}
