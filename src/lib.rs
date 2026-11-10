//! The implementation of the `cutwater` command-line tool. The library exists
//! for the binary and its tests; it has no stable API.

pub mod adapter;
pub mod changelog;
pub mod changes;
pub mod changeset;
pub mod cli;
pub mod commands;
pub mod config;
pub mod git;
pub mod graph;
pub mod manifest;
pub mod output;
pub mod plan;
pub mod prerelease;
pub mod units;
