## ADDED Requirements

### Requirement: Version plan
The next version of a unit SHALL follow the highest bump among the pending changesets that name it. A fixed group SHALL take the highest next version among its members, and every member SHALL take that version. The plan SHALL repeat these rules, and the dependency rule, until no unit changes.

#### Scenario: Highest bump wins
- **WHEN** `api` at 1.4.2 has one pending patch changeset and one pending minor changeset
- **THEN** the plan releases `api` at 1.5.0

#### Scenario: Fixed group
- **WHEN** `web` and `ui` form a fixed group at 2.1.0 and only `ui` has a pending minor changeset
- **THEN** the plan releases both at 2.2.0

### Requirement: Shared written versions
Units whose adapter writes one shared version, such as Cargo crates that inherit `workspace.package.version`, SHALL form a fixed group. Every command that plans SHALL refuse otherwise, naming the units and the file that holds their version.

#### Scenario: Inherited version outside a group
- **WHEN** the crates `core` and `api` inherit `workspace.package.version` and no fixed group holds both
- **THEN** `status` refuses and names `core`, `api` and `Cargo.toml`

### Requirement: Releases caused by dependencies
A unit SHALL take a patch bump when a unit it depends on through a runtime or optional dependency is released at a version outside the range the dependent declares, judged by the ecosystem's own range rules. Cargo build dependencies SHALL count as runtime dependencies. Development dependencies SHALL not cause a release. The changelog entry of such a release SHALL name the dependencies and their new versions.

#### Scenario: Range excludes the new major
- **WHEN** `api` depends on `core` with `^1.2.0` and `core` is released at 2.0.0
- **THEN** the plan releases `api` with a patch bump and its entry says that `core` was updated to 2.0.0

#### Scenario: Development dependency
- **WHEN** `api` depends on `core` only as a development dependency and `core` takes a major release
- **THEN** the plan does not release `api`

### Requirement: Releases caused by peer dependencies
A unit SHALL take a major bump when a unit it declares as a peer dependency takes a minor or major bump whose version, without its prerelease part, is outside the declared range, because the rewritten range drops versions its consumers already install. Any other peer release outside the range SHALL cause a patch bump, as Changesets does. The changelog entry SHALL name the dependency and its new version.

#### Scenario: Peer range excludes the new major
- **WHEN** `ui` declares `web` as a peer dependency with `^1.0.0` and `web` is released at 2.0.0
- **THEN** the plan releases `ui` with a major bump and its entry says that `web` was updated to 2.0.0

#### Scenario: Patch of an exact peer
- **WHEN** `ui` declares `web` as a peer dependency with `workspace:*` and `web` takes a patch release
- **THEN** the plan releases `ui` with a patch bump

#### Scenario: Peer prerelease inside the range
- **WHEN** prerelease mode is on, `ui` declares `web` as a peer dependency with `^1.0.0` and `web` at 1.3.0 takes a minor prerelease to 1.4.0-beta.0
- **THEN** the plan releases `ui` with a patch bump, because 1.4.0 is inside the range

### Requirement: Release graph
The release graph's edges SHALL be the runtime, Cargo build and optional dependencies between units. It SHALL be acyclic, ignoring edges between members of the same fixed group. A cycle SHALL make every command that plans refuse and list the edges of the cycle. Publication SHALL follow the graph's topological order, and members of a fixed group SHALL be ordered by their own edges.

#### Scenario: Cycle between two units
- **WHEN** `a` depends on `b` at runtime and `b` depends on `a` at runtime, in different groups
- **THEN** `status`, `version` and `mr` refuse and list the edges `a → b` and `b → a`

#### Scenario: Order inside a group
- **WHEN** `web` and `ui` form a fixed group and `web` depends on `ui`
- **THEN** `ui` is published before `web`

### Requirement: Dependencies the ecosystem's tool publishes first
The release graph SHALL also hold every dependency between units that the ecosystem's tool needs in the registry before it publishes the dependent: for Cargo, each dependency that names a version, development dependencies included. Such an edge SHALL count between members of the same fixed group too, because the tool publishes them one at a time.

#### Scenario: Cargo development dependency with a version
- **WHEN** the crate `core` declares `testkit` as a development dependency with a version, and `testkit` depends on `core` at runtime, in different groups
- **THEN** `status`, `version` and `mr` refuse and list the edges `core → testkit` and `testkit → core`, because cargo publishes neither before the other is in the registry

#### Scenario: Such a cycle inside a fixed group
- **WHEN** the crates `core` and `testkit` form a fixed group, `core` declares `testkit` as a development dependency with a version, and `testkit` depends on `core` at runtime with a version
- **THEN** `status`, `version` and `mr` refuse and list both edges, and do not suggest a fixed group

### Requirement: Version manifest
The tool SHALL keep `.cutwater/versions.toml` with the released version of every unit. Only `init` and `version` SHALL write it, and `check` SHALL fail a merge request other than the release merge request that changes an existing manifest. Where an adapter's files also hold the released version, `check` SHALL fail when the two disagree, naming the unit and both versions.

#### Scenario: Manual version edit
- **WHEN** a merge request changes the `version` of an npm unit's `package.json` without changing the manifest
- **THEN** `check` fails, names the unit, the manifest's version and the package file's version

#### Scenario: Revert of a release merge request
- **WHEN** a merge request reverts a landed release merge request
- **THEN** `check` fails, because published versions cannot be withdrawn and a wrong release is corrected by the next one

