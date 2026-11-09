## ADDED Requirements

### Requirement: Jobs and pipelines
The project SHALL ship a GitLab CI/CD component that adds four jobs: `check` in merge request pipelines, `goal` in the release merge request's pipelines, and `mr` and `publish` in pipelines of the default branch. The component SHALL install the tool release of its own version from a download location it takes as an input, and verify it against a checksum recorded in the component itself.

#### Scenario: Merge request pipeline
- **WHEN** a merge request that is not the release merge request starts a pipeline
- **THEN** the pipeline runs `check` and neither `goal`, `mr` nor `publish`

### Requirement: One writer at a time
The component SHALL run `mr` in a resource group that processes the oldest pipeline first, and `publish` in a resource group of its own. Neither job SHALL be interruptible. The component's documentation SHALL require that default branch pipelines are never cancelled automatically, because a release commit's pipeline is the only one that publishes its release.

#### Scenario: Two default branch pipelines
- **WHEN** two default branch pipelines reach `publish` together
- **THEN** the second `publish` waits for the first and is not cancelled by any newer pipeline

### Requirement: Order after publication
The component SHALL let a repository order its own jobs after `publish`, so that jobs which deploy snapshots referring to newly released versions, or build images of released units, run only once `publish` succeeded.

#### Scenario: Snapshot deployment after a release
- **WHEN** a repository declares its snapshot deployment job to need `publish`
- **THEN** in the release commit's pipeline the snapshot deployment starts only after `publish` succeeded

### Requirement: Component inputs
The component SHALL take as inputs the stage of each job, the name of the variable holding the bot token, the image the jobs run in and the commands that prepare registry credentials before `goal` and `publish`, and SHALL work unchanged on GitLab.com and on self-managed instances that mirror it.

#### Scenario: Registry login before publication
- **WHEN** a repository sets the credentials input to a command that writes its registry login
- **THEN** that command runs in `goal` and `publish` before the tool
