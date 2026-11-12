# cutwater

`cutwater` releases the packages of a repository through one merge request that a bot keeps up to date, from changeset files reviewed with each change. It supports npm packages (pnpm and npm workspaces, single packages) and Cargo crates, released through GitLab. This package installs its executable for Linux (x64, arm64) and macOS (x64, arm64) from the platform package that matches the machine, and runs it.

Set a repository up, on a clean checkout of its default branch:

```sh
npm install --save-dev cutwater
npx cutwater init
```

`init` refuses a package that was never published and says so; declare each such package new with `npx cutwater init --new <name>`. Then describe each change with a changeset:

```sh
npx cutwater add
```

The [project's README](https://gitlab.com/sakiori-oss/cutwater#readme) walks through the rest of the setup, the GitLab CI/CD component and the project's settings, and describes every command and setting.