### Requirement: Plannable merge requests
`check` SHALL fail a merge request whose tree cannot be planned, for the reasons that make planning refuse: a cycle in the release graph, or units that share a written version outside a fixed group. It SHALL name what `status` would name.

#### Scenario: Cycle introduced by a merge request
- **WHEN** a merge request makes `core` depend at runtime on `api`, which already depends at runtime on `core`, in different groups
- **THEN** `check` fails and lists the edges `api → core` and `core → api`

### Requirement: New and removed units
A unit without a manifest entry SHALL be new and SHALL be released only once a changeset names it with a bump other than `none`. Its first release SHALL be the version its adapter's files declare, for Maven the `revision` without `-SNAPSHOT`, and the changesets naming it SHALL only provide its changelog entry.

#### Scenario: First release of a new package
- **WHEN** a merge request adds the npm package `export` at version 0.1.0 with a changeset, and the release merge request lands
- **THEN** the manifest holds `export` 0.1.0 and `publish` releases it at 0.1.0

### Requirement: Removed units
`version` SHALL remove the manifest entry of a unit that no longer exists when it applies a release. A removal alone SHALL not make a release. Renaming is a removal and an addition, and existing tags stay.

#### Scenario: Package deleted
- **WHEN** a merge request deletes the package `legacy` with an empty changeset, and later `api` takes a release
- **THEN** the release merge request removes `legacy` from the manifest, and none is opened before `api`'s release

### Requirement: New members of fixed groups
A new unit that joins a fixed group with released members SHALL be released with its group, at the group's next version, and the version its files declare SHALL be ignored. Its changesets SHALL count toward the group's bump like those of any member.

#### Scenario: Package added to a group
- **WHEN** `web` and `ui` form a fixed group at 2.1.0, a merge request adds `icons` at 0.1.0 to the group with a minor changeset, and `ui` has a pending patch changeset
- **THEN** the plan releases `web`, `ui` and `icons` at 2.2.0

### Requirement: Applying the plan
`version` SHALL apply the plan to the working tree: write each released unit's version into the manifest and through its adapter, update the references of dependents through their adapter, refresh the lockfiles the adapters own, and delete the applied changesets. With nothing pending, it SHALL leave the working tree unchanged.

#### Scenario: Applied release
- **WHEN** `version` runs with one pending minor changeset for `api` at 1.4.2
- **THEN** `api`'s files and the manifest hold 1.5.0, the workspace lockfile is refreshed by the package manager, and the changeset file is gone

### Requirement: Changelogs
`version` SHALL prepend each released unit's entry to its `CHANGELOG.md`. By default an entry SHALL be headed by the version, group the changeset entries by bump, and name no commit hashes. A fixed group's members SHALL each get their own entry.

#### Scenario: Entry of a minor release
- **WHEN** `version` releases `api` 1.5.0 from one minor and one patch changeset
- **THEN** `api/CHANGELOG.md` starts with a 1.5.0 entry listing the minor change before the patch change

### Requirement: Prerelease mode
`pre enter <label>` SHALL record prerelease mode in `.cutwater/pre.toml`. While it is recorded, every release SHALL be `<next>-<label>.<n>`, counting `n` from 0 per unit and next version, including releases caused by dependencies. `version` SHALL delete applied changeset files as usual. `pre exit` SHALL make the next release final. Both SHALL be merged through a merge request.

#### Scenario: Prerelease sequence
- **WHEN** `api` is at 1.4.2, `pre enter beta` is merged, and two minor changesets are applied by two releases
- **THEN** the releases are 1.5.0-beta.0 and 1.5.0-beta.1, and after `pre exit` the next release is 1.5.0 with both entries in its changelog

### Requirement: Changelog of a final release after prereleases
While prerelease mode is recorded, `version` SHALL record in `.cutwater/pre.toml` the entries of the changesets it applies and the dependency updates of the releases they cause, so that the final release's changelog carries all of them with the dependencies' final versions.

#### Scenario: Release caused only by a dependency
- **WHEN** `ui` takes prereleases only because `web` does, and `pre exit` is followed by the final release
- **THEN** `ui`'s final entry names `web` with its final version

### Requirement: Updated dependencies in changelogs
A release's changelog entry SHALL list its dependency updates under Patch Changes, as Changesets does, except when a dependency caused a bump above patch, in which case they SHALL go under that bump's section.

#### Scenario: Minor release with a dependency update
- **WHEN** `api` takes a minor release from its own changeset and its dependency `core` is updated to 2.0.0
- **THEN** `api`'s entry lists the minor change under Minor Changes and `core` 2.0.0 under Patch Changes

### Requirement: Final ranges after prereleases
When a unit releases a final version, `version` SHALL rewrite every dependent's range that names a prerelease of that unit to accept the final version, keeping the range's operator, so that no final release depends on a prerelease.

#### Scenario: Range written during prerelease mode
- **WHEN** prerelease mode wrote `ui`'s dependency on `web` as `^1.4.0-beta.0`, and after `pre exit` `web` releases 1.4.0
- **THEN** `ui`'s dependency on `web` becomes `^1.4.0`

### Requirement: Dry run
`version --dry-run` SHALL print the plan and the files it would change and SHALL write nothing.

#### Scenario: Nothing written
- **WHEN** `version --dry-run` runs with pending changesets
- **THEN** the working tree is unchanged and the output lists every unit's next version and every file `version` would write
