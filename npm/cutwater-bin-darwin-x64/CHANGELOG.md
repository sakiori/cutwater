# @sakiori/cutwater-bin-darwin-x64

## 0.2.0

### Minor Changes

- cutwater releases Maven units: an aggregator POM whose version is `${revision}`, with the modules that share it, while the default branch holds the snapshot of its next release. Dependents refer to producers through version properties that `cutwater version` keeps equal to the producers' revisions, and are released with a patch when a producer they depend on at run time is. The release merge request's `goal` builds the released units with their released versions and checks the POMs they would deploy, `publish` deploys only the files the deploy repository lacks, asking that repository alone, and `init` seeds a Maven unit with the highest final version its deploy repository holds.

## 0.1.0

### Minor Changes

- cutwater releases the packages of a repository, npm packages and Cargo crates on GitLab, through one merge request that a bot keeps up to date, from changeset files reviewed with each change. Its GitLab CI/CD component runs the checks, the release merge request and the publication in the pipelines they belong to.
