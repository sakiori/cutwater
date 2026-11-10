## ADDED Requirements

### Requirement: Release merge request
`mr` SHALL run on a pipeline of the default branch. When changesets are pending at the default branch's current tip, read after it holds the CI component's resource group for `mr`, it SHALL apply the plan to that tip as `version` does and record the result as one commit, whose parent is that tip, on the release branch. It SHALL then create the release merge request or update the open one. There SHALL be at most one open release merge request per default branch.

#### Scenario: First release merge request
- **WHEN** `mr` runs on the default branch with pending changesets and no release merge request is open
- **THEN** the release branch holds one commit on top of the default branch's tip with the applied versions, and one merge request from it is open

#### Scenario: Older pipeline runs last
- **WHEN** the pipeline of commit A reaches `mr` after commit B, which added a changeset, landed
- **THEN** `mr` applies the plan to B, so the release merge request carries B's changeset

### Requirement: Release merge request description
The release merge request's title SHALL come from the configuration, and its description SHALL list every released unit's next version and changelog entry.

#### Scenario: Description of two units
- **WHEN** the plan releases `core` 2.0.0 and `api` 1.4.3
- **THEN** the description lists both with their versions and entries

### Requirement: Unchanged release
`mr` SHALL not write the release branch when the file changes it would commit equal those of the release branch's commit and either that commit already sits on the default branch's tip or the project's merge setting does not require it to, so that an unchanged release keeps its pipeline and approvals.

#### Scenario: Merge without changesets under merged results
- **WHEN** a merge request without changesets lands while the release merge request is open, and the project uses merged results pipelines
- **THEN** `mr` leaves the release branch as it is

#### Scenario: Same release written twice
- **WHEN** an older pipeline's `mr` already wrote the release onto the current tip and the tip's own `mr` runs
- **THEN** `mr` leaves the release branch as it is

### Requirement: Nothing pending
When no changeset is pending, `mr` SHALL close an open release merge request, even a queued one, and leave the release branch as it is, because merging it would release changes that are no longer pending.

#### Scenario: Pending changeset reverted
- **WHEN** the only pending changeset is deleted by a revert and `mr` runs
- **THEN** the open release merge request is closed with a note that nothing is pending

#### Scenario: Queued release with nothing pending
- **WHEN** the release merge request is assigned to the configured merge queue account and a revert deletes the only pending changeset
- **THEN** `mr` closes the release merge request

### Requirement: Queued release merge request
While changesets are pending, `mr` SHALL not update a release merge request that is in a merge train, set to merge automatically, or assigned to an account the configuration names as a merge queue, and SHALL say so; the next run after the merge request leaves that state updates it.

#### Scenario: Merge request in the queue
- **WHEN** a person assigned the release merge request to the configured merge queue account and a new changeset lands
- **THEN** `mr` leaves the release branch and the merge request unchanged and reports that the merge request is queued

### Requirement: The tool never merges
The tool SHALL never merge a merge request, approve it, set it to merge automatically, add it to a merge train or assign it to anyone.

#### Scenario: Green release pipeline
- **WHEN** the release merge request's pipeline succeeds
- **THEN** the merge request stays open until a person merges it or hands it to the merge queue

### Requirement: Bot token
`mr` and `publish` SHALL authenticate to GitLab with a token from an environment variable the configuration names, `CUTWATER_GITLAB_TOKEN` by default, and `mr` SHALL refuse the CI job token, whose commits start no pipelines. The tool SHALL never write a token to its output, its error messages or the repository.

#### Scenario: Job token only
- **WHEN** `mr` runs in CI without the configured token variable
- **THEN** it refuses, names the variable, and explains that the release merge request needs a pipeline the job token cannot start

### Requirement: Merge settings warning
`mr` SHALL read the project's merge settings and warn, in its output and in the release merge request's description, when a merge request pipeline does not test the tree that lands. The pipeline tests that tree under fast-forward merges and under merge commits with semi-linear history, both of which require the source branch to contain the target branch, and under merged results pipelines or merge trains.

#### Scenario: Merge commits without merged results
- **WHEN** the project merges with merge commits and has merged results pipelines off
- **THEN** `mr` warns that the release merge request's checks do not cover the tree that lands

#### Scenario: Semi-linear history
- **WHEN** the project merges with merge commits with semi-linear history and has merged results pipelines off
- **THEN** `mr` does not warn

### Requirement: Dry run of the release merge request
`mr --dry-run` SHALL report the release commit's file changes and how it would create, update or close the release merge request, and SHALL write nothing to the forge.

#### Scenario: Nothing written
- **WHEN** `mr --dry-run` runs with pending changesets and no release merge request is open
- **THEN** it lists the files the release commit would create, update and delete, says that it would create the release merge request, and the forge receives no write

### Requirement: Release merge request checks
`goal` SHALL run in the release merge request's pipeline and fail when a pending changeset names a unit that the release publishes, when the manifest and the adapters' files disagree, or when a unit's adapter checks fail on what would be published. It SHALL then run the repository checks. Pending changesets for other units SHALL not fail it.

#### Scenario: Rebased onto a changeset for a released unit
- **WHEN** a merge queue rebases the release merge request, which releases `api`, onto a commit that added a changeset for `api`
- **THEN** `goal` fails and names the changeset, and the merge request does not land

#### Scenario: Rebased onto a changeset for another unit
- **WHEN** the release merge request releases `api` and is rebased onto a commit that added a changeset for `web` only
- **THEN** `goal` does not fail for that changeset, and `web` goes into the next release
