//! The forge: what `mr` needs from the service that hosts the repository,
//! its branches and its merge requests. GitLab is the only implementation;
//! the interface keeps the commands independent of it.

pub mod gitlab;

use anyhow::Result;

/// How the project merges merge requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeSettings {
    pub method: MergeMethod,
    /// Whether merge request pipelines test the merged result.
    pub merged_results: bool,
    /// Whether merge requests land through merge trains.
    pub merge_trains: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    /// A merge commit, whatever the source branch is based on.
    Merge,
    /// A merge commit, allowed only when the source branch contains the
    /// target branch, as for a fast-forward.
    SemiLinear,
    /// A fast-forward of the target branch.
    FastForward,
}

impl MergeSettings {
    /// Whether the tree a merge request pipeline tests is the tree that
    /// lands.
    pub fn tests_landing_tree(&self) -> bool {
        self.method != MergeMethod::Merge || self.merged_results || self.merge_trains
    }

    /// Whether a merge request can land only when its source branch
    /// contains the target branch's tip.
    pub fn requires_tip(&self) -> bool {
        self.method != MergeMethod::Merge
    }
}

/// A commit, by its full name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
}

/// An open merge request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    /// The project-scoped number, `!<number>` on GitLab.
    pub number: u64,
    pub title: String,
    pub description: String,
    pub url: String,
    /// The usernames it is assigned to.
    pub assignees: Vec<String>,
    /// Whether it is set to merge when its pipeline succeeds.
    pub auto_merge: bool,
}

/// A file of a commit written through the forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAction {
    pub path: String,
    pub change: FileChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    Create(Vec<u8>),
    Update(Vec<u8>),
    Delete,
}

impl FileAction {
    /// The file's content after the action, `None` for a deletion.
    pub fn content(&self) -> Option<&[u8]> {
        match &self.change {
            FileChange::Create(content) | FileChange::Update(content) => Some(content),
            FileChange::Delete => None,
        }
    }

    pub fn verb(&self) -> &'static str {
        match self.change {
            FileChange::Create(_) => "create",
            FileChange::Update(_) => "update",
            FileChange::Delete => "delete",
        }
    }
}

/// A commit to write onto a branch.
#[derive(Debug, Clone)]
pub struct NewCommit<'a> {
    pub branch: &'a str,
    /// The commit's parent. The branch is reset onto the new commit
    /// whatever it held before.
    pub parent: &'a str,
    pub message: &'a str,
    pub actions: &'a [FileAction],
}

/// The operations `mr` performs on the forge.
pub trait Forge {
    /// How the project merges merge requests.
    fn merge_settings(&self) -> Result<MergeSettings>;

    /// The commit a branch points at, `None` when the branch does not exist.
    fn branch(&self, name: &str) -> Result<Option<Commit>>;

    /// The open merge requests from `source` into `target`, both branches of
    /// the project.
    fn open_merge_requests(&self, source: &str, target: &str) -> Result<Vec<MergeRequest>>;

    /// Whether the merge request is in a merge train.
    fn in_merge_train(&self, merge_request: &MergeRequest) -> Result<bool>;

    /// Writes a commit and points the branch at it; returns the commit's
    /// name.
    fn write_commit(&self, commit: &NewCommit) -> Result<String>;

    fn create_merge_request(
        &self,
        source: &str,
        target: &str,
        title: &str,
        description: &str,
    ) -> Result<MergeRequest>;

    fn edit_merge_request(&self, number: u64, title: &str, description: &str) -> Result<()>;

    /// Adds `note` to the merge request and closes it.
    fn close_merge_request(&self, number: u64, note: &str) -> Result<()>;
}
