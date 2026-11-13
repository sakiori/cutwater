# ecosystem-adapters Specification

## Purpose
The built-in npm, Cargo and Maven adapters, which discover units, read and write their versions, report the references between them, check and publish what a release ships through the ecosystem's own tools, and the repository checks that plug in as commands.

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
The built-in adapters SHALL implement one internal interface: discover units, read and write versions, report edges with their kind and what the files break of the adapter's rules, judge whether a range accepts a version, update a dependent's reference, change the files on adoption, prepare and check what would be published, ask the registry, and publish. Its inputs and outputs SHALL be serializable, so that a later protocol for external adapters can carry them unchanged.

#### Scenario: Edges of a workspace
- **WHEN** `api` depends on `core` at runtime and on `testkit` for development
- **THEN** the adapter reports a runtime edge to `core` and a development edge to `testkit`

### Requirement: Repository checks
A repository check SHALL be a command the configuration names for `check`, `goal` or both. It SHALL receive the units and the plan as JSON on standard input, fail the step with a non-zero exit, and its findings on standard output SHALL appear in the step's output. In `check` the plan SHALL be the releases the pending changesets make, and in `goal` the releases the release merge request makes.

#### Scenario: Repository check in the release merge request
- **WHEN** the configuration names a check for `goal` that rejects a package exporting a removed module
- **THEN** the release merge request's `goal` fails and shows the check's findings

### Requirement: Maven units
The Maven adapter SHALL treat a directory whose aggregator POM sets its version to `${revision}` as one unit, whose modules share that version, and SHALL read units and the edges between them from the POMs without running Maven. It SHALL support Maven 3.9 or a later Maven 3 with the flatten plugin configured to resolve `revision` and to interpolate `dependencies` and `dependencyManagement`, so that deployed POMs carry no property references.

#### Scenario: Aggregator with modules
- **WHEN** `billing/pom.xml` aggregates `billing-api` and `billing-service` and sets its version to `${revision}`
- **THEN** `billing` is one unit and both modules release with its version

### Requirement: Maven version
Before its first Maven step for a unit, a run SHALL read the version of the `mvn` on the `PATH` once, and SHALL refuse a Maven older than 3.9, or one that is no Maven 3, naming the version found.

#### Scenario: Older Maven
- **WHEN** `goal` must build a Maven unit and the `mvn` on the `PATH` is Maven 3.8.8
- **THEN** it fails before running Maven for the unit and names Maven 3.8.8

### Requirement: Maven module versions
Every module of a unit SHALL take the unit's `revision` as its version: from a parent among the unit's POMs, or by declaring `${revision}`. The adapter SHALL refuse any other module, naming it and its POM.

#### Scenario: Parent outside the unit
- **WHEN** `billing-api` declares no version and inherits from a POM outside the unit `billing`
- **THEN** every command refuses and names `billing-api`'s POM

### Requirement: POMs outside units
A POM that is not part of a Maven unit, such as a test fixture, SHALL not stop a command when it cannot be read, also in a repository without Maven units; one that a unit needs SHALL make every command refuse, naming the file.

#### Scenario: Malformed fixture
- **WHEN** `init` runs in an npm workspace that holds a malformed `pom.xml` as a test fixture
- **THEN** `init` goes on as without it

### Requirement: Maven dependents are released
A Maven reference SHALL accept only the exact version its dependent's last release names, because the deployed POM fixes that version and Maven's nearest-wins mediation gives it to the dependent's consumers. A `compile`, `runtime` or `import` dependency and a parent SHALL be runtime edges, and a `test`, `provided`, `system` or optional dependency, a plugin and a plugin's dependency, none of which consumers resolve, development edges.

#### Scenario: Producer released, dependent without a changeset
- **WHEN** `billing` takes a minor release to 1.4.0 and `orders`, which depends on it at the `compile` scope through `billing.version`, has no pending changeset
- **THEN** the plan releases `orders` with a patch bump and its entry says that `billing` was updated to 1.4.0

#### Scenario: Test dependency only
- **WHEN** `billing` takes a minor release and `orders` depends on it only at the `test` scope
- **THEN** the plan does not release `orders`

