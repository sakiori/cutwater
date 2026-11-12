## MODIFIED Requirements

### Requirement: New and removed units
A unit without a manifest entry SHALL be new and SHALL be released only once a changeset names it with a bump other than `none`. Its first release SHALL be the version its adapter's files declare, for Maven the `revision` without `-SNAPSHOT`, and the changesets naming it SHALL only provide its changelog entry.

#### Scenario: First release of a new package
- **WHEN** a merge request adds the npm package `export` at version 0.1.0 with a changeset, and the release merge request lands
- **THEN** the manifest holds `export` 0.1.0 and `publish` releases it at 0.1.0

#### Scenario: First release of a new Maven unit
- **WHEN** a merge request adds the Maven unit `billing` with `revision` `0.1.0-SNAPSHOT` and a changeset naming it, and the release merge request lands
- **THEN** the manifest holds `billing` 0.1.0 and `publish` deploys it at 0.1.0
