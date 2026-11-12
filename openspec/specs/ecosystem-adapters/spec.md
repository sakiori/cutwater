# ecosystem-adapters Specification

## Purpose
The built-in npm and Cargo adapters, which discover units, read and write their versions, report the references between them, check and publish what a release ships through the ecosystem's own tools, and the repository checks that plug in as commands.

## Requirements

### Requirement: Ecosystem tools do registry work
Adapters SHALL run the ecosystem's own tools for packing, registry queries and publication, and SHALL not log in to registries.

#### Scenario: Ecosystem tool missing
- **WHEN** `publish` must publish the npm units of a pnpm workspace and `pnpm` is not on the `PATH`
- **THEN** it fails before publishing anything and names the missing tool

### Requirement: npm units
The npm adapter SHALL treat each publishable package of a pnpm or npm workspace, or a single `package.json`, as a unit, whose version is the `version` field. It SHALL report `dependencies`, `peerDependencies` and `optionalDependencies` as runtime, peer and optional edges and `devDependencies` as development edges, and SHALL resolve pnpm catalogs and the `workspace:` protocol, reading `workspace:*` as the exact version.

#### Scenario: Catalog range
- **WHEN** `api` depends on `core` through a pnpm catalog entry `^1.0.0` and `core` is released at 2.0.0
- **THEN** the plan releases `api` with a patch bump, because the range excludes 2.0.0

### Requirement: Other package managers
The npm adapter SHALL refuse a workspace whose root holds the lockfile of a package manager it does not support, such as `yarn.lock`, `bun.lock` or `bun.lockb`, naming the file.

#### Scenario: Yarn workspace
- **WHEN** npm discovery runs in a repository whose root holds `yarn.lock`
- **THEN** every command refuses and names `yarn.lock`

### Requirement: npm packing and publication
The npm adapter SHALL pack each unit with the workspace's package manager, so that `workspace:` references become versions, and publish the packed archive. It SHALL ask the registry from the package's `publishConfig.registry`, or the configured default, whether it holds a version, and SHALL refresh the workspace lockfile when versions change.

#### Scenario: Workspace protocol
- **WHEN** `api` depends on `"core": "workspace:^"` and is packed after `core` became 2.0.0
- **THEN** the packed `package.json` of `api` depends on `"core": "^2.0.0"`

### Requirement: npm distribution tags
A prerelease SHALL be published under its prerelease label as the distribution tag. A final version above the registry's `latest` SHALL become `latest`, and a lower final version SHALL be published under `superseded`.

#### Scenario: Maintenance release
- **WHEN** the registry's `latest` is 2.1.0 and `publish` publishes 1.9.4
- **THEN** 1.9.4 is published under `superseded` and `latest` stays 2.1.0

### Requirement: npm credentials
Publication SHALL support a token from the environment and npm trusted publishing with provenance from GitLab CI, which npm accepts only from GitLab.com's shared runners. Existence checks for a private package SHALL use the environment's read credentials.

#### Scenario: Trusted publishing
- **WHEN** the publish job runs on GitLab.com's shared runners with an npm ID token and no npm token
- **THEN** the adapter publishes with provenance through trusted publishing

### Requirement: Cargo units
The Cargo adapter SHALL treat each publishable member of a Cargo workspace, or a single crate, as a unit, whose version is the package's `version` or the `workspace.package.version` it inherits. `version` SHALL update dependents' version requirements that no longer match and refresh `Cargo.lock`.

#### Scenario: Inherited version
- **WHEN** two crates inherit `workspace.package.version` and are a fixed group
- **THEN** `version` writes the new version once, in `workspace.package`

### Requirement: Cargo publication
The Cargo adapter SHALL package crates with `cargo package`, publish them with `cargo publish` to the crate's registry in dependency order, and ask that registry's index whether a version exists. Publication SHALL support a token from the environment and crates.io trusted publishing from GitLab.com.

#### Scenario: Version already in the index
- **WHEN** the crate's registry index lists the manifest's version
- **THEN** `publish` does not run `cargo publish` for that crate

### Requirement: Adapter interface
The built-in adapters SHALL implement one internal interface: discover units, read and write versions, report edges with their kind, judge whether a range accepts a version, update a dependent's reference, prepare and check what would be published, ask the registry, and publish. Its inputs and outputs SHALL be serializable, so that a later protocol for external adapters can carry them unchanged.

#### Scenario: Edges of a workspace
- **WHEN** `api` depends on `core` at runtime and on `testkit` for development
- **THEN** the adapter reports a runtime edge to `core` and a development edge to `testkit`

### Requirement: Repository checks
A repository check SHALL be a command the configuration names for `check`, `goal` or both. It SHALL receive the units and the plan as JSON on standard input, fail the step with a non-zero exit, and its findings on standard output SHALL appear in the step's output. In `check` the plan SHALL be the releases the pending changesets make, and in `goal` the releases the release merge request makes.

#### Scenario: Repository check in the release merge request
- **WHEN** the configuration names a check for `goal` that rejects a package exporting a removed module
- **THEN** the release merge request's `goal` fails and shows the check's findings
