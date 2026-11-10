//! `add`: create a changeset file.

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::{Result, bail};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect, Select};

use super::Repository;
use crate::changes;
use crate::changeset::{self, Bump};
use crate::cli::AddArgs;
use crate::config::CONFIG_FILE;

pub fn run(args: &AddArgs) -> Result<ExitCode> {
    let repo = Repository::open()?;
    let (releases, entry) = if args.empty {
        (Vec::new(), String::new())
    } else if !args.units.is_empty() {
        let mut seen = BTreeSet::new();
        for (unit, _) in &args.units {
            if repo.units.get(unit).is_none() {
                bail!(
                    "`{unit}` is not a unit of this repository; {}",
                    known_units(&repo)
                );
            }
            if !seen.insert(unit) {
                bail!("`--unit` names `{unit}` twice");
            }
        }
        let entry = args.message.clone().unwrap_or_default();
        if entry.trim().is_empty() {
            bail!("the entry for the release notes, `--message`, is empty");
        }
        (args.units.clone(), entry)
    } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        ask(&repo)?
    } else {
        bail!(
            "no unit given and no terminal to ask in: pass `--unit <name>:<bump>` with \
             `--message <entry>`, or `--empty` for a change that releases nothing"
        );
    };
    let file = changeset::create(repo.git.root(), &releases, &entry)?;
    println!("Created {file}");
    Ok(ExitCode::SUCCESS)
}

fn known_units(repo: &Repository) -> String {
    let names: Vec<String> = repo
        .units
        .iter()
        .map(|unit| format!("`{}`", unit.name))
        .collect();
    if names.is_empty() {
        "it has none".to_string()
    } else {
        format!("its units are {}", names.join(", "))
    }
}

/// Asks for the units, their bumps and the entry, preselecting the units
/// whose shipped files differ from the merge base with the default branch.
fn ask(repo: &Repository) -> Result<(Vec<(String, Bump)>, String)> {
    let units: Vec<_> = repo.units.iter().collect();
    if units.is_empty() {
        bail!("this repository has no units; turn on discovery or list units in {CONFIG_FILE}");
    }
    let branch = &repo.config.default_branch;
    let changed = match changes::merge_base_with_branch(&repo.git, branch)? {
        Some(base) => changes::since(&repo.git, &base)?,
        None => {
            eprintln!(
                "No merge base with the default branch `{branch}`, so no unit is preselected."
            );
            Vec::new()
        }
    };
    let preselected: Vec<bool> = units
        .iter()
        .map(|unit| changed.iter().any(|file| unit.ships(&file.path)))
        .collect();
    let items: Vec<String> = units
        .iter()
        .map(|unit| format!("{} ({})", unit.name, unit.path))
        .collect();

    let theme = ColorfulTheme::default();
    let chosen = MultiSelect::with_theme(&theme)
        .with_prompt("Units this change releases (space selects, enter confirms)")
        .items(&items)
        .defaults(&preselected)
        .interact()?;
    if chosen.is_empty() {
        let empty = Confirm::with_theme(&theme)
            .with_prompt("No unit selected. Create an empty changeset, which releases nothing?")
            .default(false)
            .interact()?;
        if !empty {
            bail!("no changeset created");
        }
        return Ok((Vec::new(), String::new()));
    }
    let bumps: Vec<&str> = Bump::ALL.iter().map(|bump| bump.as_str()).collect();
    let mut releases = Vec::new();
    for index in chosen {
        let unit = &units[index].name;
        let bump = Select::with_theme(&theme)
            .with_prompt(format!("Bump of {unit}"))
            .items(&bumps)
            .default(0)
            .interact()?;
        releases.push((unit.clone(), Bump::ALL[bump]));
    }
    let entry: String = Input::with_theme(&theme)
        .with_prompt("Entry for the release notes")
        .validate_with(|entry: &String| {
            if entry.trim().is_empty() {
                Err("the entry may not be empty")
            } else {
                Ok(())
            }
        })
        .interact_text()?;
    Ok((releases, entry))
}
