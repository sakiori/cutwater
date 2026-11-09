## ADDED Requirements

### Requirement: Release commit
A unit's release commit SHALL be the commit on the default branch's first-parent history whose change from its first parent last changed the unit's entry in the version manifest. `publish` SHALL build, publish and tag a unit only in a pipeline of its release commit, so that what is published is the tree the release merge request's pipeline checked.

#### Scenario: Release merge request merged
- **WHEN** the release merge request that raised `core` to 2.0.0 and `api` to 1.4.3 lands as commit R and `publish` runs in R's pipeline
- **THEN** `core` 2.0.0 is published before `api` 1.4.3, and both tags point at R

#### Scenario: Merge commit
- **WHEN** the project merges with merge commits and the release merge request lands as merge commit M
- **THEN** M is the release commit, not the bot's commit on the release branch, and `publish` runs in M's pipeline

### Requirement: Unfinished release in a later pipeline
In a pipeline of any other commit, `publish` SHALL publish and tag nothing for a released unit. It SHALL fail when the unit's registry lacks the released version and the release commit's pipeline has finished, and SHALL only warn while that pipeline still runs or when the version is published but untagged, naming the unit, its release commit and the pipeline whose `publish` job to retry.

#### Scenario: Retry after a later merge
- **WHEN** `api` 1.4.3 failed to publish in R's finished pipeline, merge request B landed after R, and `publish` runs in B's pipeline
- **THEN** it publishes nothing for `api`, fails, and says to retry the `publish` job of R's pipeline

#### Scenario: Release pipeline still running
- **WHEN** B's pipeline reaches `publish` while R's pipeline has not yet run its own `publish`
- **THEN** B's `publish` warns about `api` and succeeds

### Requirement: The deploy target decides
`publish` SHALL decide whether a version is missing by asking the registry the unit deploys to, through the ecosystem's own tool and the credentials the environment configured, not by a public registry of the same name. A tag together with its Release SHALL count as proof of publication without asking the registry.

#### Scenario: Private registry with a public namesake
- **WHEN** a unit deploys to a private registry and a public registry has a package of the same name and version
- **THEN** `publish` asks the private registry and publishes when it lacks the version

### Requirement: Repeatable publication
A repeated `publish` SHALL publish only what is still missing and tag only what is complete. A registry's answer that the version already exists SHALL count as published once the registry lists it. After publishing, the tool SHALL wait, for a configurable time, for the registry to list the version before it counts the unit as published. It SHALL never replace or unpublish a version.

#### Scenario: Retry after the first unit
- **WHEN** `publish` published `core` and failed on `api`, and its job is retried
- **THEN** it does not publish `core` again, publishes `api`, and then tags both

#### Scenario: Slow registry
- **WHEN** the registry accepts `core` 2.0.0 but lists it only some seconds later
- **THEN** `publish` waits for it to appear before tagging, and fails after the configured time if it never does

### Requirement: Tags and Releases
After a unit is published, `publish` SHALL tag the release commit from the unit's tag template and create its GitLab Release, with the version's changelog entry as notes. A fixed group SHALL get one tag and one Release once all its members are published, with every member's entry in the notes, and none while any member is unpublished.

#### Scenario: Group partly published
- **WHEN** a fixed group's first member was published and the second failed
- **THEN** the group has no tag and no Release until a retry publishes the second member

### Requirement: Existing tags
A tag that exists on the release commit without a Release SHALL get its Release. A tag that exists on another commit SHALL make `publish` fail and name it. Tags SHALL never be moved or deleted.

#### Scenario: Tag left without a Release
- **WHEN** an earlier run created `api-v1.4.3` and failed before the Release, and the job is retried
- **THEN** the Release of `api-v1.4.3` is created with the 1.4.3 changelog entry

### Requirement: Release assets
A unit MAY list release asset patterns. `publish` SHALL upload the matching files, which CI built from the release commit, to the project's generic package registry under the unit's registry name and version before tagging, and link each from the Release. A file of the same name with another checksum SHALL make `publish` fail.

#### Scenario: Binaries of a command-line tool
- **WHEN** a unit lists `dist/*.tar.gz` and CI built four archives before `publish`
- **THEN** the four archives are in the generic package registry under the unit's version and its Release links all four

### Requirement: Registry names of assets
A unit's registry name SHALL be its name with characters the generic package registry rejects replaced by `-`, unless the unit sets one. Two units with the same registry name SHALL make every command refuse.

#### Scenario: Colliding scoped names
- **WHEN** the units `@a/b-c` and `@a-b/c` both map to the same registry name
- **THEN** every command refuses and names both units

### Requirement: Record of a run
`publish --json` SHALL print what it published, uploaded and tagged, and what it found already present, so that later CI jobs, such as image builds, can act on it.

#### Scenario: Image build after publication
- **WHEN** `publish --json` published `api` 1.4.3
- **THEN** its output names `api`, 1.4.3, the registry and the tag `api-v1.4.3`

### Requirement: Forge access for publication
`publish` SHALL create tags and Releases with the bot token, so that the right to create protected release tags does not depend on who merged. Before publishing anything, it SHALL check that the bot may create every tag it is about to create, and SHALL fail naming the tag and the protection rule when it may not.

#### Scenario: Bot not allowed to create the tag
- **WHEN** release tags are protected for an access level the bot does not have
- **THEN** `publish` fails before publishing anything and names the tag pattern and the bot's access level

### Requirement: Dry run
`publish --dry-run` SHALL report what it would publish, upload and tag, and SHALL change nothing in any registry or on the forge.

#### Scenario: Nothing changed
- **WHEN** `publish --dry-run` runs with two missing versions
- **THEN** it lists both units and their tags, and no registry or forge receives a write
