//! The forge: what `mr` and `publish` need from the service that hosts the
//! repository, its branches, merge requests, pipelines, tags, releases and
//! package registry. GitLab is the only implementation; the interface keeps
//! the commands independent of it.

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

/// The account whose token the tool uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    pub id: u64,
    pub username: String,
    /// Whether the account administers the instance.
    pub admin: bool,
}

/// A rule that protects the tags matching `pattern`, in which `*` stands
/// for any text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedTag {
    pub pattern: String,
    /// Who may create the tags; anyone the rules name.
    pub create: Vec<CreateAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateAccess {
    /// Members with this access level or a higher one; 0 for no one.
    Level(u32),
    User(u64),
    /// The members of a group.
    Group(u64),
    DeployKey(u64),
}

/// A group the project is shared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupShare {
    pub group: u64,
    /// The highest access level the group's members get in the project.
    pub access: u32,
}

/// The newest pipeline of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub id: u64,
    pub status: String,
    pub url: String,
}

impl Pipeline {
    /// Whether the pipeline is over: no job of it runs or waits any more.
    pub fn finished(&self) -> bool {
        matches!(
            self.status.as_str(),
            "success" | "failed" | "canceled" | "skipped"
        )
    }
}

/// A file of a package in the project's generic package registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    pub name: String,
    /// The SHA-256 checksum of its content, in lowercase hex.
    pub sha256: String,
}

/// A link of a release to one of its assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseLink {
    pub name: String,
    pub url: String,
    /// The path of the asset's permanent link below the release.
    pub path: String,
}

/// A release to create, tagging `commit` when its tag does not exist.
#[derive(Debug, Clone)]
pub struct NewRelease<'a> {
    pub tag: &'a str,
    pub commit: &'a str,
    pub name: &'a str,
    pub notes: &'a str,
    pub links: &'a [ReleaseLink],
}

/// The operations `mr` and `publish` perform on the forge.
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

    /// The account of the token.
    fn bot(&self) -> Result<Bot>;

    /// The access level of a user in the project, inherited ones included;
    /// 0 when the user is no member.
    fn access_level(&self, user: u64) -> Result<u32>;

    /// The access level of a user's own membership of a group, not one
    /// inherited from a group above it; `None` when the user has none.
    fn direct_group_member(&self, group: u64, user: u64) -> Result<Option<u32>>;

    /// The groups the project is shared with, `None` when the forge does
    /// not say.
    fn shared_groups(&self) -> Result<Option<Vec<GroupShare>>>;

    /// The project's protected tags.
    fn protected_tags(&self) -> Result<Vec<ProtectedTag>>;

    /// The commit a tag points at, `None` when the tag does not exist.
    fn tag(&self, name: &str) -> Result<Option<String>>;

    /// Whether the release of a tag exists.
    fn has_release(&self, tag: &str) -> Result<bool>;

    /// The newest pipeline of `commit` on `branch`.
    fn latest_pipeline(&self, commit: &str, branch: &str) -> Result<Option<Pipeline>>;

    /// The files of a version of a package in the generic package registry.
    fn package_files(&self, package: &str, version: &str) -> Result<Vec<PackageFile>>;

    /// Uploads a file to a version of a package in the generic package
    /// registry.
    fn upload_package_file(
        &self,
        package: &str,
        version: &str,
        file: &str,
        content: &[u8],
    ) -> Result<()>;

    /// Where a file of the generic package registry is downloaded from.
    fn package_file_url(&self, package: &str, version: &str, file: &str) -> String;

    /// Creates a release, and its tag on its commit where the tag does not
    /// exist.
    fn create_release(&self, release: &NewRelease) -> Result<()>;
}
