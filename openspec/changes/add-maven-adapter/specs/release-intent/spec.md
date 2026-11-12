## MODIFIED Requirements

### Requirement: Units
A unit SHALL be what is versioned and released as one, as its ecosystem adapter defines it: an npm package, a Maven aggregator with the modules that share its version, or a Cargo crate. Each unit SHALL have a name unique in the repository, a path, an adapter, the patterns of its shipped files and a tag template.

#### Scenario: Duplicate unit name
- **WHEN** two units have the same name
- **THEN** every command refuses and names both paths
