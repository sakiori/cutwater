//! The only place that runs `git`: the installed git, as a subprocess, whose
//! failures become [`GitError`]s naming the command and carrying git's message.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// A git handle bound to the top-level directory of a working tree.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

impl Git {
    /// The handle of the working tree that contains `dir`.
    pub fn discover(dir: &Path) -> Result<Self> {
        let probe = Self {
            dir: dir.to_path_buf(),
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
        })
    }

    /// The top-level directory of the working tree.
    pub fn root(&self) -> &Path {
        &self.dir
    }

    fn command<S: AsRef<str>>(&self, args: &[S]) -> Command {
        let mut cmd = Command::new("git");
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

    /// Runs git and captures the result whatever the exit status.
    pub fn raw<S: AsRef<str>>(&self, args: &[S]) -> Result<Output> {
        let owned: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
        let out = self
            .command(args)
            .output()
            .with_context(|| format!("cannot run `git {}`; is git installed?", owned.join(" ")))?;
        Ok(Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            args: owned,
        })
    }

    /// Runs git and returns stdout, failing on a non-zero exit status.
    pub fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<String> {
        Ok(self.raw(args)?.checked()?)
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
