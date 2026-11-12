## Context

The first cutwater release covers npm and Cargo. Maven brings what the other ecosystems lack: snapshots on the default branch, versions passed on the command line, and deployments that are not atomic across a reactor. Maven's CI-friendly versions, `${revision}` with the flatten plugin, are the established practice this change builds on. The version manifest that every ecosystem shares already exists for this reason: a Maven unit's committed files hold its next snapshot, so its released version lives in the manifest.

## Decisions

### Snapshots everywhere, released versions only in release builds

The committed `revision` of every unit is its next snapshot. A dependent refers to a producer through a version property that also holds the producer's snapshot, so a change to a producer and its dependents fits in one merge request and builds in one reactor. Release builds cover only the units the release publishes, one per Maven invocation in dependency order. Each build passes the released versions from the manifest on the command line and installs the unit into a job-local repository for the dependents that follow. Producers the release does not publish resolve from the deploy repository, so a dependent is built against what consumers will actually get. Command-line properties do not reach a deployed POM, so the flatten plugin must interpolate dependencies as well as `revision`. `goal` inspects the POMs that would be deployed for snapshots and unresolved properties. A released unit therefore never depends on a snapshot, and the default branch never carries a release version.

Alternative: commit the released version and add a second "snapshot bump" request, as release-please's Java strategy does. Rejected because between the two merges the default branch carries a release version. Every snapshot deployment from it then tries to redeploy a released version, which release repositories reject.

Alternative: commit the producer's released version in dependents. Rejected because a change that spans a producer and its dependents would then need two merge requests and a release in between.

Maven deployment is not atomic across a reactor. The adapter therefore asks the deploy repository about every module and deploys only the missing ones, so an interrupted deployment can be completed instead of failing on its first module. Maven Central's Portal publishes atomic bundles and Nexus staging repositories hide unreleased deployments. Both need their own target types and are out of scope here.

### Release commits and command-line versions

Publication happens from the release commit, as for every ecosystem. For Maven this matters more: a Maven build passes the version on the command line, so a build of a later commit under the released version would not even notice that it builds unreviewed code.

## Risks and open points

- The Maven adapter reads POMs without running Maven. Dependency versions that inheritance or profiles change are outside edge discovery, and integration tests against real Maven confirm the supported cases.
