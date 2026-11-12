# Security policy

## Reporting a vulnerability

Report a vulnerability as a confidential issue of the project: open a [new confidential issue](https://gitlab.com/sakiori-oss/cutwater/-/issues/new?issue[confidential]=true) and check that **This issue is confidential** is selected before you create it, so that only you, its assignees and the project's members with at least the Planner role see it. Never report one in a public issue or merge request before a fix is released.

Include the release (`cutwater --version`), how it was installed, the platform, and the steps or a repository that show the problem.

## Scope

- the tool, `cutwater`, as the crate, the npm packages and the release archives ship it;
- the GitLab CI/CD component, `templates/release.yml`, including how it installs and verifies the tool and handles the bot's and the registries' credentials;
- the release pipeline and its signatures: the archives, `SHA256SUMS` and its Sigstore bundle, and the npm packages' provenance.

Vulnerabilities of GitLab, the registries or Sigstore themselves belong to their projects.

## Supported versions

Fixes are made on `main` and ship in the next release; only the latest release is supported. The README's _Install_ section shows how to verify a release's archives.
