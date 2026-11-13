# release-intent Specification

## Purpose
The configuration, units and fixed groups, the adoption of a repository by `init`, changeset files in the format of Changesets, and the merge request check that a change to a unit's shipped files carries a changeset.

## Requirements

### Requirement: Configuration
The tool SHALL read the repository's settings from `.cutwater/config.toml`: the default branch, the release branch, the title of the release merge request, the forge settings, the units, the fixed groups, the repository checks and the adapter settings. It SHALL refuse to run when the file is missing or invalid, naming the setting and the problem. The project SHALL publish a JSON Schema of the file.

#### Scenario: Missing configuration
- **WHEN** any command but `init` runs in a repository without `.cutwater/config.toml`
- **THEN** it refuses, says that the file is missing and suggests `cutwater init`

#### Scenario: Unknown setting
- **WHEN** the configuration contains a setting the tool does not know
- **THEN** every command refuses, naming the setting and its line

### Requirement: Units
A unit SHALL be what is versioned and released as one, as its ecosystem adapter defines it: an npm package, a Maven aggregator with the modules that share its version, or a Cargo crate. Each unit SHALL have a name unique in the repository, a path, an adapter, the patterns of its shipped files and a tag template.

#### Scenario: Duplicate unit name
- **WHEN** two units have the same name
- **THEN** every command refuses and names both paths

### Requirement: Shipped files
Without patterns, a unit's shipped files SHALL be every file under its path. With or without patterns, they SHALL never include the files of units nested under it or the tool's own `.changeset/` and `.cutwater/` directories.

#### Scenario: Nested unit
- **WHEN** a root crate at `.` has a workspace member `crates/core`, and a merge request changes only `crates/core/src/lib.rs`
- **THEN** the change ships `core` and not the root crate

### Requirement: Unit discovery
Units SHALL be either listed in the configuration or discovered by their adapter. A listed unit SHALL override a discovered one of the same name, and the configuration MAY exclude discovered units by name.

#### Scenario: Discovered workspace packages
- **WHEN** the configuration enables npm discovery in a pnpm workspace with three publishable packages and one private package
- **THEN** the repository has three npm units, named after their packages, and the private package is not a unit

### Requirement: Tag templates
A unit's tag template SHALL default to `v{version}` when the repository has one unit and to `{unit}-v{version}` otherwise. A fixed group SHALL have its own tag template, by default `v{version}` when it holds every unit of the repository and `{group}-v{version}` otherwise, and its members SHALL take no tags of their own.

#### Scenario: Several units
- **WHEN** a repository has the units `core` and `api` and neither sets a tag template
- **THEN** the release of `api` 1.4.3 is tagged `api-v1.4.3`

#### Scenario: One group of every unit
- **WHEN** the crates `core` and `api` are the repository's only units and form the fixed group `crates` without a tag template
- **THEN** the group's release 2.0.0 is tagged `v2.0.0`

### Requirement: Fixed groups
A fixed group SHALL name two or more units that always share one version. A unit SHALL belong to at most one fixed group.

#### Scenario: Unit in two groups
- **WHEN** a unit is listed in two fixed groups
- **THEN** every command refuses and names the unit and both groups

### Requirement: Adoption
`init` SHALL create `.cutwater/config.toml` and seed the version manifest with each discovered unit's released version, marked as seeded. It SHALL import the `fixed`, `ignore` and `baseBranch` settings of a Changesets `.changeset/config.json`, and name every other setting it does not carry over. It SHALL refuse while that file declares `linked` groups or `.changeset/pre.json` or `.changeset/pre/` exists, naming what to resolve first.

#### Scenario: Repository on Changesets
- **WHEN** `init` runs in a pnpm workspace whose `.changeset/config.json` declares one fixed group and two pending changesets exist
- **THEN** the configuration holds that fixed group and the `{unit}@{version}` tag format, the manifest holds every package's current version marked as seeded, and both changesets stay pending

#### Scenario: Setting not carried over
- **WHEN** `init` runs while `.changeset/config.json` sets `updateInternalDependencies` to `minor`
- **THEN** it creates the files and says that `updateInternalDependencies` is not carried over

#### Scenario: Changesets prerelease in progress
- **WHEN** `init` runs while `.changeset/pre.json` or `.changeset/pre/` exists
- **THEN** it refuses and says to exit Changesets' prerelease mode first

### Requirement: Tags kept on adoption
`init` SHALL keep the tag format Changesets used for the npm units: `v{version}` for a repository whose root package has no workspaces, and `{unit}@{version}` for the packages of a workspace. Other units SHALL keep the tool's default tag templates. `init` SHALL write every tag template it relies on into the configuration, so that adding a unit later does not change the tags of existing ones.

#### Scenario: Workspace with one publishable package
- **WHEN** `init` runs in a pnpm workspace with one publishable package `@acme/core` and one private package
- **THEN** the release of `@acme/core` 1.2.1 is tagged `@acme/core@1.2.1`

### Requirement: Seeded versions from the registry
For npm and Cargo units, `init` SHALL ask the unit's registry through the ecosystem's tool. A unit whose declared version it lists SHALL be seeded with that version. Otherwise `init` SHALL refuse, naming the unit, the registry asked and the highest version listed, because an unreachable, replaced or denying registry looks like one without the package. `init --new <unit>` SHALL make a unit new, and `init --published <unit>` SHALL seed its declared version without asking.

