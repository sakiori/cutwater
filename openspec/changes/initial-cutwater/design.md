## Context

`cutwater` joins three established practices: Changesets' reviewed intent files, release-please's bot-maintained release request with a version manifest, and Maven's CI-friendly versions. The decisions below are the ones a reader cannot recover from the specifications alone.

## Decisions

### Changeset files, not commit messages

A version bump is a statement by the author, reviewed with the change it describes. A wrong bump is corrected by a later merge request that edits or deletes the pending changeset, and `check` shows reviewers such edits. Deriving versions from commit messages ties the release to message discipline and to history that is hard to amend. The file format is Changesets', so repositories using Changesets adopt the tool without touching their pending files. The `changesets` crate, which knope and Sampo also use, parses it.

Alternative: conventional commits, as release-please and semantic-release use. Rejected for the reasons above.

### Publication happens from the release commit

A version is published only from the commit that released it, in that commit's pipeline. This is the commit on the default branch's first-parent history that landed the release merge request, a merge commit where the project merges with merge commits, so what is published is the tree its pipeline checked. A failed publication is resumed by retrying the job in that pipeline. Publishing from any later default-branch commit would build later, unreviewed code under the released version number. A Maven build passes the version on the command line, so it would not even notice. release-please tags the release pull request's merge commit for the same reason.

The registry the unit deploys to decides whether a version is missing. A tag is created only after its unit, or its whole group, is published, so a tag together with its Release is proof of publication and spares later pipelines a registry query. Later pipelines fail only when a released version is missing from its registry, and only warn when it is published but untagged. Versions seeded by `init` are taken as released and never tagged, so adopting the tool does not fail every later pipeline. A fixed group that Changesets tagged per member gets one group tag from its next release on. Tags as the only source of truth would fail both ways: a tag created before publication names a version nobody can install, and a publication without a tag would be republished, which registries reject.

### One version manifest for every ecosystem

`.cutwater/versions.toml` holds the released version of every unit. It also identifies a unit's release commit: the commit that last changed the unit's entry. Maven needs the manifest, because its committed files hold snapshots and the released version has to live elsewhere. Keeping it for every ecosystem gives planning, `goal` and `publish` one source, and lets adapters that hold the version in their own files be checked against it. release-please keeps a manifest for the same reason. Group versions are not stored: a group's version is its members' common version.

Alternative: a manifest for Maven only. Rejected because the release commit would have no single definition and the core would treat one ecosystem differently.

### Maven: snapshots everywhere, released versions only in release builds

The committed `revision` of every unit is its next snapshot. A dependent refers to a producer through a version property that also holds the producer's snapshot, so a change to a producer and its dependents fits in one merge request and builds in one reactor. Release builds cover only the units the release publishes, one per Maven invocation in dependency order. Each build passes the released versions from the manifest on the command line and installs the unit into a job-local repository for the dependents that follow. Producers the release does not publish resolve from the deploy repository, so a dependent is built against what consumers will actually get. Command-line properties do not reach a deployed POM, so the flatten plugin must interpolate dependencies as well as `revision`. `goal` inspects the POMs that would be deployed for snapshots and unresolved properties. A released unit therefore never depends on a snapshot, and the default branch never carries a release version.

Alternative: commit the released version and add a second "snapshot bump" request, as release-please's Java strategy does. Rejected because between the two merges the default branch carries a release version. Every snapshot deployment from it then tries to redeploy a released version, which release repositories reject.

Alternative: commit the producer's released version in dependents. Rejected because a change that spans a producer and its dependents would then need two merge requests and a release in between.

Maven deployment is not atomic across a reactor. The adapter therefore asks the deploy repository about every module and deploys only the missing ones, so an interrupted deployment can be completed instead of failing on its first module. Maven Central's Portal publishes atomic bundles and Nexus staging repositories hide unreleased deployments. Both need their own target types and are out of scope here.

### The release branch is written through the API

