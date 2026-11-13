## Why

Maven projects with several independently versioned units need what cutwater gives npm and Cargo units: reviewed changesets, one bot-maintained release merge request, and publication from the release commit in dependency order. Maven adds a constraint the other ecosystems do not have: the default branch must carry snapshots, because every snapshot deployment from it would otherwise try to redeploy a released version, which release repositories reject. The established tools either push release versions to the branch (the Maven release plugin), keep a release version on the branch between two requests (release-please's Java strategy), or publish static versions without a snapshot cycle (Sampo). The first cutwater release covers npm and Cargo; this change adds Maven.

## What changes

- **Maven units.** An aggregator POM whose version is `${revision}` is one unit, released with the modules that share its version. The adapter reads units and the edges between them from the POMs without running Maven, and supports Maven 3.9 or a later Maven 3 with the flatten plugin resolving `revision` and interpolating dependencies.
- **Snapshots on the default branch.** A unit's committed `revision` is always the snapshot of its next version, and a dependent refers to a producer through a version property kept equal to the producer's `revision`. `version` writes the next snapshots; the released versions live in the version manifest alone.
- **Adoption.** `init` sets the producer properties and seeds each unit from the highest final version its deploy repository holds.
- **Dependents follow their producers.** A deployed POM names one version of each dependency, which Maven's nearest-wins mediation gives its consumers, so a unit is released with a patch whenever a unit it depends on at run time, or whose version it manages as a BOM, is released.
- **Release builds and deployment.** A release builds only the units it publishes, one Maven invocation each in dependency order, with the released versions on the command line. `goal` refuses a deployed POM that carries a snapshot or an unresolved property, and `publish` deploys only the files the deploy repository lacks, so an interrupted deployment can be completed.

Maven Central's Portal, Nexus staging repositories and Maven 4 are out of scope: the first two publish atomically or hide unreleased deployments and need target types of their own.

## Capabilities

### New capabilities

None.

### Modified capabilities

- `ecosystem-adapters`: the Maven adapter.
- `release-intent`: a Maven aggregator with the modules that share its version is a unit.
- `release-versioning`: the first release of a new Maven unit, releases caused by versions a BOM manages, and the Maven dependencies that order publication.

## Impact

- **Repository files.** Maven units move to CI-friendly versions, `${revision}` with the flatten plugin, before adoption; dependents refer to producers through version properties.
- **CI.** The jobs that run `goal` and `publish` need Maven, and the deploy repository's credentials in Maven's settings.
