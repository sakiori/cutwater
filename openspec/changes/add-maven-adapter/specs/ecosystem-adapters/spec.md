## ADDED Requirements

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

#### Scenario: Maven missing
- **WHEN** `publish` must publish a Maven unit and `mvn` is not on the `PATH`
- **THEN** it fails before publishing anything and names the missing tool
