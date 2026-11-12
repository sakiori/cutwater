## Why

A repository that publishes several packages, often from more than one ecosystem, needs the same release procedure every time: decide each package's next version, write it into the package files and changelogs, have the result reviewed, publish what is new in dependency order, and only then tag it. Done by hand, or by tools that push to the default branch directly, this goes wrong in familiar ways: a version bump nobody reviewed, a tag that names a version the registry never received, a dependent package released against a producer that is not published yet, a Maven release version that sits on the default branch and breaks the next snapshot deployment.

The established tools each cover part of this:

- release-please keeps one release pull request with a manifest of per-path components and understands Maven workspaces, but only on GitHub.
- Changesets, with changesets-gitlab, keeps a release merge request on GitLab, but only for npm packages.
- release-plz keeps a release merge request on GitLab, but only for Cargo.
- Sampo handles several ecosystems through changeset files, but only on GitHub, and it publishes static Maven versions without a snapshot cycle.
- monochange plans releases across several ecosystems from change files and opens release requests on several forges, but it has no Maven support.
- semantic-release, cocogitto and the Maven release plugin push release commits or tags to the branch themselves.

None of them combines a GitLab release merge request, reviewed changeset files, multi-module Maven with independently versioned units and a snapshot cycle, and npm. This change builds the tool with npm and Cargo; the Maven adapter follows in the change `add-maven-adapter`.

## What changes

`cutwater` is a single command-line tool, written in Rust and shipped as one static binary. It releases the units of a repository through one merge request that a bot keeps up to date.

- **Intent.** Authors declare each change's version bump in a changeset file, `.changeset/<name>.md`, in the format of Changesets, and it is reviewed with the change. A merge request pipeline refuses a change to a unit's shipped files that carries no changeset for it. A repository that uses Changesets adopts the tool with `init` and keeps its pending files.
- **Versioning.** The tool plans the next version of each unit from its pending changesets, its fixed group and the units it depends on. It records the released versions in a version manifest, writes them through the unit's ecosystem adapter, writes each unit's changelog, and removes the applied changesets.
- **Release merge request.** On the default branch, the tool commits the applied plan onto its own release branch and creates or updates one merge request. It never merges it: merging it, or handing it to the project's merge queue, is the release decision. The release merge request's pipeline checks what would be published.
- **Publication.** The pipeline of the release commit, the commit that landed the release merge request, publishes every unit that the commit released and the registry lacks, in dependency order, through the ecosystem's own tool and the credentials the environment configured. Only after that does it tag the commit and create the GitLab Release of each completed unit or group. A failed publication is resumed in the same commit's pipeline, so what is published is always the reviewed tree.
- **Ecosystems.** Built-in adapters cover npm (pnpm and npm workspaces, single packages) and Cargo (workspaces and single crates). Repository-specific checks plug in as commands. An adapter protocol for external executables is left to a later change; the internal adapter interface is designed so that the protocol can be added on top of it.
- **Forge and CI.** The tool supports GitLab, on GitLab.com and on self-managed instances, through API v4, and ships a CI/CD component with the jobs and their ordering. The forge sits behind a narrow interface so that other forges can follow.

The tool does not derive versions from commit messages, does not log in to registries, and does not merge or queue merge requests. Maven is out of scope for this change.

## Capabilities

### New capabilities

- `release-intent`: the configuration, units and fixed groups, adoption by `init`, changeset files, and the merge request check.
- `release-versioning`: the version plan, the version manifest, applying the plan, changelogs and prerelease mode.
- `release-merge-request`: the bot-maintained release merge request and the checks of its pipeline.
- `release-publication`: publication from the release commit, tags, GitLab Releases, release assets and the record of a run.
- `ecosystem-adapters`: the npm and Cargo adapters and repository checks.
- `ci-component`: the GitLab CI/CD component that runs the tool's jobs in the right pipelines and order.

### Modified capabilities

None.

## Impact

- **Repository files.** A repository adopts a configuration directory, `.cutwater/`, keeps changeset files in `.changeset/`, and includes the CI component.
- **Forge setup.** The project needs a bot that can push the protected release branch. On GitLab.com Free this is a bot user's personal access token, because project access tokens need Premium there. The project also needs a merge setting under which a merge request pipeline tests the tree that lands: fast-forward merges on any tier, or merged results pipelines or merge trains on Premium.
- **Credentials.** Registry credentials, including the read credentials that private registries need for existence checks, stay with the CI environment.
- **Trusted publishing.** npm trusted publishing works only on GitLab.com's shared runners, and crates.io trusted publishing only from GitLab.com.
- **Self-release.** The tool is released by its own release merge request to GitLab Releases, crates.io and npm, with `X.Y.Z` tags.
