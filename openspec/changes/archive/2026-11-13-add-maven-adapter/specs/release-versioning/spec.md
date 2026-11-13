## MODIFIED Requirements

### Requirement: New and removed units
A unit without a manifest entry SHALL be new and SHALL be released only once a changeset names it with a bump other than `none`. Its first release SHALL be the version its adapter's files declare, for Maven the `revision` without `-SNAPSHOT`, and the changesets naming it SHALL only provide its changelog entry.

#### Scenario: First release of a new package
- **WHEN** a merge request adds the npm package `export` at version 0.1.0 with a changeset, and the release merge request lands
- **THEN** the manifest holds `export` 0.1.0 and `publish` releases it at 0.1.0

#### Scenario: First release of a new Maven unit
- **WHEN** a merge request adds the Maven unit `billing` with `revision` `0.1.0-SNAPSHOT` and a changeset naming it, and the release merge request lands
- **THEN** the manifest holds `billing` 0.1.0 and `publish` deploys it at 0.1.0

### Requirement: Dependencies the ecosystem's tool publishes first
The release graph SHALL also hold every dependency between units that the ecosystem's tool needs in the registry before it publishes the dependent: for Cargo, each dependency that names a version, development dependencies included; for Maven, every reference the release build resolves, test dependencies included, but no version that `dependencyManagement` only manages. Such an edge SHALL count between members of the same fixed group too, as the tool publishes them one at a time.

#### Scenario: Cargo development dependency with a version
- **WHEN** the crate `core` declares `testkit` as a development dependency with a version, and `testkit` depends on `core` at runtime, in different groups
- **THEN** `status`, `version` and `mr` refuse and list the edges `core → testkit` and `testkit → core`, because cargo publishes neither before the other is in the registry

#### Scenario: Such a cycle inside a fixed group
- **WHEN** the crates `core` and `testkit` form a fixed group, `core` declares `testkit` as a development dependency with a version, and `testkit` depends on `core` at runtime with a version
- **THEN** `status`, `version` and `mr` refuse and list both edges, and do not suggest a fixed group

#### Scenario: Maven test dependency
- **WHEN** the Maven unit `orders` depends on `ledger` with the `test` scope
- **THEN** `ledger` is published before `orders`, whose release build resolves it

### Requirement: Releases caused by dependencies
A unit SHALL take a patch bump when a unit it depends on through a runtime or optional dependency, or whose version it manages for its consumers as a Maven BOM does, is released at a version outside the range the dependent declares, judged by the ecosystem's own range rules. Cargo build dependencies SHALL count as runtime dependencies. Development dependencies SHALL not cause a release. The changelog entry of such a release SHALL name the dependencies and their new versions.

#### Scenario: Range excludes the new major
- **WHEN** `api` depends on `core` with `^1.2.0` and `core` is released at 2.0.0
- **THEN** the plan releases `api` with a patch bump and its entry says that `core` was updated to 2.0.0

#### Scenario: Development dependency
- **WHEN** `api` depends on `core` only as a development dependency and `core` takes a major release
- **THEN** the plan does not release `api`