### Requirement: Maven managed versions
A version that a unit's `dependencyManagement` manages for another unit, other than an imported BOM, SHALL release the managing unit when the managed unit is released, as a managed edge that orders no publication, since nothing needs it to build.

#### Scenario: BOM managing a unit that imports it
- **WHEN** the BOM unit `platform` manages `orders`' version, `orders` imports `platform`, and `orders` is released
- **THEN** the plan releases `platform` too, and neither `status` nor `version` reports a cycle

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

### Requirement: Maven parents
A unit SHALL not inherit from another unit's POM, whose version Maven 3 takes from no property, and `check` SHALL fail when one does, naming both units.

#### Scenario: Parent of another unit
- **WHEN** `ledger/pom.xml` inherits from `billing-api` of the unit `billing`
- **THEN** `check` fails and names `ledger` and `billing`

### Requirement: Maven adoption
`init` SHALL set every dependent's producer property to the producer's `revision`, and SHALL refuse, naming each, when a dependent refers to a producer with a literal version. It SHALL seed each Maven unit with the highest final version its deploy repository holds.

#### Scenario: Team developing on the next minor
- **WHEN** `init` seeds `billing` 1.3.0 from its deploy repository while its `revision` is 1.4.0-SNAPSHOT
- **THEN** the seeded repository passes `check`, because 1.4.0 is above 1.3.0

### Requirement: Maven seeding refused
`init` SHALL refuse a Maven unit whose deploy repository cannot be asked, holds no final version, or holds a final version that is not below the release its `revision` leads to, naming the unit and that version, because an unreachable or hiding repository looks like an empty one; `--new` SHALL declare a unit new. `init` SHALL refuse `--published` for a Maven unit, whose files hold no released version.

#### Scenario: Revision behind the deploy repository
- **WHEN** `init` runs while `billing`'s `revision` is 1.3.0-SNAPSHOT and its deploy repository holds 1.3.0
- **THEN** `init` refuses, names `billing` and 1.3.0, and suggests the snapshot of a later version

#### Scenario: Unit never released
- **WHEN** `init` runs and the deploy repository holds no final version of `ledger`
- **THEN** `init` refuses, names `ledger`, and suggests `--new ledger`

### Requirement: Maven release builds
A release build SHALL build only the units the release publishes, one per Maven invocation in topological order, passing `-Drevision` with the unit's released version and each producer's version property with the producer's released version from the manifest. It SHALL install each built unit into a job-local repository, from which it also reads the unit's effective POMs; producers the release does not publish SHALL resolve from the deploy repository.

#### Scenario: Release build of a dependent
- **WHEN** the release publishes `billing` 1.4.0 and `orders`, and `orders` also depends on `ledger`, which the release does not publish and the manifest holds at 2.1.0
- **THEN** `orders` is built with `-Dbilling.version=1.4.0` against the `billing` built earlier in the job and `-Dledger.version=2.1.0` against `ledger` 2.1.0 from the deploy repository

#### Scenario: BOM of a unit released with it
- **WHEN** the release publishes `billing` 1.4.0 and `orders`, which imports `billing`'s BOM through `billing.version`
- **THEN** `goal` and `publish` resolve the BOM from the `billing` built earlier in the job

### Requirement: Deployed POMs carry released versions
`goal` SHALL inspect the parent, the dependencies and the dependency management, also of profiles, of the POM each deployable module would deploy, and fail when they hold a snapshot version or an unresolved property reference, naming the module and the value.

#### Scenario: Property left in a deployed POM
- **WHEN** the flatten plugin of `orders` keeps `${billing.version}` in a dependency of the deployed POM
- **THEN** `goal` fails, names `orders`' module and `${billing.version}`

### Requirement: Questions to the deploy repository
The adapter SHALL resolve each unit's effective deploy repository with one Maven invocation, and ask that repository alone, with one Maven invocation per round, a local repository of the run's own and settings that hold the `<server>` entries of the repository and the mirrors as written, the proxies, the settings-security file, and the mirrors, none of which mirrors that repository, but no profile or other repository, found as the build in the unit's directory finds them.

