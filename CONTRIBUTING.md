# Contributing

## Building and testing

cutwater builds with the Rust toolchain that `rust-toolchain.toml` names, which rustup installs; `rust-version` in `Cargo.toml` is the oldest supported. The tests run git 2.15 or newer, a POSIX `sh`, curl, and the ecosystem tools the tool runs: npm and pnpm, with Node.js, and cargo.

```sh
cargo build --locked
cargo test --locked --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Two groups of tests are ignored by default, because they need tools of their own, which their CI jobs install with the versions and checksums that `.gitlab/scripts/install-tools.sh` and the lockfiles under `.gitlab/` pin:

- `tests/registries.rs` publishes to a local Verdaccio and a local cargo-http-registry, named by `CUTWATER_TEST_VERDACCIO` and `CUTWATER_TEST_CARGO_HTTP_REGISTRY`: `npm ci --prefix .gitlab/verdaccio --ignore-scripts` installs Verdaccio, and `cargo test --locked --test registries -- --ignored` runs them.
- `tests/component.rs` runs cosign 3.1.4 and shellcheck 0.11.0 on the CI/CD component's steps, named by `CUTWATER_TEST_COSIGN` and `CUTWATER_TEST_SHELLCHECK`: `cargo test --locked --test component -- --ignored` runs them.

Merge request pipelines also build with the oldest supported Rust and test against the oldest supported git, run `cargo deny check` as `deny.toml` configures it, package the crate, run shellcheck on `.gitlab/scripts/`, validate the OpenSpec specification, and, when a change can affect the build, build the Linux release archives. `.gitlab/scripts/build-release.sh <target>` builds an archive: a Linux target with cargo-zigbuild and zig, a macOS target on macOS with the Xcode Command Line Tools, each with cargo-about 0.9.2 and the toolchain's `rust-src` component for the third-party notices, which `cargo run --locked -p xtask -- notices <dir> <target>` writes. `install-tools.sh` installs the pinned tools on x86_64 Linux, and rustup-init and cargo-about on Apple silicon macOS.

## Behaviour changes

cutwater's behaviour is specified with [OpenSpec](https://github.com/Fission-AI/OpenSpec): `openspec/specs/` holds the current specification, `openspec/changes/` the changes not yet implemented, and `openspec/changes/archive/` those that led to the specification. A change of behaviour starts as a change under `openspec/changes/<name>/` in the repository's schema `proposal-spec`: a `proposal.md`, the spec deltas under `specs/<capability>/spec.md`, and a `design.md` only for a consequential technical choice. A change has no `tasks.md`. Once the code, tests and README follow it, it is archived into the specification. The CLI that `.gitlab/openspec/package.json` pins checks it:

```sh
npm ci --prefix .gitlab/openspec --ignore-scripts
.gitlab/openspec/node_modules/.bin/openspec validate --all --strict --no-interactive
```

## Merge requests

A merge request that changes the crate's shipped files (`src/`, `Cargo.toml`, `Cargo.lock`, `LICENSE`, `README.md` and the component in `templates/`) or an npm package under `npm/` needs a changeset, which `cutwater-check` requires: `cutwater add` for a change that releases, or `cutwater add --empty` for one that releases nothing (`cargo run -- add` runs the tool of the checkout).

Merge request titles and commit messages follow [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `test:`, `ci:`, `chore:` and so on. The changesets, not the commit types, decide the release.

A merge request lands through the merge queue: once it is ready, a Maintainer assigns it to the `sakiori-merge` account (`glab mr update <id> --ready --assignee sakiori-merge`). The queue takes one merge request at a time: it rebases it onto `main`, waits for its pipeline and merges it. Nobody merges a merge request by hand, sets auto-merge on it or rebases one that is queued. The release merge request is queued the same way; `gitlab.merge-queue` in `.cutwater/config.toml` names the account, so `cutwater-mr` leaves a queued release merge request as it is.

Merge request pipelines run on the group's runners, which a fork outside the group does not have, so a merge request from such a fork gets its pipeline only when a member of this project with at least the Developer role, having read the change, runs it in this project.

## Releasing cutwater

cutwater releases itself through its own component, with the executable that its pipeline builds from the commit it runs for, so that a release commit's code publishes itself. Its units, in `.cutwater/config.toml`, form one fixed group tagged `<version>`, without a `v`: the crate `cutwater`, whose release assets are the four archives, `SHA256SUMS` and its bundle; the npm package `cutwater`, the unit `cutwater-npm`; and the four platform packages, which `npm/check-package.mjs` refuses to pack without their executable and its notices. `cargo run --locked -p xtask -- notices <dir> <target>` writes the notices with cargo-about, from `about.toml` and `about.hbs`; it fails when a crate's license is not one that `about.toml` accepts, and when `about.toml` accepts other licenses than `deny.toml` allows. The release merge request's pipeline builds the four targets and checks every package that the release publishes. The push pipeline of the release commit, which `cutwater released` names in the `release-decision` job, builds them again, signs `SHA256SUMS` keylessly with its GitLab ID token, stages the npm packages and publishes. No other pipeline builds, signs or stages a release, neither a later commit's nor one scheduled or started by hand or by the API on the release commit; its `cutwater-publish` passes where the release's tag and Release exist, and fails where the release is unpublished, as its files are missing. The macOS targets are built on the group's runner tagged `sakiori-oss-macos`, which takes jobs of protected refs only.

The project's settings, beyond those the README's _Project settings_ lists for every project that uses the component:

- `CUTWATER_GITLAB_TOKEN`: the personal access token of the group's release bot, with the `api` scope, masked, protected and hidden, with the environment scope `cutwater-*`. The bot needs the Maintainer role: the protected tag pattern `*.*.*` lets Maintainers create the release tags, and the protected branch `cutwater/release` lets Maintainers push and force push, and no one merge into it.
- _Allow merge request pipelines to access protected variables and runners_, in the CI/CD settings: the release merge request's pipeline, between the protected `cutwater/release` and `main`, then builds the macOS targets, and its check covers the macOS packages. GitLab grants this only when the user whose push started the pipeline, the bot, may push or merge into `main`. Without it, `cutwater-goal` fails and says why. With it, every job of that pipeline receives the protected variables that no environment scopes, which is why the bot token and the registry tokens are scoped to the environments of `cutwater-mr` and `cutwater-publish`.
- The runner tagged `sakiori-oss-macos` enabled for the project.
- For the first publication of each package, as both registries require, set before the first release merge request is merged: `NPM_TOKEN`, a granular npm token of an account that may publish to the scope `@sakiori`, with read and write access to all packages, as the new unscoped `cutwater` needs, and _Bypass two-factor authentication_ checked, and `CARGO_REGISTRY_TOKEN`, a crates.io token with the `publish-new` and `publish-update` scopes for the crate `cutwater`; each masked, protected and hidden, scoped to the environment `cutwater-publish`, and expiring within days.
- After the first publication: a trusted publisher for each npm package, with namespace `sakiori-oss`, project `cutwater`, CI file `.gitlab-ci.yml` and environment `cutwater-publish`, and one for the crate on crates.io with the same namespace, project, workflow file path and environment. npm drops a trusted publisher that no publication used within two days, so the next release follows soon. Then delete `NPM_TOKEN` and `CARGO_REGISTRY_TOKEN` and revoke both tokens: the pipeline publishes through trusted publishing whenever they are absent.

## Security

Report a vulnerability as [SECURITY.md](SECURITY.md) describes, never in a public issue.
