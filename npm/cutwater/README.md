# cutwater

`cutwater` releases the packages of a repository through one merge request that a bot keeps up to date, from changeset files reviewed with each change. This package installs its executable for Linux (x64, arm64) and macOS (x64, arm64) from the platform package that matches the machine, and runs it:

```sh
npx cutwater add
```

The [project's README](https://gitlab.com/sakiori-oss/cutwater#readme) describes the commands, the configuration and the GitLab CI/CD component.
