# cutwater

`cutwater` releases the packages of a repository through one merge request that a bot keeps up to date. Authors state each change's version bump in changeset files, reviewed with the change. Merging the release merge request publishes every package whose version is missing from its registry, then tags it and creates the GitLab Release.

It is meant to cover npm (pnpm and npm workspaces, single packages), Maven (multi-module projects with independently versioned units, CI-friendly `${revision}` versions) and Cargo, on GitLab. `openspec/` holds the specification.

## What works now

The settings live in `.cutwater/config.toml`; [`schema/config.schema.json`](schema/config.schema.json) describes them for editors. A repository whose publishable npm packages and Cargo crates are its units needs only:

```toml
[npm]
discover = true

[cargo]
discover = true
```

A unit ships every file under its directory, or the files its `shipped` patterns select, but never the files of a unit nested under it.

- `cutwater add` creates a changeset file in `.changeset/`, in the format of [Changesets](https://github.com/changesets/changesets). In a terminal it asks for the units, their bumps and the entry, with the units the branch changed preselected. `cutwater add --unit api:minor --message "Add the export endpoint"` does the same without asking, and `cutwater add --empty` records that a change releases nothing.
- `cutwater check` fails a merge request whose commits change a unit's shipped files without a changeset for that unit, and lists the merge request's edits and deletions of other merge requests' pending changesets. In a merge request pipeline it compares HEAD with the merge base of the merge request's diff base, and in a merged results or merge train pipeline with the target branch commit the pipeline merged onto; elsewhere, pass `--base <commit>`. The clone must reach that commit, so raise `GIT_DEPTH` if it is shallow.

`cutwater` runs the installed git, 2.15 or newer. Cargo discovery runs `cargo metadata` in the repository, so cargo must be on the `PATH` where it is turned on. Through rustup, that cargo honours the repository's `rust-toolchain.toml` and may first install the toolchain it names; a cargo older than the workspace needs makes `cutwater` exit with 2.

`cutwater` exits with 1 when a rule is violated, such as a change without a changeset, and with 2 when it cannot reach a verdict: an invalid configuration or changeset, a failing git, or a missing tool.

## Building from source

With a Rust toolchain (`rust-toolchain.toml` names the one the project uses; `rust-version` in `Cargo.toml` is the oldest supported):

```sh
cargo install --locked --path .
```

## License

MIT
