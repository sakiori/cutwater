//! Repository checks: commands the configuration names, which `check` and
//! `goal` run after their own checks. A check reads the units and the plan
//! as JSON on standard input; a non-zero exit fails the step.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::adapter::AdapterKind;
use crate::config::{CheckSettings, CheckStep};
use crate::plan::Plan;

/// What a repository check reads on standard input.
#[derive(Debug, Clone, Serialize)]
pub struct Input {
    /// The step that runs the check.
    pub step: CheckStep,
    /// Every unit of the repository, ordered by name.
    pub units: Vec<UnitInput>,
    pub plan: PlanInput,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnitInput {
    pub name: String,
    /// The unit's directory relative to the repository root, `.` for the
    /// root.
    pub path: String,
    pub adapter: AdapterKind,
    /// The unit's fixed group.
    pub group: Option<String>,
    /// The released version the manifest holds; `None` for a new unit.
    pub released: Option<String>,
    /// The version the unit's files declare.
    pub declared: Option<String>,
}

/// The releases the step judges: in `check`, those the pending changesets
/// make; in `goal`, those the release merge request makes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PlanInput {
    /// In the order of publication.
    pub releases: Vec<ReleaseInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseInput {
    pub unit: String,
    pub adapter: AdapterKind,
    /// The released version before the release; `None` for a new unit.
    pub current: Option<String>,
    pub next: String,
}

impl PlanInput {
    /// The releases of a plan.
    pub fn of(plan: &Plan) -> PlanInput {
        PlanInput {
            releases: plan
                .releases
                .iter()
                .map(|release| ReleaseInput {
                    unit: release.unit.clone(),
                    adapter: release.adapter,
                    current: release.current.as_ref().map(ToString::to_string),
                    next: release.next.to_string(),
                })
                .collect(),
        }
    }
}

/// The checks the configuration names for `step`.
pub fn for_step(checks: &[CheckSettings], step: CheckStep) -> Vec<&CheckSettings> {
    checks
        .iter()
        .filter(|check| check.steps.contains(&step))
        .collect()
}

/// Runs the checks in the repository at `root`, one after another, each
/// with `input` on standard input and its output passed through; `true`
/// when every check passed.
pub fn run(root: &Path, checks: &[&CheckSettings], input: &Input) -> Result<bool> {
    let json = serde_json::to_vec(input).expect("the check input serializes");
    let mut passed = true;
    for check in checks {
        let name = check.name.get_ref();
        let (program, args) = check
            .command
            .split_first()
            .expect("a check's command names a program");
        crate::outln!("Repository check `{name}`:")?;
        std::io::stdout().flush()?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("repository check `{name}`: cannot run `{program}`"))?;
        let mut stdin = child.stdin.take().expect("the check's stdin is piped");
        // The check may exit without reading its input, or read it only
        // after writing more than a pipe holds.
        let writer = std::thread::spawn({
            let json = json.clone();
            move || match stdin.write_all(&json) {
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                other => other,
            }
        });
        let status = child
            .wait()
            .with_context(|| format!("repository check `{name}`: cannot wait for `{program}`"))?;
        writer
            .join()
            .expect("the input writer does not panic")
            .with_context(|| format!("repository check `{name}`: cannot write its input"))?;
        if status.success() {
            crate::outln!("Repository check `{name}` passed.")?;
        } else {
            let how = match status.code() {
                Some(code) => format!("exit status {code}"),
                None => "terminated by a signal".to_string(),
            };
            eprintln!("error: repository check `{name}` failed ({how})");
            passed = false;
        }
    }
    Ok(passed)
}
