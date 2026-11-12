# ci-component Specification

## Purpose
The GitLab CI/CD component that runs cutwater's jobs in the pipelines they belong to and in the right order, installs the tool release of its own version after verifying its signature, and keeps the credentials of the release with the jobs that use them.

## Requirements

### Requirement: Jobs and pipelines
The project SHALL ship a GitLab CI/CD component that adds four jobs: `check` in merge request pipelines other than the release merge request's, `goal` in the release merge request's pipelines, and `mr` and `publish` in pipelines of the default branch.

#### Scenario: Merge request pipeline
- **WHEN** a merge request that is not the release merge request starts a pipeline
- **THEN** the pipeline runs `check` and neither `goal`, `mr` nor `publish`

### Requirement: The tool's release
The jobs SHALL run the tool release of the component's own version, which the component learns from GitLab's component context, as the catalog version or the exact release tag it is included by, and downloads from a location it takes as an input. A pipeline that includes the component by a branch or a commit SHALL name an executable it built, which the jobs run instead.

#### Scenario: Included by a commit
- **WHEN** a pipeline includes the component by a commit and names no executable
- **THEN** every job fails and says to include a release tag or to name an executable

### Requirement: Verified installation
Before it runs the tool, the component SHALL verify the Sigstore bundle of the release's `SHA256SUMS` with cosign against the identity of cutwater's release pipeline, the GitLab.com CI configuration of cutwater's default branch, and then the downloaded archive against `SHA256SUMS`. It SHALL pin cosign by version and by the SHA-256 of each platform's executable, and SHALL take a trusted root file as an input that replaces Sigstore's public trust root.

#### Scenario: Tampered download
- **WHEN** the download location serves an archive that `SHA256SUMS` does not list, or a `SHA256SUMS` whose bundle does not verify against the identity of cutwater's release pipeline
- **THEN** the job fails before it runs the tool

#### Scenario: Mirror without Sigstore's trust root
- **WHEN** a self-managed instance mirrors the component and the release files, cannot reach Sigstore's public trust root, and sets the trusted root input to a copy of it
- **THEN** the component verifies the bundle against that file without reaching Sigstore

### Requirement: One writer at a time
The component SHALL run `mr` in a resource group, and `publish` in a resource group of its own. Neither job SHALL be interruptible. The component's documentation SHALL require that default branch pipelines are never cancelled automatically, because a release commit's pipeline is the only one that publishes its release, and that both resource groups process the oldest pipeline first, which GitLab lets a project set only through its API.

#### Scenario: Two default branch pipelines
- **WHEN** two default branch pipelines reach `publish` together
- **THEN** the second `publish` waits for the first and is not cancelled by any newer pipeline

### Requirement: Order after publication
The component SHALL let a repository order its own jobs after `publish`, so that jobs which deploy snapshots referring to newly released versions, or build images of released units, run only once `publish` succeeded. It SHALL keep the record of `publish`'s run as an artifact of the job, for those jobs to read.

#### Scenario: Snapshot deployment after a release
- **WHEN** a repository declares its snapshot deployment job to need `publish`
- **THEN** in the release commit's pipeline the snapshot deployment starts only after `publish` succeeded

### Requirement: Credentials of the jobs alone
The component SHALL run `mr` and `publish` in environments it takes as inputs, without deploying, so that the bot token and the registry tokens can be scoped to them and no other job of a pipeline receives them.

#### Scenario: Test job of the release merge request
- **WHEN** a project lets the release merge request's pipeline use protected variables and scopes the bot token to the environments of `mr` and `publish`
- **THEN** the pipeline's test jobs do not receive the bot token

### Requirement: Component inputs
The component SHALL take as inputs the jobs' image and stages, commands every job runs before the tool, commands that prepare registry credentials and packages before `goal` and `publish`, the release branch, `publish`'s runner tags, environment and registry authentication, the download locations of the tool and of cosign, a trusted root and an executable to run instead of a release. It SHALL work unchanged on GitLab.com and on self-managed instances that mirror it.

#### Scenario: Registry login before publication
- **WHEN** a repository sets the preparation input to a command that writes its registry login
- **THEN** that command runs in `goal` and `publish` before the tool

#### Scenario: Release branch input and configuration disagree
- **WHEN** the release branch input names another branch than `release-branch` in the configuration
- **THEN** every job refuses to run, because the release merge request's pipeline would run `check` instead of `goal`

### Requirement: Trusted publishing on crates.io
The component SHALL offer crates.io's trusted publishing as an opt-in input. With it, `publish` SHALL exchange the job's GitLab ID token, with the audience crates.io, for a crates.io token when the commit releases a Cargo unit, give that token to the tool alone, and revoke it once the tool ends, or after the job when it is canceled. Neither token SHALL appear in the job's log.

#### Scenario: No Cargo unit released
- **WHEN** trusted publishing is chosen and the commit releases no Cargo unit
- **THEN** `publish` exchanges no token, so that crates.io's availability does not decide the job

#### Scenario: First publication with a token
- **WHEN** trusted publishing is chosen and the environment sets a crates.io token, as a crate's first publication needs
- **THEN** `publish` uses that token instead of an exchange and warns that it does

#### Scenario: Exchange refused
- **WHEN** crates.io refuses to exchange the ID token
- **THEN** `publish` fails with crates.io's answer and the tool does not run

#### Scenario: Publication fails
- **WHEN** the tool fails after the exchange
- **THEN** the token is revoked and the job fails with the tool's exit status