#### Scenario: Release in a repository of the settings
- **WHEN** an active profile of the settings adds a repository that holds `billing` 1.4.0, and the deploy repository does not
- **THEN** `publish` finds `billing` 1.4.0 missing and deploys it

#### Scenario: Settings of a unit's directory
- **WHEN** the unit `billing` lies in `java/`, and `java/.mvn/maven.config` names `--settings=.mvn/settings.xml`, which holds the deploy repository's credentials
- **THEN** the questions to the deploy repository use those credentials

#### Scenario: Authenticated mirror
- **WHEN** Maven reaches its plugins only through a mirror that needs the credentials of its `<server>` entry
- **THEN** the questions to the deploy repository resolve their plugins through that mirror with those credentials

### Requirement: One build in each release
Before it completes a module that the deploy repository partly holds, `publish` SHALL compare the files the repository holds with those its own build made, a detached signature by the file it signs, and on any difference SHALL upload nothing, naming the module and the files that differ and saying that the build is not reproducible, so that no release mixes the files of two builds.

#### Scenario: Retry with a build that is not reproducible
- **WHEN** an earlier `publish` deployed the POM of `billing-api` and failed before its JAR, and the retry's build makes another POM
- **THEN** `publish` fails before uploading anything, names `billing-api` and its POM, and says to make the build reproducible

#### Scenario: Signature made again
- **WHEN** an earlier `publish` of a reproducible build deployed the JAR of `billing-api` and its `.asc` and failed before its POM, and the retry signs the JAR again
- **THEN** the retried `publish` deploys the POM alone and keeps the stored signature

### Requirement: Wrong SHA-1 in the deploy repository
When the deploy repository holds a file of a module's release with a SHA-1 that does not match the file, `publish` SHALL upload nothing for the unit, naming the module and the file, and say that the file and its checksums must be deleted there, or a new version released.

#### Scenario: Wrong SHA-1
- **WHEN** the deploy repository holds the JAR of `billing-api` with a SHA-1 that does not match it
- **THEN** `publish` uploads nothing, names `billing-api` and its JAR, and says to delete the file there or release a new version

### Requirement: Maven deployment
A module's release SHALL count as deployed only when its deploy repository holds every file its release build installed, the POM, the main artifact and the attached artifacts, each with its SHA-1 but detached signatures, of which Maven keeps none. `publish` SHALL deploy only the missing files, and the SHA-1 alone of a file that lacks one, each on its own and the POM last, then ask the repository again, and SHALL count the unit as published only when every file is there, whatever Maven answered.

#### Scenario: Deployment interrupted between modules
- **WHEN** an earlier `publish` deployed two of `billing`'s three modules and failed
- **THEN** the retried `publish` deploys only the third module and then tags `billing`

#### Scenario: POM without its JAR
- **WHEN** an earlier `publish` deployed the POM of `billing-api` and failed before its JAR
- **THEN** the retried `publish` deploys the JAR alone, without uploading the POM again, and then tags `billing`

#### Scenario: Signed module
- **WHEN** `publish` deploys `billing-api` with its JAR's `.asc` to a repository that refuses to replace a file
- **THEN** `billing` counts as published without a SHA-1 of the `.asc`

#### Scenario: Deployed POM that imports an external BOM
- **WHEN** the POM that `ledger` deploys imports a BOM from outside the deploy repository
- **THEN** `publish` finds each of `ledger`'s files in the deploy repository and does not upload it again

#### Scenario: File without its SHA-1
- **WHEN** an earlier `publish` deployed the POM of `billing` and failed before its SHA-1
- **THEN** the retried `publish` deploys that SHA-1 alone

#### Scenario: Refused file
- **WHEN** the deploy repository refuses a file of `billing-api` and holds none
- **THEN** `publish` fails, names `billing-api` with the files it holds and lacks, and tags nothing

#### Scenario: Maven missing
- **WHEN** `publish` must publish a Maven unit and `mvn` is not on the `PATH`
- **THEN** it fails before publishing anything and names the missing tool