`mr` writes the release commit through GitLab's commits API, with the default branch's tip, read after taking the resource group, as the start and `force` set, from the file actions that `version` produced in a scratch worktree. The runner never holds a push credential in a git remote, and nothing depends on its working copy. The API accepts requests far larger than a release commit. Files tracked by Git LFS are not written by `version`.

### Queued release requests are left alone, races are serialised

GitLab removes a merge request from a merge train when new commits arrive, and cancels auto-merge on a push. A merge queue bot treats a changed merge request as new. `mr` therefore does not touch a release merge request in any of these states. `mr` jobs run one at a time, oldest pipeline first, and each applies the plan to the default branch's tip as it reads it after taking the resource group, not to its own pipeline's commit. An older pipeline that runs last therefore still writes the newest release, and a newer pipeline that skips `mr` leaves nothing stale.

`goal` fails only on pending changesets for units the release publishes. A changeset for such a unit means unversioned code would enter its artifact. A changeset for any other unit is harmless and waits for the next release. If every pending changeset failed `goal`, a busy repository's merge train could keep dropping the release merge request.

### Credentials and GitLab tiers

`mr` needs a token whose commits start pipelines: a bot user's personal access token on GitLab.com Free, or a project access token where the tier provides one. `publish` creates tags and Releases with the bot token too. The CI job token would act with the rights of whoever merged, so whether a protected release tag can be created would depend on the person. `publish` checks the bot's right to create every tag before it publishes anything, because a failure after publication leaves an untagged release. The tool's guarantee that the checked tree lands needs fast-forward merges, which every tier has, or merged results pipelines or merge trains, which need Premium. `mr` reads the project's settings and warns when none of these holds.

### Rust, built-in adapters and repository checks

The tool serves several ecosystems, so it must not require the runtime of one of them: a Maven repository's CI should not need Node. A static binary installs anywhere, including minimal CI images. npm, Maven and Cargo are built in behind one internal interface with serializable inputs and outputs. A protocol for external adapters is left to a later change and will carry that interface unchanged. Repository-specific checks are plain commands that receive the units and the plan as JSON.

Alternative: TypeScript, with plugins as ESM modules loaded by path. Rejected because it ties every adopter to Node.

### Version policy of npm distribution tags

A final release above the registry's `latest` becomes `latest`, a lower one goes to `superseded`, and a prerelease goes to its label. Maintenance releases therefore never move `latest` backwards. npm also refuses to publish a prerelease without an explicit tag. A lower version is not refused: versions come only from the manifest, which only the plan raises, so a lower one is a deliberate maintenance release.

### GitLab client and configuration

The `gitlab` crate is the established Rust client for GitLab's API and sits behind the tool's forge interface, which keeps a later GitHub implementation independent of it. The configuration, the version manifest and the prerelease state share `.cutwater/`. Changeset files stay in `.changeset/`, where Changesets puts them. The configuration is TOML, as in Cargo, release-plz and knope, with a published JSON Schema for editors.

### Self-release

The tool releases itself through its own changesets and release merge request, with `vX.Y.Z` tags. Its `publish` job runs on GitLab.com's shared runners, because npm accepts trusted publishing only from there and crates.io only from GitLab.com. The first publication of each package uses a token, as both registries require, and trusted publishing after that.

## Risks and open points

- Whether a commit through the commits API to a merge request in a merge train behaves like a push needs confirming on a disposable project. So do the pipelines a fast-forward merge starts.
- Cargo keeps dev-dependencies that declare a version in the published manifest. Whether they constrain publication order needs confirming against `cargo publish --workspace`, which orders a workspace itself.
- The Maven adapter reads POMs without running Maven. Dependency versions that inheritance or profiles change are outside edge discovery, and integration tests against real Maven confirm the supported cases.
- GitHub as a forge, maintenance branches, external adapters, Maven Central's Portal and Maven 4 are not part of this change. The forge interface, the `superseded` distribution tag and the adapter interface keep each of them possible.
