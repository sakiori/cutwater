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
- `cutwater version` applies the pending changesets to the working tree. Each unit takes the highest bump its changesets name, and a fixed group the highest version among its members; a new member of a group releases with it. A unit whose dependency is released outside the range it declares, as npm or Cargo judges it, takes a patch release; for a peer dependency whose minor or major release leaves the range, a major release, since its rewritten peer range drops the versions its consumers install. Units that share one written version, such as crates inheriting `workspace.package.version`, must form a fixed group. It writes the versions into the manifest, `package.json` and `Cargo.toml` (once, in `workspace.package`, for crates that inherit it), rewrites dependents' ranges that no longer accept the new versions, keeping their operator, prepends each released unit's `CHANGELOG.md` in the layout of Changesets, refreshes `pnpm-lock.yaml`, `package-lock.json` and `Cargo.lock` with pnpm, npm and cargo, and deletes the changesets; when a step fails, it puts every file back. `workspace:*`, `workspace:^` and `workspace:~` follow the new versions by themselves; an explicit range such as `workspace:~1.2.0` is rewritten like any other, and a pnpm catalog range in `pnpm-workspace.yaml`. Yarn and Bun workspaces are refused. A new unit's first release is the version its files declare. `cutwater version --dry-run` prints the plan and the files it would change, and changes nothing.
- `cutwater pre enter <label>` records prerelease mode in `.cutwater/pre.toml`: releases become `<version>-<label>.<n>`, and the prereleases' changelog entries and dependency updates are kept for the final release, which also rewrites the ranges that name prereleases. `cutwater pre exit` makes the next release final. Merge both like any other change.

`cutwater` runs the installed git, 2.15 or newer. Cargo units need `cargo metadata`, so cargo must be on the `PATH` where the repository has them, and `cutwater version` runs the tool of each lockfile it refreshes. Through rustup, that cargo honours the repository's `rust-toolchain.toml` and may first install the toolchain it names; a cargo older than the workspace needs makes `cutwater` exit with 2.

`cutwater` exits with 1 when a rule is violated, such as a change without a changeset, and with 2 when it cannot reach a verdict: an invalid configuration or changeset, a failing git, or a missing tool.

## Building from source

With a Rust toolchain (`rust-toolchain.toml` names the one the project uses; `rust-version` in `Cargo.toml` is the oldest supported):

```sh
cargo install --locked --path .
```

## License

MIT
