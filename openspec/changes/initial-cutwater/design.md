## Context

`cutwater` joins three established practices: Changesets' reviewed intent files, release-please's bot-maintained release request with a version manifest, and Maven's CI-friendly versions. The decisions below are the ones a reader cannot recover from the specifications alone.

## Decisions

### Changeset files, not commit messages

A version bump is a statement by the author, reviewed with the change it describes. A wrong bump is corrected by a later merge request that edits or deletes the pending changeset, and `check` shows reviewers such edits. Deriving versions from commit messages ties the release to message discipline and to history that is hard to amend. The file format is Changesets', so repositories using Changesets adopt the tool without touching their pending files. Like Changesets, the tool reads the front matter as YAML. The `changesets` crate that knope and Sampo use rejects empty front matter, keeps quotes in quoted names, and does not know `none`.

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

`mr` needs a token whose commits start pipelines: a bot user's personal access token on GitLab.com Free, or a project access token where the tier provides one. `publish` creates tags and Releases with the bot token too. The CI job token would act with the rights of whoever merged, so whether a protected release tag can be created would depend on the person. `publish` checks the bot's right to create every tag before it publishes anything, because a failure after publication leaves an untagged release. The tool's guarantee that the checked tree lands needs fast-forward merges or merge commits with semi-linear history, which every tier has and which both require the merge request to contain the target branch, or merged results pipelines or merge trains, which need Premium. `mr` reads the project's settings and warns when none of these holds.

The configuration alone names the bot token's variable, which the tool reads; the CI component takes no input for it, since a second name could disagree with the first. The release branch, by contrast, is an input of the component as well, because GitLab chooses a pipeline's jobs before any job can read the configuration. The component hands it to the tool, which refuses to run when the configuration names another branch, as the release merge request's pipeline would otherwise run `check` instead of `goal`.

The tool never logs in to a registry. crates.io's trusted publishing needs a token exchanged for the job's ID token before `cargo publish` runs, so the exchange, and the revocation after the publication, belong to the component, as an opt-in beside a token from the environment. npm exchanges its ID token itself.

### Rust, built-in adapters and repository checks

The tool serves several ecosystems, so it must not require the runtime of one of them: a Maven repository's CI should not need Node. A static binary installs anywhere, including minimal CI images. npm, Maven and Cargo are built in behind one internal interface with serializable inputs and outputs. A protocol for external adapters is left to a later change and will carry that interface unchanged. Repository-specific checks are plain commands that receive the units and the plan as JSON.

Alternative: TypeScript, with plugins as ESM modules loaded by path. Rejected because it ties every adopter to Node.

### Cargo crates are published one at a time

`publish` runs `cargo publish --locked` for one crate at a time, in the release graph's order, and asks the registry before and after each. A crate is therefore published against its dependencies as the registry holds them, and a retry continues where the last run stopped. `--locked` publishes the dependency versions of the reviewed `Cargo.lock`, and cargo's verification build stays on. Cargo resolves every dependency that names a version before it publishes a crate, development dependencies included, so these edges order publication too. A cycle through such a development dependency cannot be published by cargo in any order, inside a fixed group as well, and planning refuses it; dropping the development dependency's version breaks it.

Alternative: one `cargo publish` with every crate of the release selected, which since cargo 1.90 publishes the selected crates in its own order. Rejected because it reports one outcome for all of them: a failure could not hold back only the crates that depend on the failed one, and the tool could not ask the registry between crates, which counting a crate as published and continuing after a partial publication need.

### Version policy of npm distribution tags

A final release above the registry's `latest` becomes `latest`, a lower one goes to `superseded`, and a prerelease goes to its label. Maintenance releases therefore never move `latest` backwards. npm also refuses to publish a prerelease without an explicit tag. A lower version is not refused: versions come only from the manifest, which only the plan raises, so a lower one is a deliberate maintenance release.

### GitLab client and configuration

The `gitlab` crate is the established Rust client for GitLab's API and sits behind the tool's forge interface, which keeps a later GitHub implementation independent of it. The configuration, the version manifest and the prerelease state share `.cutwater/`. Changeset files stay in `.changeset/`, where Changesets puts them. The configuration is TOML, as in Cargo, release-plz and knope, with a published JSON Schema for editors.

