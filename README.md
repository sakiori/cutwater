# cutwater

`cutwater` releases the packages of a repository through one merge request that a bot keeps up to date. Authors state each change's version bump in changeset files, reviewed with the change. Merging the release merge request publishes every package whose version is missing from its registry, then tags it and creates the GitLab Release.

It covers npm (pnpm and npm workspaces, single packages), Maven (multi-module projects with independently versioned units, CI-friendly `${revision}` versions) and Cargo, on GitLab.

The tool is in its design phase; `openspec/` holds the proposal.

## License

MIT
