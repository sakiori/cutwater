## ADDED Requirements

### Requirement: Version plan
The next version of a unit SHALL follow the highest bump among the pending changesets that name it. A fixed group SHALL take the highest next version among its members, and every member SHALL take that version. The plan SHALL repeat these rules, and the dependency rule, until no unit changes.

#### Scenario: Highest bump wins
- **WHEN** `api` at 1.4.2 has one pending patch changeset and one pending minor changeset
- **THEN** the plan releases `api` at 1.5.0

#### Scenario: Fixed group
- **WHEN** `web` and `ui` form a fixed group at 2.1.0 and only `ui` has a pending minor changeset
- **THEN** the plan releases both at 2.2.0

### Requirement: Releases caused by dependencies
A unit SHALL take a patch bump when a unit it depends on through a runtime, peer or optional dependency is released at a version outside the range the dependent declares, judged by the ecosystem's own range rules. Development dependencies SHALL not cause a release. The changelog entry of such a release SHALL name the dependencies and their new versions.

#### Scenario: Range excludes the new major
- **WHEN** `api` depends on `core` with `^1.2.0` and `core` is released at 2.0.0
- **THEN** the plan releases `api` with a patch bump and its entry says that `core` was updated to 2.0.0

#### Scenario: Development dependency
- **WHEN** `api` depends on `core` only as a development dependency and `core` takes a major release
- **THEN** the plan does not release `api`

### Requirement: Release graph
The release graph's edges SHALL be the runtime and optional dependencies between units. It SHALL be acyclic, ignoring edges between members of the same fixed group. A cycle SHALL make every command that plans refuse and list the edges of the cycle. Publication SHALL follow the graph's topological order, and members of a fixed group SHALL be ordered by their own edges.

#### Scenario: Cycle between two units
- **WHEN** `a` depends on `b` at runtime and `b` depends on `a` at runtime, in different groups
- **THEN** `status`, `version` and `mr` refuse and list the edges `a → b` and `b → a`

#### Scenario: Order inside a group
- **WHEN** `web` and `ui` form a fixed group and `web` depends on `ui`
- **THEN** `ui` is published before `web`

### Requirement: Version manifest
The tool SHALL keep `.cutwater/versions.toml` with the released version of every unit. Only `init` and `version` SHALL write it. Where an adapter's files also hold the released version, `check` SHALL fail when the two disagree, naming the unit and both versions.

#### Scenario: Manual version edit
- **WHEN** a merge request changes the `version` of an npm unit's `package.json` without changing the manifest
- **THEN** `check` fails, names the unit, the manifest's version and the package file's version

### Requirement: New and removed units
A unit without a manifest entry SHALL be new and SHALL be released only once a changeset names it with a bump other than `none`. Its first release SHALL be the version its adapter's files declare, for Maven the `revision` without `-SNAPSHOT`, and the changesets naming it SHALL only provide its changelog entry. `version` SHALL remove the entry of a unit that no longer exists; renaming is a removal and an addition, and existing tags stay.

#### Scenario: First release of a new package
- **WHEN** a merge request adds the npm package `export` at version 0.1.0 with a changeset, and the release merge request lands
- **THEN** the manifest holds `export` 0.1.0 and `publish` releases it at 0.1.0

### Requirement: Applying the plan
`version` SHALL apply the plan to the working tree: write each released unit's version into the manifest and through its adapter, update the references of dependents through their adapter, refresh the lockfiles the adapters own, and delete the applied changesets. With nothing pending, it SHALL leave the working tree unchanged.

#### Scenario: Applied release
- **WHEN** `version` runs with one pending minor changeset for `api` at 1.4.2
- **THEN** `api`'s files and the manifest hold 1.5.0, the workspace lockfile names 1.5.0, and the changeset file is gone

### Requirement: Changelogs
`version` SHALL prepend each released unit's entry to its `CHANGELOG.md`. By default an entry SHALL be headed by the version, group the changeset entries by bump, and name no commit hashes. A fixed group's members SHALL each get their own entry.

#### Scenario: Entry of a minor release
- **WHEN** `version` releases `api` 1.5.0 from one minor and one patch changeset
- **THEN** `api/CHANGELOG.md` starts with a 1.5.0 entry listing the minor change before the patch change

### Requirement: Prerelease mode
`pre enter <label>` SHALL record prerelease mode in `.cutwater/pre.toml`. While it is recorded, every release SHALL be `<next>-<label>.<n>`, counting `n` from 0 per unit and next version, including releases caused by dependencies. `version` SHALL delete applied changeset files as usual and record their entries in `.cutwater/pre.toml`, so that the final release's changelog carries all of them. `pre exit` SHALL make the next release final. Both SHALL be merged through a merge request.

#### Scenario: Prerelease sequence
- **WHEN** `api` is at 1.4.2, `pre enter beta` is merged, and two minor changesets are applied by two releases
- **THEN** the releases are 1.5.0-beta.0 and 1.5.0-beta.1, and after `pre exit` the next release is 1.5.0 with both entries in its changelog

### Requirement: Dry run
`version --dry-run` SHALL print the plan and the files it would change and SHALL write nothing.

#### Scenario: Nothing written
- **WHEN** `version --dry-run` runs with pending changesets
- **THEN** the working tree is unchanged and the output lists every unit's next version and every file `version` would write
