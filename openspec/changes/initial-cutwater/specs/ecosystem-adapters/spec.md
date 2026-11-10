## ADDED Requirements

### Requirement: Ecosystem tools do registry work
Adapters SHALL run the ecosystem's own tools for packing, registry queries and publication, and SHALL not log in to registries.

#### Scenario: Ecosystem tool missing
- **WHEN** `publish` must publish a Maven unit and `mvn` is not on the `PATH`
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

### Requirement: Maven units
The Maven adapter SHALL treat a directory whose aggregator POM sets its version to `${revision}` as one unit, whose modules share that version, and SHALL read units and the edges between them from the POMs without running Maven. It SHALL support Maven 3 with the flatten plugin configured to resolve `revision` and to interpolate `dependencies` and `dependencyManagement`, so that deployed POMs carry no property references.

#### Scenario: Aggregator with modules
- **WHEN** `billing/pom.xml` aggregates `billing-api` and `billing-service` and sets its version to `${revision}`
- **THEN** `billing` is one unit and both modules release with its version

### Requirement: Maven versions
A Maven unit's `revision` SHALL be a semantic version followed by `-SNAPSHOT`. The adapter SHALL refuse any other value, naming the unit, the POM and the value.

#### Scenario: Two-part version
- **WHEN** a unit's aggregator POM sets `revision` to `1.0-SNAPSHOT`
- **THEN** every command refuses and names the unit, its POM and `1.0-SNAPSHOT`

### Requirement: Maven snapshots on the default branch
The committed `revision` of a released unit SHALL be a snapshot whose base version is above its released version; `version` SHALL write the next patch snapshot, or in prerelease mode the next final version's snapshot. A dependent SHALL refer to a producer through a version property that `version` keeps equal to the producer's `revision`, so one merge request can change both. `check` SHALL fail when either rule does not hold.

#### Scenario: Release of a Maven unit
- **WHEN** the manifest holds `billing` 1.3.0, its `revision` is 1.3.1-SNAPSHOT, a minor changeset is applied, and `orders` refers to `billing` through `billing.version`
- **THEN** the manifest holds `billing` 1.4.0, and both `billing`'s `revision` and `orders`' `billing.version` are 1.4.1-SNAPSHOT

#### Scenario: Property out of step
- **WHEN** a merge request sets `orders`' `billing.version` to a value other than `billing`'s `revision`
- **THEN** `check` fails and names the property, its value and the producer's `revision`

### Requirement: Maven adoption
`init` SHALL set every dependent's producer property to the producer's `revision`, and SHALL refuse, naming each, when a dependent refers to a producer with a literal version. A Maven unit whose deploy repository holds no final version SHALL be seeded as new, without a manifest entry.

#### Scenario: Team developing on the next minor
- **WHEN** `init` seeds `billing` 1.3.0 from its deploy repository while its `revision` is 1.4.0-SNAPSHOT
- **THEN** the seeded repository passes `check`, because 1.4.0 is above 1.3.0

### Requirement: Maven release builds
A release build SHALL build only the units the release publishes, one per Maven invocation in topological order, passing `-Drevision` with the unit's released version and each producer's version property with the producer's released version from the manifest. It SHALL install each built unit into a job-local repository; producers the release does not publish SHALL resolve from the deploy repository.

#### Scenario: Release build of a dependent
- **WHEN** the release publishes `billing` 1.4.0 and `orders`, and `orders` also depends on `ledger`, which the release does not publish and the manifest holds at 2.1.0
- **THEN** `orders` is built with `-Dbilling.version=1.4.0` against the `billing` built earlier in the job and `-Dledger.version=2.1.0` against `ledger` 2.1.0 from the deploy repository

### Requirement: Deployed POMs carry released versions
`goal` SHALL inspect the parent, dependencies and dependency management of the POM each deployable module would deploy, and fail when they hold a snapshot version or an unresolved property reference, naming the module and the value.

#### Scenario: Property left in a deployed POM
- **WHEN** the flatten plugin of `orders` keeps `${billing.version}` in a dependency of the deployed POM
- **THEN** `goal` fails, names `orders`' module and `${billing.version}`

### Requirement: Maven deployment
The adapter SHALL resolve each unit's effective deploy repository with one Maven invocation and ask that repository, through Maven with the environment's settings and an empty local repository, which deployable modules of the version exist. It SHALL deploy only the missing modules. The released version of a Maven unit that `init` seeds SHALL be the highest final version that repository holds.

#### Scenario: Deployment interrupted between modules
- **WHEN** an earlier `publish` deployed two of `billing`'s three modules and failed
- **THEN** the retried `publish` deploys only the third module and then tags `billing`

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
A repository check SHALL be a command the configuration names for `check`, `goal` or both. It SHALL receive the units and the plan as JSON on standard input, fail the step with a non-zero exit, and its findings on standard output SHALL appear in the step's output.

#### Scenario: Repository check in the release merge request
- **WHEN** the configuration names a check for `goal` that rejects a package exporting a removed module
- **THEN** the release merge request's `goal` fails and shows the check's findings
