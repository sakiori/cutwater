//! The only place that runs `git`: the installed git, as a subprocess, whose
//! failures become [`GitError`]s naming the command and carrying git's message.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// A failed git invocation.
#[derive(Debug, Clone, thiserror::Error)]
#[error("`git {}` failed ({}){}", .args.join(" "), exit_status(*.code), stderr_suffix(.stderr))]
pub struct GitError {
    pub args: Vec<String>,
    pub code: Option<i32>,
    pub stderr: String,
}

fn exit_status(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit status {code}"),
        None => "terminated by a signal".to_string(),
    }
}

fn stderr_suffix(stderr: &str) -> String {
    match stderr.trim() {
        "" => String::new(),
        message => format!(": {message}"),
    }
}

/// The captured result of a git invocation, successful or not.
#[derive(Debug, Clone)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    args: Vec<String>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Returns stdout on success, the error otherwise.
    pub fn checked(self) -> Result<String, GitError> {
        if self.success() {
            Ok(self.stdout)
        } else {
            Err(GitError {
                args: self.args,
                code: self.code,
                stderr: self.stderr,
            })
        }
    }
}

/// The variables that point git at a repository, an index or a working
/// tree, as `git rev-parse --local-env-vars` lists them, without the
/// configuration ones; a git hook, for one, runs with `GIT_DIR` set. The
/// installed git names them, because the set differs between versions.
fn location_variables() -> &'static [String] {
    static VARIABLES: OnceLock<Vec<String>> = OnceLock::new();
    VARIABLES.get_or_init(|| {
        Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|variable| !variable.starts_with("GIT_CONFIG"))
                    .map(str::to_string)
                    .collect()
            })
            // Without git, the git commands themselves fail and say so.
            .unwrap_or_default()
    })
}

/// A git handle bound to the top-level directory of a working tree.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
    /// Whether the working tree is one the tool made, whose git calls must
    /// not follow the caller's [`location_variables`] to the caller's
    /// repository.
    made: bool,
}

impl Git {
    /// The handle of the working tree that contains `dir`, found as git
    /// itself finds it, with the caller's environment.
    pub fn discover(dir: &Path) -> Result<Self> {
        Self::find(dir, false)
    }

    /// The handle of a working tree the tool made at `dir`, such as a
    /// scratch working tree, whatever repository the caller's environment
    /// names.
    pub fn discover_made(dir: &Path) -> Result<Self> {
        Self::find(dir, true)
    }

    fn find(dir: &Path, made: bool) -> Result<Self> {
        let probe = Self {
            dir: dir.to_path_buf(),
            made,
        };
        let out = probe.raw(&["rev-parse", "--show-toplevel"])?;
        if !out.success() {
            anyhow::bail!(
                "{} is not inside a git working tree: {}",
                dir.display(),
                out.stderr.trim()
            );
        }
        Ok(Self {
            dir: PathBuf::from(out.stdout.trim_end_matches('\n')),
            made,
        })
    }

    /// The top-level directory of the working tree.
    pub fn root(&self) -> &Path {
        &self.dir
    }

    fn command<S: AsRef<str>>(&self, args: &[S]) -> Command {
        let mut cmd = Command::new("git");
        if self.made {
            for variable in location_variables() {
                cmd.env_remove(variable);
            }
        }
        cmd.arg("-C").arg(&self.dir);
        cmd.args(args.iter().map(AsRef::as_ref));
        cmd.env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            // Reading commands must not take the index lock a concurrent
            // git in the same working tree may need.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null());
        cmd
    }

    /// Runs git with the extra environment `env` and captures what it
    /// returns, with the arguments as owned strings.
    fn capture<S: AsRef<str>>(
        &self,
        args: &[S],
        env: &[(&str, &str)],
    ) -> Result<(Vec<String>, std::process::Output)> {
        let owned: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
        let out = self
            .command(args)
            .envs(env.iter().copied())
            .output()
            .with_context(|| format!("cannot run `git {}`; is git installed?", owned.join(" ")))?;
        Ok((owned, out))
    }

    /// Runs git and captures the result whatever the exit status.
    pub fn raw<S: AsRef<str>>(&self, args: &[S]) -> Result<Output> {
        let (args, out) = self.capture(args, &[])?;
        Ok(Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            args,
        })
    }

    /// Runs git and returns stdout, failing on a non-zero exit status.
    pub fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<String> {
        Ok(self.raw(args)?.checked()?)
    }

    /// Runs git with the extra environment `env` and returns stdout as
    /// bytes, failing on a non-zero exit status.
    fn checked_bytes<S: AsRef<str>>(&self, args: &[S], env: &[(&str, &str)]) -> Result<Vec<u8>> {
        let (args, out) = self.capture(args, env)?;
        if !out.status.success() {
            return Err(GitError {
                args,
                code: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }
            .into());
        }
        Ok(out.stdout)
    }

    /// Runs git and returns stdout as bytes, failing on a non-zero exit
    /// status.
    pub fn bytes<S: AsRef<str>>(&self, args: &[S]) -> Result<Vec<u8>> {
        self.checked_bytes(args, &[])
    }

    /// Checks `commit` out into a new linked working tree at `path`,
    /// without a branch. Files tracked by Git LFS keep their pointers.
    pub fn add_worktree(&self, path: &Path, commit: &str) -> Result<()> {
        let path = path
            .to_str()
            .context("the scratch directory's path is not UTF-8")?;
        self.checked_bytes(
            &["worktree", "add", "--detach", path, commit],
            &[("GIT_LFS_SKIP_SMUDGE", "1")],
        )?;
        Ok(())
    }

    /// The full name of the commit `rev` names, or `None` when the repository
    /// has no such commit.
    pub fn commit(&self, rev: &str) -> Result<Option<String>> {
        // git would take it for an option.
        if rev.starts_with('-') {
            return Ok(None);
        }
        let out = self.raw(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ])?;
        Ok(out
            .success()
            .then(|| out.stdout.trim().to_string())
            .filter(|sha| !sha.is_empty()))
    }

    /// The best common ancestor of two commits, or `None` when the history
    /// in this clone connects them nowhere.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>> {
        let out = self.raw(&["merge-base", a, b])?;
        if out.code == Some(1) {
            return Ok(None);
        }
        Ok(Some(out.checked()?.trim().to_string()))
    }

    /// The content of `path` in commit `rev`, or `None` when the commit has
    /// no such file.
    pub fn file_at(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let spec = format!("{rev}:{path}");
        let exists = self.raw(&["cat-file", "-e", &spec])?;
        if !exists.success() {
            return Ok(None);
        }
        Ok(Some(self.run(&["cat-file", "blob", &spec])?))
    }

    /// Whether the repository is a shallow clone.
    pub fn is_shallow(&self) -> Result<bool> {
        Ok(self.run(&["rev-parse", "--is-shallow-repository"])?.trim() == "true")
    }
}