#### Scenario: Package never published
- **WHEN** `init` runs and the registry lists no version of the package `export`
- **THEN** `init` refuses, names `export` and the registry, and suggests `--new export` if the package was never published

#### Scenario: Yanked declared version
- **WHEN** the declared version of the crate `core` is yanked, so cargo does not list it, and `init --published core` runs
- **THEN** the manifest seeds `core` with its declared version

#### Scenario: Declared new
- **WHEN** `init --new export` runs
- **THEN** the manifest has no entry for `export`, and its first release publishes the version its `package.json` declares

### Requirement: Seeded versions
A seeded manifest entry SHALL count as released: `publish` SHALL neither publish nor tag it, and its next release SHALL replace the mark with an ordinary entry.

#### Scenario: Pipeline after adoption
- **WHEN** the merge request with `init`'s files has landed and the default branch pipeline runs `publish`
- **THEN** `publish` succeeds without publishing, tagging or asking a registry about any seeded unit

### Requirement: Changeset files
A changeset SHALL be a Markdown file `.changeset/<name>.md` whose front matter maps unit names, quoted or not, to `major`, `minor`, `patch` or `none`, and whose body is the entry for the release notes. `none`, and a changeset with empty front matter, SHALL release nothing. The tool SHALL ignore `.changeset/README.md` and `.changeset/config.json`.

#### Scenario: Changesets file
- **WHEN** `.changeset/brave-lions-dance.md` maps `"@acme/web": minor` and `"@acme/api": patch`, and both are units
- **THEN** the tool reads a minor bump of `@acme/web` and a patch bump of `@acme/api` with the file's body as their entry

#### Scenario: Unknown unit or bump
- **WHEN** a changeset names a unit the repository does not have, or an unknown bump
- **THEN** `check`, `status`, `version` and `mr` refuse and name the file, the unit and the value

### Requirement: Adding a changeset
`add` SHALL create one changeset file with a generated unique name. Interactively, it SHALL preselect the units whose shipped files differ from the merge base with the default branch and ask for each selected unit's bump and for the entry. Non-interactively, it SHALL take units, bumps and the entry as arguments, and `--empty` SHALL create a changeset that releases nothing.

#### Scenario: Non-interactive changeset
- **WHEN** `add --unit api:minor --message "Add the export endpoint"` runs
- **THEN** one new file under `.changeset/` releases `api` with a minor bump and that entry

### Requirement: Changeset for a shipped change
`check` SHALL compare the merge request's tree with its merge base and fail when a unit's shipped files changed and no pending changeset the merge request adds or edits names the unit or a member of its fixed group, unless the merge request adds an empty changeset, which covers every unit it touches. It SHALL name the units and files. The release merge request SHALL be exempt.

#### Scenario: Change without a changeset
- **WHEN** a merge request changes a file under unit `api`'s shipped patterns and adds no changeset
- **THEN** `check` fails, names `api` and the changed files, and suggests `cutwater add` or an empty changeset

#### Scenario: Empty changeset
- **WHEN** a merge request changes `api`'s shipped files and adds a changeset with empty front matter
- **THEN** `check` passes

### Requirement: Corrections are reviewed changes
A merge request MAY edit or delete pending changesets that other merge requests added, so that a wrong bump can be lowered and a revert removes its changeset. `check` SHALL list every such edit and deletion in its output, so that reviewers see it.

#### Scenario: Lowering a bump
- **WHEN** a merge request changes a pending changeset of another merge request from `major` to `minor`
- **THEN** `check` passes and lists the file with the old and the new bump

### Requirement: Release merge request exemption
The exemption from the changeset rule SHALL apply only to a merge request whose source branch is the configured release branch of the same project.

#### Scenario: Fork branch with the release branch's name
- **WHEN** a merge request from a fork uses a branch with the release branch's name and changes shipped files without a changeset
- **THEN** `check` fails as for any other merge request

### Requirement: Check without forge or registry
`check` SHALL reach its verdict from the repository alone, without the forge API or any registry. It SHALL compare the merge request's committed tree with the target branch commit the pipeline merged, where the pipeline tests a merged result, and otherwise with the merge base of the merge request's diff base, taken from GitLab's CI variables. It SHALL fail with an explanation when the clone lacks that commit.

#### Scenario: Shallow clone
- **WHEN** `check` runs in a clone that does not contain the merge request's diff base
- **THEN** it fails and says to raise the clone depth

#### Scenario: Merged results pipeline
- **WHEN** the target branch gained a release commit after the merge request's diff base, and `check` runs in a merged results pipeline of a merge request that changes only `api` with a changeset for it
- **THEN** `check` passes and reports neither the release commit's files nor its changesets

### Requirement: Pending status
`status` SHALL list the pending changesets and, for every unit they release, its current version, its next version and the reason: its own changesets, its fixed group, a dependency whose new version its range does not accept, or the final release that ends prerelease mode. `status --json` SHALL print the same as JSON.

#### Scenario: Release caused by a dependency
- **WHEN** `core` has a pending major changeset and `api` depends on `core` with a range that excludes the next major
- **THEN** `status` lists `api` with a patch release caused by `core`