### Self-release

The tool releases itself through its own changesets and release merge request, with `X.Y.Z` tags, plain semantic versions, so that a project includes its CI component by the version itself. Its `publish` job runs on GitLab.com's shared runners, because npm accepts trusted publishing only from there and crates.io only from GitLab.com. The first publication of each package uses a token, as both registries require, and trusted publishing after that.

The CI component installs the release of its own version. It cannot pin that release's checksums: the tag that versions the component sits on the release commit, and the archives are built from that commit, so their checksums cannot be part of it. The release pipeline therefore signs the archives' `SHA256SUMS` keylessly with cosign, with the GitLab ID token of the default branch's pipeline, and the component verifies that signature against the release pipeline's identity before it checks the archive against `SHA256SUMS`. Only cosign itself is pinned by checksum in the component.

Alternative: record the checksums in the component in a commit after the release. Rejected because the tag would then not version the component that pins its own archives, and every release would need a second merge request.

GitLab's component context names a catalog version only for releases of a CI/CD Catalog resource, which the catalog takes only from releases a CI job creates with the `release` keyword, not through the Releases API. The tool creates its releases through the API, so repositories include the component by an exact release tag, `X.Y.Z`, from which the component reads its version when the context has none. Partial versions and `~latest` are not available that way. Publishing to the catalog otherwise takes `glab repo publish catalog`, which is experimental and behind a feature flag.

Release files are built and signed only in the pipeline of the push of a release commit, which `released` names without GitLab or a registry. GitLab chooses a pipeline's jobs when it creates the pipeline, before any job runs, so the build and signing jobs read the tool's answer when they start and end at once without a release. Their rules create them only for a push that changes the version manifest, which every release commit's push does, so that the macOS runner starts no virtual machine for pushes that release nothing. Only that pipeline stages the release files for publication. Any other pipeline of the default branch, a later commit's or one scheduled or started by hand on the release commit, builds, signs and stages nothing; its `publish` passes where the release's tag and Release exist, and fails where the release is unpublished, because the files its assets name are missing.

Alternative: build and sign in a dynamic child pipeline that the tool's answer generates. Rejected because a parent pipeline cannot take a child pipeline's artifacts, which publication needs, and GitLab does not support Sigstore signing with the ID tokens of child pipelines.

Sigstore's ID token reaches two jobs: the signing job of the release commit's push pipeline, which runs no build code, and, with npm's trusted publishing, the publication job of every pipeline of the default branch, which needs it for npm's provenance and also runs `cargo publish`'s verification build, with the build scripts and procedural macros of the dependencies, and npm's lifecycle scripts. Fulcio's certificates name the pipeline's configuration and ref, not the job, and cosign verifies only that identity and the issuer, so a signature the publication job made verifies as the signing job's. Fulcio records the runner environment, the build trigger, the job's URL and the deployment environment as certificate extensions, which would tell the jobs apart, but cosign has no option to require them, and checking them would take a certificate parser in every image that runs the component. The residual risk is accepted: a dependency compromised at build time controls the signed executables through the build already; the publication job adds only a signature over other content, made in a pipeline of the default branch. npm's provenance is kept.

The tool's own pipeline includes the component from its own commit, as GitLab documents for testing a component in its project, so that the component is used exactly as other projects use it. Its jobs run the executable that the pipeline builds from that commit instead of a downloaded release: the release commit's code publishes itself, and the first release has nothing to download.

## Risks and open points

- Whether a commit through the commits API to a merge request in a merge train behaves like a push needs confirming on a disposable project. So do the pipelines a fast-forward merge starts.
- The Maven adapter reads POMs without running Maven. Dependency versions that inheritance or profiles change are outside edge discovery, and integration tests against real Maven confirm the supported cases.
- GitHub as a forge, maintenance branches, external adapters, Maven Central's Portal and Maven 4 are not part of this change. The forge interface, the `superseded` distribution tag and the adapter interface keep each of them possible.
