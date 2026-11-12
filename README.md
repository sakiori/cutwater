# cutwater

`cutwater` releases the packages of a repository through one merge request that a bot keeps up to date. Authors state each change's version bump in changeset files, reviewed with the change. Merging the release merge request publishes every package whose version is missing from its registry, then tags it and creates the GitLab Release.

It is meant to cover npm (pnpm and npm workspaces, single packages), Maven (multi-module projects with independently versioned units, CI-friendly `${revision}` versions) and Cargo, on GitLab. `openspec/` holds the specification.

## What works now

The settings live in `.cutwater/config.toml`; [`schema/config.schema.json`](schema/config.schema.json) describes them for editors, and [`schema/versions.schema.json`](schema/versions.schema.json) the version manifest. A repository whose publishable npm packages and Cargo crates are its units needs only:

```toml
[npm]
discover = true

[cargo]
discover = true
```

A unit ships every file under its directory, or the files its `shipped` patterns select, but never the files of a unit nested under it. Releases are tagged `v{version}` when the repository has one unit, or one fixed group of every unit, and `{unit}-v{version}` or `{group}-v{version}` otherwise; the `tag` settings of units, adapters and groups change that.

- `cutwater init` sets a repository up: it turns on discovery for the npm packages and Cargo crates it finds and asks each one's registry, through `npm view` or `cargo info` with the repository's and the environment's settings, whether it lists the declared version. A listed version goes into `.cutwater/versions.toml`, the version manifest, as seeded; for any other answer `init` refuses, because an unreachable, replaced or denying registry looks like one without the package. `cutwater init --new <unit>` declares a unit that was never published, which gets no manifest entry, and `cutwater init --published <unit>` seeds a declared version the registry does not list, such as a yanked one, without asking. `init` writes every tag template it relies on into the configuration. In a repository on Changesets it imports the fixed groups, ignored packages and base branch of `.changeset/config.json`, names the settings it does not carry over, keeps Changesets' tag format for the npm units (`{unit}@{version}` in a workspace, `v{version}` for a single package), and leaves the pending changesets in place; it refuses while Changesets is in prerelease mode or declares linked groups. Crates that inherit `workspace.package.version` become one fixed group.
- `cutwater add` creates a changeset file in `.changeset/`, in the format of [Changesets](https://github.com/changesets/changesets). In a terminal it asks for the units, their bumps and the entry, with the units the branch changed preselected. `cutwater add --unit api:minor --message "Add the export endpoint"` does the same without asking, and `cutwater add --empty` records that a change releases nothing.
- `cutwater check` fails a merge request whose commits change a unit's shipped files without a changeset for that unit, and lists the merge request's edits and deletions of other merge requests' pending changesets. In a merge request pipeline it compares HEAD with the merge base of the merge request's diff base, and in a merged results or merge train pipeline with the target branch commit the pipeline merged onto; elsewhere, pass `--base <commit>`. The clone must reach that commit, so raise `GIT_DEPTH` if it is shallow. It also fails when a unit's `package.json` or `Cargo.toml` holds another version than the manifest, or when a merge request other than the release merge request changes the manifest, because only `cutwater version` changes released versions; and when the tree cannot be planned, for a dependency cycle or units that share a version outside a fixed group.
- `cutwater status` lists the pending changesets and every release they make: the unit's current and next version, and why it is released, by its own changesets, its fixed group, or a dependency released at a version outside the range the unit declares. A unit removed from the repository leaves the manifest with the next release. `cutwater status --json` prints the same as JSON.
- `cutwater version` applies the pending changesets to the working tree. Each unit takes the highest bump its changesets name, and a fixed group the highest version among its members; a new member of a group releases with it. A unit whose dependency is released outside the range it declares, as npm or Cargo judges it, takes a patch release; for a peer dependency whose minor or major release leaves the range, a major release, since its rewritten peer range drops the versions its consumers install. Units that share one written version, such as crates inheriting `workspace.package.version`, must form a fixed group. It writes the versions into the manifest, `package.json` and `Cargo.toml` (once, in `workspace.package`, for crates that inherit it), rewrites dependents' ranges that no longer accept the new versions, keeping their operator, prepends each released unit's `CHANGELOG.md` in the layout of Changesets, titled with the package's name, refreshes `pnpm-lock.yaml`, `package-lock.json` and `Cargo.lock` with pnpm, npm and cargo, and deletes the changesets; when a step fails, it puts every file back. `workspace:*`, `workspace:^` and `workspace:~` follow the new versions by themselves; an explicit range such as `workspace:~1.2.0` is rewritten like any other, and a pnpm catalog range in `pnpm-workspace.yaml`. Yarn and Bun workspaces are refused. A new unit's first release is the version its files declare. `cutwater version --dry-run` prints the plan and the files it would change, and changes nothing.
- `cutwater pre enter <label>` records prerelease mode in `.cutwater/pre.toml`: releases become `<version>-<label>.<n>`, and the prereleases' changelog entries and dependency updates are kept for the final release, which also rewrites the ranges that name prereleases. `cutwater pre exit` makes the next release final. Merge both like any other change.
- `cutwater mr` runs in pipelines of the default branch and keeps the release merge request. It reads the default branch's tip through GitLab's API, fetches it from `origin` when the clone lacks it, and applies the pending changesets to it as `cutwater version` does, in a scratch working tree. It writes the result, with a file written through a symbolic link committed as the link's target and a link out of the repository refused, as one commit on top of the tip onto the release branch (`release-branch`, `cutwater/release` by default) through the commits API, replacing what the branch held, and creates or updates the merge request into the default branch, titled `release-title` (`Release` by default), whose description lists every released unit's next version and changelog entry, once under the names of all units whose releases are the same, as a fixed group's members' often are. It leaves the release branch as it is when its commit, which it fetches from `origin` like the tip, makes the same changes on top of the tip, or on top of an older commit where the project merges with merge commits, so that an unchanged release keeps its pipeline and approvals. It leaves a merge request alone, and says so, while it is set to merge automatically, is in a merge train, or is assigned to one of the merge queue accounts `gitlab.merge-queue` names; the first run after it leaves the queue updates it. With nothing pending, it closes the merge request with a note. It warns, in its output and in the description, when the project merges with merge commits without merged results pipelines or merge trains, because the merge request's pipeline then does not check the tree that lands. It never merges, approves, assigns or queues a merge request. `cutwater mr --dry-run` says what it would write and changes nothing on GitLab.
- `mr` and `publish` authenticate with the token in `CUTWATER_GITLAB_TOKEN`, or in the variable `gitlab.token-variable` names: a personal access token of a bot user, as GitLab.com Free requires, or a project access token, with the `api` scope and the right to push to the release branch, kept in a masked and protected CI/CD variable. They refuse the CI job token: commits made with it start no pipelines, and it acts with the rights of whoever started the pipeline. They reach the instance at `CI_SERVER_URL` and the project `CI_PROJECT_ID`, both set in GitLab CI, and also trust the certificates of `CI_SERVER_TLS_CA_FILE` where the runner sets it. The token never appears in the tool's output or errors.
- `cutwater goal` checks the release merge request in its pipeline. The units it releases are those whose manifest entry it changes against the target branch commit, found as `check` finds its base, or `--base <commit>`. It fails when a pending changeset names one of them with a bump, since its change would ship under the released version without it; a changeset for any other unit waits for the next release. It also fails when the manifest and the units' files disagree, and when what would be published fails the adapter's checks. The npm adapter packs each released package with pnpm or npm as publishing does, lifecycle scripts included, so pnpm needs the workspace installed, and checks that the packed `package.json` carries the released version, no `workspace:` or `catalog:` reference, and ranges that accept the released versions of the units it depends on. The Cargo adapter runs `cargo package` for the released crates together, which builds each packaged crate; when one released crate refers to another with a version, as a dependency of any kind, this needs cargo 1.90 or newer, the first to package crates of a workspace against each other's new versions, and `goal` refuses an older one.
- `cutwater publish` runs in pipelines of the default branch and publishes each unit from its release commit: the commit on the default branch's first-parent history that last changed the unit's entry in `.cutwater/versions.toml`, which is the merge commit of the release merge request where the project merges with merge commits. In a shallow clone it deepens the history from `origin` until it reaches the release commits. In the release commit's pipeline it asks each unit's registry, through `npm view` or `cargo info` with the repository's and the environment's settings, whether it holds the version, and publishes what is missing in dependency order: an npm package is packed as `goal` packs it and its archive published with `npm publish`, under its prerelease label as the distribution tag, as `latest` when it is above the registry's `latest`, and as `superseded` when it is below; a crate is published with `cargo publish --locked`, one at a time, after every crate it refers to with a version, development dependencies included. It then waits until the registry lists the version, for at most `publish.registry-timeout` seconds (600 by default). A unit whose publication fails holds back the units that depend on it, as does a dependency whose earlier release is missing from its registry, and the job fails; retrying it publishes only what is still missing. Once a unit, or every member of a fixed group, is published, it uploads the unit's release assets and tags the release commit with the unit's or the group's tag, creating the GitLab release, whose notes are the version's entry in each member's `CHANGELOG.md`, each distinct entry once under the names of the members that have it. A tag that exists on the release commit without a release gets its release; a tag on another commit fails the job, as tags are never moved. Before it publishes anything, it checks that the bot may create every tag it is about to create or release, given the project's protected tags and the bot's role, as GitLab requires that right for a release of a protected tag too; as in GitLab, a rule that names a group lets the bot create tags only where the project is shared with that very group with the Developer role or higher and the bot is a member of that group itself, not through a group above it, with the Developer role or higher. In the pipeline of any later commit it publishes and tags nothing, but fails when a released version is missing from its registry after its release commit's pipeline finished, and warns while that pipeline still runs or when the version is published but untagged, naming the pipeline whose `publish` job to retry. A tag with its release counts as proof of publication without asking the registry, and versions seeded by `init` are neither published nor tagged. `cutwater publish --dry-run` says what it would publish, upload and tag, and writes to no registry and not to GitLab; `cutwater publish --json` prints the record of the run, every unit with its version, registry, tag and whether it was published or found present, and every asset and tag, for later jobs to act on.
- `cutwater released` names the units whose release commit is HEAD, which the pipeline of HEAD publishes, one per line with its version and adapter, or with `--json` with their tags too. It reaches neither GitLab nor a registry, so CI can decide early whether a pipeline builds and signs release files.
- Release assets are files that CI builds before `publish`, which a unit lists as glob patterns relative to the repository root:

  ```toml
  [[unit]]
  name = "@acme/cli"
  assets = ["dist/*.tar.gz"]
  ```

  `publish` uploads them to the project's generic package registry, under the unit's registry name and version, and links each from the release under its file name, as tools that install from releases expect, so that the members of a fixed group may not have assets of the same name; a file of the same name already there is kept when its checksum matches and fails the job when it does not. Every pattern must match a file. The registry name is the unit's name with every character but letters, digits, `.`, `_`, `-` and `+` replaced by `-`, `-acme-cli` here, unless the unit sets `registry-name`; two units with the same registry name make every command refuse.
- `publish` creates tags, releases and packages with the bot token `mr` uses, which needs at least the Developer role and the right to create the release tags where they are protected, so that what it may do does not depend on who merged. Registry credentials stay with the environment: `cutwater` never logs in. npm takes a token or trusted publishing, with provenance, from its own configuration and the job's environment, which `cutwater` passes on unchanged; cargo takes the registry's token from the environment, `CARGO_REGISTRY_TOKEN` or `CARGO_REGISTRIES_<NAME>_TOKEN`, which `cutwater` passes to cargo alone, never to npm, pnpm, lifecycle scripts or repository checks.
- Repository checks are commands that `check`, `goal` or both run after their own checks, each in the repository root:

  ```toml
  [[check]]
  name = "exports"
  command = ["node", "scripts/check-exports.mjs"]
  steps = ["goal"]
  ```

  A check reads JSON on standard input: `step`, `units` (each with `name`, `path`, `adapter`, `group`, the `released` version of the manifest and the `declared` version of its files), and `plan.releases` (each with `unit`, `adapter`, `current` and `next`), which in `check` are the releases the pending changesets make and in `goal` those the release merge request makes. What it prints appears in the step's output, and a non-zero exit fails the step.

`cutwater` runs the installed git, 2.15 or newer. Cargo units need `cargo metadata`, so cargo must be on the `PATH` where the repository has them, and `cutwater version` runs the tool of each lockfile it refreshes. Through rustup, that cargo honours the repository's `rust-toolchain.toml` and may first install the toolchain it names; a cargo older than the workspace needs makes `cutwater` exit with 2.

`cutwater` exits with 1 when a rule is violated, such as a change without a changeset, and with 2 when it cannot reach a verdict: an invalid configuration or changeset, a failing git, or a missing tool.

## The GitLab CI/CD component

The component `release` runs the commands in the pipelines they belong to: `cutwater-check` runs `cutwater check` in every merge request pipeline but the release merge request's, `cutwater-goal` runs `cutwater goal` in the release merge request's pipelines, and `cutwater-mr` and `cutwater-publish` run `cutwater mr` and `cutwater publish` in every pipeline of the default branch. It needs GitLab 18.7 or newer. Include it by a release tag of cutwater, a plain version such as `1.2.3`:

```yaml
include:
  - component: gitlab.com/sakiori-oss/cutwater/release@1.2.3
    inputs:
      image: node:24-trixie
      setup:
        - corepack enable
      prepare:
        - pnpm install --frozen-lockfile
```

The component is not in the CI/CD Catalog, so partial versions such as `@1.2` and `~latest` do not resolve: GitLab's documentation requires a catalog release to be created by the `release` keyword in a CI job, not through the Releases API, through which cutwater creates its releases, and the only other way, `glab repo publish catalog`, is experimental and behind a feature flag.

Every job installs the cutwater release that the tag names: it downloads the archive of the runner's platform, `SHA256SUMS` and `SHA256SUMS.sigstore.json`, verifies with cosign, pinned by version and checksum in the component, that cutwater's release pipeline signed `SHA256SUMS`, and checks the archive against it; it fails before running anything when either check fails. The tool lives outside the project directory, so no package takes it in. A component included by a branch or a commit has no release to install and fails, unless `binary` names an executable the pipeline built, as cutwater's own pipeline does.

The inputs:

- `image`, required: the image the jobs run in. It needs a POSIX shell, git 2.15 or newer, curl, tar, gzip, `sha256sum` or `shasum`, and the tools of the repository's units: cargo for Cargo units, npm or pnpm for npm units, as `cutwater version` refreshes lockfiles and `goal` packs with them.
- `setup` (none): commands every job runs after installing cutwater, such as `corepack enable` or installing a Rust toolchain.
- `prepare` (none): commands `cutwater-goal` and `cutwater-publish` run after `setup` and before the tool: registry credentials, the installation of a pnpm workspace, which packing with lifecycle scripts needs, and files that earlier jobs built for the packages.
- `release-branch` (`cutwater/release`): the release merge request's branch, which `release-branch` in `.cutwater/config.toml` must name too; every job refuses to run when they differ.
- `check-stage`, `goal-stage` (`test`), `mr-stage`, `publish-stage` (`deploy`): the jobs' stages. `cutwater-check` and `cutwater-mr` start at once; `cutwater-goal` and `cutwater-publish` wait for the jobs of earlier stages and take their artifacts.
- `publish-tags` (none): the runner tags of `cutwater-publish`, in place of the pipeline's default tags; without tags it runs untagged. npm's trusted publishing works only on GitLab.com's shared runners, such as `saas-linux-small-amd64`.
- `environment` (`cutwater-publish`) and `mr-environment` (`cutwater-mr`): the environments `cutwater-publish` and `cutwater-mr` prepare, without deploying. CI/CD variables scoped to them reach these jobs alone: the bot token both, the registry tokens `cutwater-publish`; trusted publishers can name `cutwater-publish`.
- `npm-auth` (`token`): `token` leaves npm's authentication to its configuration and the environment, which `prepare` can write; `trusted-publishing` gives `cutwater-publish` the ID token npm's trusted publishing exchanges, `NPM_ID_TOKEN`, and the one its provenance needs, `SIGSTORE_ID_TOKEN`.
- `crates-io-auth` (`token`): `token` leaves cargo the token in `CARGO_REGISTRY_TOKEN`; `trusted-publishing` exchanges the job's ID token, `CRATES_IO_ID_TOKEN`, for a crates.io token at crates.io's trusted publishing endpoint when the commit releases a Cargo unit, as `cutwater released` names it, gives it to `cutwater publish` alone, which passes it to cargo alone, and revokes it once that ends, or in `after_script` when the job is canceled; a job that times out revokes nothing, and the token expires 30 minutes after the exchange. A set `CARGO_REGISTRY_TOKEN`, as the first publication of a crate needs, takes its place, with a warning. crates.io trusts pipelines of GitLab.com only.
- `download-url` (the generic package registry of `gitlab.com/sakiori-oss/cutwater`): where the releases lie, each in `<version>/`.
- `cosign-url` (cosign's GitHub releases): where cosign's releases lie, each in `v<version>/`.
- `trusted-root` (none): a Sigstore trusted root file in the project, for runners that cannot reach Sigstore's public trust root.
- `binary` (none): an executable the pipeline built, relative to the project directory, which the jobs run instead of a release.

A self-managed instance that mirrors the component mirrors the releases with it, in the same layout, and points `download-url` at them, and `cosign-url` at a copy of cosign's releases when its runners cannot reach GitHub. Where they cannot reach Sigstore either, `cosign trusted-root` or Sigstore's TUF repository gives the trusted root, which the project commits and names in `trusted-root`. The signature stays that of cutwater's release pipeline on GitLab.com.

`cutwater-check` and `cutwater-goal` clone the whole history, as they compare with the merge request's base; the other jobs take the project's clone depth, and `cutwater-publish` deepens it as far as the release commits lie. Any job's keywords can be overridden by a job of the same name in the including pipeline, such as `needs`, which the component leaves to the stages.

The project needs:

- the bot's token in the CI/CD variable `gitlab.token-variable` names, `CUTWATER_GITLAB_TOKEN` by default, masked and protected, with the default branch protected so that its pipelines receive it, and scoped to the environments `cutwater-mr` and `cutwater-publish`, with the scope `cutwater-*` or once for each, so that no other job of a pipeline, a test job or a repository's build script, receives it; registry tokens scoped to `cutwater-publish` alike;
- default branch pipelines that are never cancelled automatically, because the release commit's pipeline is the only one that publishes its release: `workflow: auto_cancel: on_new_commit: none` in the pipeline, or _Auto-cancel redundant pipelines_ off in the project's CI/CD settings;
- the release tags protected, so that only the bot creates them where the tier lets a protection rule name a user, and otherwise for the Maintainer role, which the bot then needs;
- a merge method under which the release merge request's pipeline checks the tree that lands: fast-forward merges, merge commits with semi-linear history, merged results pipelines or merge trains;
- the release branch protected, so that only the bot pushes and force pushes it where the tier lets a protection rule name a user, and otherwise the Maintainer role, which the bot then needs, with merging into it allowed to no one;
- duplicate generic packages refused, so that a later upload cannot shadow a release's file: in the group's _Settings_ > _Packages and registries_, _Duplicate packages_, turn off _Allow duplicates_ in the _Generic_ row, which needs the group's Owner role;
- the resource groups `cutwater-mr` and `cutwater-publish` set to process the oldest pipeline first, through the API once they exist: `PUT /projects/:id/resource_groups/cutwater-mr` with `process_mode=oldest_first`, and the same for `cutwater-publish`.

A job that must run only after a release is published, such as one that deploys a snapshot depending on newly released versions or builds the image of a released unit, needs `cutwater-publish`. In the release commit's pipeline it then starts once the publication succeeded, and reads the record of the run, which `cutwater publish --json` prints, from the artifact `cutwater-publish.json`:

```yaml
deploy-snapshot:
  stage: .post
  needs:
    - cutwater-publish
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
  script:
    - ./deploy-snapshot.sh cutwater-publish.json
```

## Installing

Each release of cutwater is a GitLab Release with the tag `<version>`, such as `1.2.3`, the crate `cutwater` on crates.io, and the npm package `cutwater`, which installs the executable for its platform from one of the packages `@sakiori/cutwater-bin-<os>-<cpu>`. The executables are built for Linux, statically linked against musl, and for macOS, on x86_64 and arm64.

```sh
cargo binstall cutwater                        # the release archive
cargo install --locked cutwater                # built from the crate
npm install --save-dev cutwater                # the npm package
mise use gitlab:sakiori-oss/cutwater           # the release archive
```

Each archive and each platform package carries, beside the executable, `THIRD-PARTY-NOTICES.txt`, the licenses and notices of the crates it is built from, of the Rust toolchain's compiler-builtins and, on Linux, of musl and Zig, and `RUST-LIBRARY-COPYRIGHT.html`, the notices of the Rust standard library. The Release links the archives `cutwater-<version>-<target>.tar.gz`, `SHA256SUMS` and its Sigstore bundle, `SHA256SUMS.sigstore.json`, signed by the pipeline of cutwater's default branch:

```sh
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity https://gitlab.com/sakiori-oss/cutwater//.gitlab-ci.yml@refs/heads/main \
  --certificate-oidc-issuer https://gitlab.com SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

## Building from source

With a Rust toolchain (`rust-toolchain.toml` names the one the project uses; `rust-version` in `Cargo.toml` is the oldest supported):

```sh
cargo install --locked --path .
```

## Releasing cutwater

cutwater releases itself through its own component, with the executable that its pipeline builds from the commit it runs for, so that a release commit's code publishes itself. Its units, in `.cutwater/config.toml`, form one fixed group tagged `<version>`, without a `v`: the crate `cutwater`, whose release assets are the four archives, `SHA256SUMS` and its bundle; the npm package `cutwater`, the unit `cutwater-npm`; and the four platform packages, which `npm/check-package.mjs` refuses to pack without their executable and its notices. `cargo run --locked -p xtask -- notices <dir> <target>` writes the notices with cargo-about, from `about.toml` and `about.hbs`; it fails when a crate's license is not one that `about.toml` accepts, and when `about.toml` accepts other licenses than `deny.toml` allows. The release merge request's pipeline builds the four targets and checks every package that the release publishes. The push pipeline of the release commit, which `cutwater released` names in the `release-decision` job, builds them again, signs `SHA256SUMS` keylessly with its GitLab ID token, stages the npm packages and publishes. No other pipeline builds, signs or stages a release, neither a later commit's nor one scheduled or started by hand or by the API on the release commit; its `cutwater-publish` passes where the release's tag and Release exist, and fails where the release is unpublished, as its files are missing. The macOS targets are built on the group's runner tagged `sakiori-oss-macos`, which takes jobs of protected refs only.

A merge request that changes the crate's shipped files (`src/`, `Cargo.toml`, `Cargo.lock`, `LICENSE`, `README.md` and the component in `templates/`) or an npm package needs a changeset: `cutwater add`, or `cutwater add --empty` for a change that releases nothing. `cutwater-check` fails it otherwise.

The project's settings, beyond those every project using the component needs:

- `CUTWATER_GITLAB_TOKEN`: the personal access token of the group's release bot, with the `api` scope, masked, protected and hidden, with the environment scope `cutwater-*`. The bot needs the Maintainer role: the protected tag pattern `*.*.*` lets Maintainers create the release tags, and the protected branch `cutwater/release` lets Maintainers push and force push, and no one merge into it.
- _Allow merge request pipelines to access protected variables and runners_, in the CI/CD settings: the release merge request's pipeline, between the protected `cutwater/release` and `main`, then builds the macOS targets, and its check covers the macOS packages. GitLab grants this only when the user whose push started the pipeline, the bot, may push or merge into `main`. Without it, `cutwater-goal` fails and says why. With it, every job of that pipeline receives the protected variables that no environment scopes, which is why the bot token and the registry tokens are scoped to the environments of `cutwater-mr` and `cutwater-publish`.
- The runner tagged `sakiori-oss-macos` enabled for the project.
- For the first publication of each package, as both registries require, set before the first release merge request is merged: `NPM_TOKEN`, a granular npm token of an account that may publish to the scope `@sakiori`, with read and write access to all packages, as the new unscoped `cutwater` needs, and _Bypass two-factor authentication_ checked, and `CARGO_REGISTRY_TOKEN`, a crates.io token with the `publish-new` and `publish-update` scopes for the crate `cutwater`; each masked, protected and hidden, scoped to the environment `cutwater-publish`, and expiring within days.
- After the first publication: a trusted publisher for each npm package, with namespace `sakiori-oss`, project `cutwater`, CI file `.gitlab-ci.yml` and environment `cutwater-publish`, and one for the crate on crates.io with the same namespace, project, workflow file path and environment. npm drops a trusted publisher that no publication used within two days, so the next release follows soon. Then delete `NPM_TOKEN` and `CARGO_REGISTRY_TOKEN` and revoke both tokens: the pipeline publishes through trusted publishing whenever they are absent.

## License

MIT
