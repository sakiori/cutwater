//! GitLab, through API v4 and the `gitlab` crate, on GitLab.com and on
//! self-managed instances.

use std::borrow::Cow;
use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use gitlab::api::common::NameOrId;
use gitlab::api::common::{SortOrder, path_escaped};
use gitlab::api::endpoint_prelude::Method;
use gitlab::api::endpoint_prelude::QueryParams;
use gitlab::api::groups::members::GroupMember;
use gitlab::api::projects::members::AllProjectMember;
use gitlab::api::projects::merge_requests::notes::CreateMergeRequestNote;
use gitlab::api::projects::merge_requests::{
    CreateMergeRequest, EditMergeRequest, MergeRequestState, MergeRequestStateEvent, MergeRequests,
};
use gitlab::api::projects::packages::generic::UploadPackageFile;
use gitlab::api::projects::packages::package_files::PackageFiles;
use gitlab::api::projects::pipelines::{PipelineOrderBy, Pipelines};
use gitlab::api::projects::protected_tags::ProtectedTags;
use gitlab::api::projects::releases::links::LinkType;
use gitlab::api::projects::releases::{CreateRelease, CreateReleaseAssetLinks};
use gitlab::api::projects::repository::branches::Branch;
use gitlab::api::projects::repository::commits::{CommitAction, CommitActionType, CreateCommit};
use gitlab::api::projects::repository::tags::Tag;
use gitlab::api::users::CurrentUser;
use gitlab::api::{self, ApiError, Endpoint, Pageable, Pagination, Query};
use gitlab::{CertificateBundle, GitlabBuilder, RestError};
use serde::Deserialize;

use super::{
    Bot, Commit, CreateAccess, FileChange, Forge, GroupShare, MergeMethod, MergeRequest,
    MergeSettings, NewCommit, NewRelease, PackageFile, Pipeline, ProtectedTag,
};
use crate::config::{GitLabSettings, JOB_TOKEN_VARIABLE};

/// GitLab CI's URL of the instance, such as `https://gitlab.com`.
const SERVER_URL_VARIABLE: &str = "CI_SERVER_URL";
/// GitLab CI's file of the certificates that the instance's TLS
/// certificate chains to, set when the runner is configured with one.
const TLS_CA_FILE_VARIABLE: &str = "CI_SERVER_TLS_CA_FILE";
const PROJECT_ID_VARIABLE: &str = "CI_PROJECT_ID";
/// The prefix of GitLab's CI job tokens.
const JOB_TOKEN_PREFIX: &str = "glcbt-";

/// The command that reaches GitLab with the bot's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotCommand {
    Mr,
    Publish,
}

impl BotCommand {
    fn name(self) -> &'static str {
        match self {
            BotCommand::Mr => "mr",
            BotCommand::Publish => "publish",
        }
    }

    fn why_not_the_job_token(self) -> &'static str {
        match self {
            BotCommand::Mr => {
                "commits made with it start no pipelines, so the release merge request would \
                 never be checked."
            }
            BotCommand::Publish => {
                "it acts with the rights of whoever started the pipeline, so whether a protected \
                 release tag can be created would depend on who merged."
            }
        }
    }
}

/// A token, never shown.
struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

/// Where and as whom the tool reaches GitLab.
#[derive(Debug)]
pub struct Connection {
    /// The instance's URL, such as `https://gitlab.com`.
    pub server: String,
    /// The project's id or full path.
    pub project: String,
    /// The variable the token came from, to name it in messages.
    pub token_variable: String,
    token: Token,
    /// The PEM certificates to trust besides the system's.
    ca_file: Option<String>,
}

impl Connection {
    /// The connection of the pipeline the tool runs in, for `command`: the
    /// instance from `CI_SERVER_URL`, the project from `CI_PROJECT_ID`, the
    /// bot token from the variable the configuration names. Refuses the CI
    /// job token.
    pub fn from_env(settings: &GitLabSettings, command: BotCommand) -> Result<Connection> {
        let variable = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let token_variable = settings.token_variable.clone();
        let why = command.why_not_the_job_token();
        let name = command.name();
        let Some(token) = variable(&token_variable) else {
            bail!(
                "{token_variable} is not set, but `{name}` needs the bot's token in it. The CI \
                 job token cannot stand in: {why} Set {token_variable} to a personal access token \
                 of a bot user or a project access token with the `api` scope, as a masked and \
                 protected CI/CD variable."
            );
        };
        let job_token = variable(JOB_TOKEN_VARIABLE);
        if token.starts_with(JOB_TOKEN_PREFIX) || job_token.as_deref() == Some(token.as_str()) {
            bail!(
                "{token_variable} holds the CI job token, but {why} Set {token_variable} to a \
                 personal access token of a bot user or a project access token with the `api` \
                 scope."
            );
        }
        let server = variable(SERVER_URL_VARIABLE).ok_or_else(|| {
            anyhow!(
                "{SERVER_URL_VARIABLE} is not set; run `{name}` in a GitLab CI pipeline, or set it \
                 to the instance's URL, such as https://gitlab.com"
            )
        })?;
        let project = variable(PROJECT_ID_VARIABLE).ok_or_else(|| {
            anyhow!(
                "{PROJECT_ID_VARIABLE} is not set; run `{name}` in a GitLab CI pipeline, or set it \
                 to the project's id"
            )
        })?;
        let mut connection = Connection::new(server, project, token_variable, token);
        connection.ca_file = variable(TLS_CA_FILE_VARIABLE);
        Ok(connection)
    }

    /// The connection to `project` on the instance at `server` with the
    /// token `token`, read from the variable `token_variable`.
    pub fn new(server: String, project: String, token_variable: String, token: String) -> Self {
        Connection {
            server,
            project,
            token_variable,
            token: Token(token),
            ca_file: None,
        }
    }
}

/// A GitLab project, reached through the API.
pub struct GitLab {
    client: gitlab::Gitlab,
    /// The instance's URL, without a trailing `/`.
    server: String,
    project: String,
    token_variable: String,
}

impl GitLab {
    /// Connects to the instance and checks that it accepts the token.
    pub fn connect(connection: Connection) -> Result<GitLab> {
        let server = connection.server.trim_end_matches('/');
        let (host, secure) = if let Some(host) = server.strip_prefix("https://") {
            (host, true)
        } else if let Some(host) = server.strip_prefix("http://") {
            (host, false)
        } else {
            bail!(
                "{SERVER_URL_VARIABLE} is `{server}`, which is not an http or https URL of a \
                 GitLab instance"
            );
        };
        let mut builder = GitlabBuilder::new(host, connection.token.0.as_str());
        builder.user_agent(concat!("cutwater/", env!("CARGO_PKG_VERSION")));
        if !secure {
            builder.insecure();
        }
        if let Some(file) = &connection.ca_file {
            let pem = std::fs::read(file)
                .with_context(|| format!("cannot read {TLS_CA_FILE_VARIABLE}, {file}"))?;
            let bundle = CertificateBundle::pem_bundle(&pem).with_context(|| {
                format!("{TLS_CA_FILE_VARIABLE}, {file}, holds no PEM certificates")
            })?;
            builder.cert_custom(bundle);
        }
        let client = builder.build().map_err(|err| {
            let refused = match &err {
                gitlab::GitlabError::Api { source } => matches!(status(source), Some(401 | 403)),
                _ => false,
            };
            if refused {
                anyhow!(
                    "GitLab at {server} refuses the token in {}: it is unknown, expired or \
                     revoked, or lacks the `api` scope ({err})",
                    connection.token_variable
                )
            } else {
                anyhow!("cannot reach GitLab at {server}: {err}")
            }
        })?;
        Ok(GitLab {
            client,
            server: server.to_string(),
            project: connection.project,
            token_variable: connection.token_variable,
        })
    }

    fn project(&self) -> NameOrId<'_> {
        match self.project.parse::<u64>() {
            Ok(id) => NameOrId::from(id),
            Err(_) => NameOrId::from(self.project.as_str()),
        }
    }

    /// The error of a request, with what it tried to do.
    fn failed(&self, what: &str, err: ApiError<RestError>) -> anyhow::Error {
        match status(&err) {
            Some(401) => anyhow!(
                "cannot {what}: GitLab refuses the token in {} ({err})",
                self.token_variable
            ),
            Some(403) => anyhow!(
                "cannot {what}: the bot whose token is in {} is not allowed to ({err})",
                self.token_variable
            ),
            _ => anyhow!("cannot {what}: {err}"),
        }
    }

    fn query<T: serde::de::DeserializeOwned>(
        &self,
        what: &str,
        endpoint: &impl Endpoint,
    ) -> Result<T> {
        endpoint
            .query(&self.client)
            .map_err(|err| self.failed(what, err))
    }

    /// Runs a request whose answer does not matter.
    fn send(&self, what: &str, endpoint: impl Endpoint) -> Result<()> {
        api::ignore(endpoint)
            .query(&self.client)
            .map_err(|err| self.failed(what, err))
    }

    /// Like [`Self::query`], with `None` for a resource that does not exist.
    fn query_optional<T: serde::de::DeserializeOwned>(
        &self,
        what: &str,
        endpoint: &impl Endpoint,
    ) -> Result<Option<T>> {
        match endpoint.query(&self.client) {
            Ok(value) => Ok(Some(value)),
            Err(err) if status(&err) == Some(404) => Ok(None),
            Err(err) => Err(self.failed(what, err)),
        }
    }
}

/// The HTTP status of an API error that GitLab answered.
fn status(err: &ApiError<RestError>) -> Option<u16> {
    match err {
        ApiError::GitlabService { status, .. }
        | ApiError::GitlabWithStatus { status, .. }
        | ApiError::GitlabObjectWithStatus { status, .. }
        | ApiError::GitlabUnrecognizedWithStatus { status, .. } => Some(status.as_u16()),
        _ => None,
    }
}

/// The fields of a project that `mr` reads.
#[derive(Deserialize)]
struct ProjectJson {
    merge_method: String,
    /// Absent on tiers without merged results pipelines.
    #[serde(default)]
    merge_pipelines_enabled: Option<bool>,
    /// Absent on tiers without merge trains.
    #[serde(default)]
    merge_trains_enabled: Option<bool>,
}

#[derive(Deserialize)]
struct BranchJson {
    commit: CommitJson,
}

#[derive(Deserialize)]
struct CommitJson {
    id: String,
    #[serde(default)]
    parent_ids: Vec<String>,
}

#[derive(Deserialize)]
struct MergeRequestJson {
    iid: u64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    web_url: String,
    #[serde(default)]
    assignees: Vec<UserJson>,
    #[serde(default)]
    merge_when_pipeline_succeeds: bool,
    source_project_id: u64,
    target_project_id: u64,
}

#[derive(Deserialize)]
struct UserJson {
    username: String,
}

impl From<MergeRequestJson> for MergeRequest {
    fn from(json: MergeRequestJson) -> Self {
        MergeRequest {
            number: json.iid,
            title: json.title,
            description: json.description.unwrap_or_default(),
            url: json.web_url,
            assignees: json
                .assignees
                .into_iter()
                .map(|user| user.username)
                .collect(),
            auto_merge: json.merge_when_pipeline_succeeds,
        }
    }
}

#[derive(Deserialize)]
struct MergeTrainCarJson {
    status: String,
}

/// A merge request's place in its merge train. The `gitlab` crate lists
/// whole trains only.
struct MergeTrainCar<'a> {
    project: NameOrId<'a>,
    merge_request: u64,
}

impl Endpoint for MergeTrainCar<'_> {
    fn method(&self) -> Method {
        Method::GET
    }

    fn endpoint(&self) -> Cow<'static, str> {
        format!(
            "projects/{}/merge_trains/merge_requests/{}",
            self.project, self.merge_request
        )
        .into()
    }
}

#[derive(Deserialize)]
struct BotJson {
    id: u64,
    username: String,
    /// Present only for administrators.
    #[serde(default)]
    is_admin: bool,
}

#[derive(Deserialize)]
struct MemberJson {
    access_level: u32,
}

/// The groups a project is shared with; absent where the API leaves the
/// field out.
#[derive(Deserialize)]
struct SharesJson {
    #[serde(default)]
    shared_with_groups: Option<Vec<ShareJson>>,
}

#[derive(Deserialize)]
struct ShareJson {
    group_id: u64,
    group_access_level: u32,
}

#[derive(Deserialize)]
struct ProtectedTagJson {
    name: String,
    #[serde(default)]
    create_access_levels: Vec<CreateAccessJson>,
}

#[derive(Deserialize)]
struct CreateAccessJson {
    #[serde(default)]
    access_level: Option<u32>,
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    group_id: Option<u64>,
    #[serde(default)]
    deploy_key_id: Option<u64>,
}

impl From<CreateAccessJson> for CreateAccess {
    fn from(json: CreateAccessJson) -> Self {
        if let Some(user) = json.user_id {
            CreateAccess::User(user)
        } else if let Some(group) = json.group_id {
            CreateAccess::Group(group)
        } else if let Some(key) = json.deploy_key_id {
            CreateAccess::DeployKey(key)
        } else {
            CreateAccess::Level(json.access_level.unwrap_or(0))
        }
    }
}

#[derive(Deserialize)]
struct TagJson {
    commit: CommitJson,
}

#[derive(Deserialize)]
struct PipelineJson {
    id: u64,
    status: String,
    web_url: String,
}

#[derive(Deserialize)]
struct PackageJson {
    id: u64,
    name: String,
    version: String,
}

#[derive(Deserialize)]
struct PackageFileJson {
    file_name: String,
    #[serde(default)]
    file_sha256: Option<String>,
}

/// The release of a tag. The `gitlab` crate's endpoint puts the tag into
/// the path unescaped, so that a tag with `/` is never found.
struct ReleaseByTag<'a> {
    project: NameOrId<'a>,
    tag: &'a str,
}

impl Endpoint for ReleaseByTag<'_> {
    fn method(&self) -> Method {
        Method::GET
    }

    fn endpoint(&self) -> Cow<'static, str> {
        format!(
            "projects/{}/releases/{}",
            self.project,
            path_escaped(self.tag)
        )
        .into()
    }
}

/// The generic packages of a name and version. The `gitlab` crate has no
/// filter for the generic package type.
struct GenericPackages<'a> {
    project: NameOrId<'a>,
    name: &'a str,
    version: &'a str,
}

impl Endpoint for GenericPackages<'_> {
    fn method(&self) -> Method {
        Method::GET
    }

    fn endpoint(&self) -> Cow<'static, str> {
        format!("projects/{}/packages", self.project).into()
    }

    fn parameters(&self) -> QueryParams<'_> {
        let mut params = QueryParams::default();
        params
            .push("package_type", "generic")
            .push("package_name", self.name)
            .push("package_version", self.version);
        params
    }
}

impl Pageable for GenericPackages<'_> {}

impl Forge for GitLab {
    fn merge_settings(&self) -> Result<MergeSettings> {
        let endpoint = gitlab::api::projects::Project::builder()
            .project(self.project())
            .build()
            .expect("the project endpoint has its fields");
        let project: ProjectJson = self.query("read the project's merge settings", &endpoint)?;
        let method = match project.merge_method.as_str() {
            "merge" => MergeMethod::Merge,
            "rebase_merge" => MergeMethod::SemiLinear,
            "ff" => MergeMethod::FastForward,
            other => bail!("the project's merge method `{other}` is unknown to cutwater"),
        };
        Ok(MergeSettings {
            method,
            merged_results: project.merge_pipelines_enabled.unwrap_or(false),
            merge_trains: project.merge_trains_enabled.unwrap_or(false),
        })
    }

    fn branch(&self, name: &str) -> Result<Option<Commit>> {
        let endpoint = Branch::builder()
            .project(self.project())
            .branch(name)
            .build()
            .expect("the branch endpoint has its fields");
        let branch: Option<BranchJson> =
            self.query_optional(&format!("read the branch `{name}`"), &endpoint)?;
        Ok(branch.map(|branch| Commit {
            id: branch.commit.id,
            parents: branch.commit.parent_ids,
        }))
    }

    fn open_merge_requests(&self, source: &str, target: &str) -> Result<Vec<MergeRequest>> {
        let endpoint = MergeRequests::builder()
            .project(self.project())
            .state(MergeRequestState::Opened)
            .source_branch(source)
            .target_branch(target)
            .build()
            .expect("the merge requests endpoint has its fields");
        let found: Vec<MergeRequestJson> = api::paged(endpoint, Pagination::All)
            .query(&self.client)
            .map_err(|err| {
                self.failed(
                    &format!("list the open merge requests from `{source}` into `{target}`"),
                    err,
                )
            })?;
        // A fork's branch of the same name is not the release branch.
        Ok(found
            .into_iter()
            .filter(|json| json.source_project_id == json.target_project_id)
            .map(MergeRequest::from)
            .collect())
    }

    fn in_merge_train(&self, merge_request: &MergeRequest) -> Result<bool> {
        let endpoint = MergeTrainCar {
            project: self.project(),
            merge_request: merge_request.number,
        };
        let car: Option<MergeTrainCarJson> = self.query_optional(
            &format!(
                "read the merge train place of merge request !{}",
                merge_request.number
            ),
            &endpoint,
        )?;
        Ok(car.is_some_and(|car| !matches!(car.status.as_str(), "merged" | "skip_merged")))
    }

    fn write_commit(&self, commit: &NewCommit) -> Result<String> {
        let actions = commit.actions.iter().map(|action| {
            let mut builder = CommitAction::builder();
            builder.file_path(action.path.as_str());
            match &action.change {
                FileChange::Create(content) => {
                    builder
                        .action(CommitActionType::Create)
                        .content(content.as_slice());
                }
                FileChange::Update(content) => {
                    builder
                        .action(CommitActionType::Update)
                        .content(content.as_slice());
                }
                FileChange::Delete => {
                    builder.action(CommitActionType::Delete);
                }
            }
            builder.build().expect("a commit action has its content")
        });
        let endpoint = CreateCommit::builder()
            .project(self.project())
            .branch(commit.branch)
            .commit_message(commit.message)
            .start_sha(commit.parent)
            .force(true)
            .actions(actions)
            .build()
            .expect("the commit endpoint has its fields");
        let written: CommitJson = self.query(
            &format!("write the release commit onto `{}`", commit.branch),
            &endpoint,
        )?;
        Ok(written.id)
    }

    fn create_merge_request(
        &self,
        source: &str,
        target: &str,
        title: &str,
        description: &str,
    ) -> Result<MergeRequest> {
        let endpoint = CreateMergeRequest::builder()
            .project(self.project())
            .source_branch(source)
            .target_branch(target)
            .title(title)
            .description(description)
            .build()
            .expect("the merge request endpoint has its fields");
        let created: MergeRequestJson =
            self.query("create the release merge request", &endpoint)?;
        Ok(created.into())
    }

    fn edit_merge_request(&self, number: u64, title: &str, description: &str) -> Result<()> {
        let endpoint = EditMergeRequest::builder()
            .project(self.project())
            .merge_request(number)
            .title(title)
            .description(description)
            .build()
            .expect("the merge request endpoint has its fields");
        self.send(
            &format!("update the release merge request !{number}"),
            endpoint,
        )?;
        Ok(())
    }

    fn close_merge_request(&self, number: u64, note: &str) -> Result<()> {
        let endpoint = CreateMergeRequestNote::builder()
            .project(self.project())
            .merge_request(number)
            .body(note)
            .build()
            .expect("the note endpoint has its fields");
        self.send(
            &format!("comment on the release merge request !{number}"),
            endpoint,
        )?;
        let endpoint = EditMergeRequest::builder()
            .project(self.project())
            .merge_request(number)
            .state_event(MergeRequestStateEvent::Close)
            .build()
            .expect("the merge request endpoint has its fields");
        self.send(
            &format!("close the release merge request !{number}"),
            endpoint,
        )?;
        Ok(())
    }

    fn bot(&self) -> Result<Bot> {
        let endpoint = CurrentUser::builder()
            .build()
            .expect("the user endpoint has its fields");
        let bot: BotJson = self.query("read the bot's account", &endpoint)?;
        Ok(Bot {
            id: bot.id,
            username: bot.username,
            admin: bot.is_admin,
        })
    }

    fn access_level(&self, user: u64) -> Result<u32> {
        let endpoint = AllProjectMember::builder()
            .project(self.project())
            .user(user)
            .build()
            .expect("the member endpoint has its fields");
        let member: Option<MemberJson> =
            self.query_optional("read the bot's access level in the project", &endpoint)?;
        Ok(member.map_or(0, |member| member.access_level))
    }

    fn direct_group_member(&self, group: u64, user: u64) -> Result<Option<u32>> {
        let endpoint = GroupMember::builder()
            .group(group)
            .user(user)
            .build()
            .expect("the group member endpoint has its fields");
        let member: Option<MemberJson> = self.query_optional(
            &format!("read the bot's membership of group {group}"),
            &endpoint,
        )?;
        Ok(member.map(|member| member.access_level))
    }

    fn shared_groups(&self) -> Result<Option<Vec<GroupShare>>> {
        let endpoint = gitlab::api::projects::Project::builder()
            .project(self.project())
            .build()
            .expect("the project endpoint has its fields");
        let project: SharesJson =
            self.query("read the groups the project is shared with", &endpoint)?;
        Ok(project.shared_with_groups.map(|shares| {
            shares
                .into_iter()
                .map(|share| GroupShare {
                    group: share.group_id,
                    access: share.group_access_level,
                })
                .collect()
        }))
    }

    fn protected_tags(&self) -> Result<Vec<ProtectedTag>> {
        let endpoint = ProtectedTags::builder()
            .project(self.project())
            .build()
            .expect("the protected tags endpoint has its fields");
        let found: Vec<ProtectedTagJson> = api::paged(endpoint, Pagination::All)
            .query(&self.client)
            .map_err(|err| self.failed("list the project's protected tags", err))?;
        Ok(found
            .into_iter()
            .map(|json| ProtectedTag {
                pattern: json.name,
                create: json
                    .create_access_levels
                    .into_iter()
                    .map(CreateAccess::from)
                    .collect(),
            })
            .collect())
    }

    fn tag(&self, name: &str) -> Result<Option<String>> {
        let endpoint = Tag::builder()
            .project(self.project())
            .tag_name(name)
            .build()
            .expect("the tag endpoint has its fields");
        let tag: Option<TagJson> =
            self.query_optional(&format!("read the tag `{name}`"), &endpoint)?;
        Ok(tag.map(|tag| tag.commit.id))
    }

    fn has_release(&self, tag: &str) -> Result<bool> {
        let endpoint = ReleaseByTag {
            project: self.project(),
            tag,
        };
        let release: Option<serde_json::Value> =
            self.query_optional(&format!("read the release of `{tag}`"), &endpoint)?;
        Ok(release.is_some())
    }

    fn latest_pipeline(&self, commit: &str, branch: &str) -> Result<Option<Pipeline>> {
        let endpoint = Pipelines::builder()
            .project(self.project())
            .sha(commit)
            .ref_(branch)
            .order_by(PipelineOrderBy::Id)
            .sort(SortOrder::Descending)
            .build()
            .expect("the pipelines endpoint has its fields");
        let found: Vec<PipelineJson> = api::paged(endpoint, Pagination::Limit(1))
            .query(&self.client)
            .map_err(|err| self.failed(&format!("list the pipelines of {commit}"), err))?;
        Ok(found.into_iter().next().map(|json| Pipeline {
            id: json.id,
            status: json.status,
            url: json.web_url,
        }))
    }

    fn package_files(&self, package: &str, version: &str) -> Result<Vec<PackageFile>> {
        let what = format!("list the files of the generic package `{package}` {version}");
        let endpoint = GenericPackages {
            project: self.project(),
            name: package,
            version,
        };
        let packages: Vec<PackageJson> = api::paged(endpoint, Pagination::All)
            .query(&self.client)
            .map_err(|err| self.failed(&what, err))?;
        let mut files = Vec::new();
        // The name filter matches parts of names.
        for found in packages
            .iter()
            .filter(|found| found.name == package && found.version == version)
        {
            let endpoint = PackageFiles::builder()
                .project(self.project())
                .package_id(found.id)
                .build()
                .expect("the package files endpoint has its fields");
            let listed: Vec<PackageFileJson> = api::paged(endpoint, Pagination::All)
                .query(&self.client)
                .map_err(|err| self.failed(&what, err))?;
            for file in listed {
                let sha256 = file.file_sha256.ok_or_else(|| {
                    anyhow!(
                        "cannot {what}: GitLab gives no SHA-256 checksum of `{}`",
                        file.file_name
                    )
                })?;
                files.push(PackageFile {
                    name: file.file_name,
                    sha256: sha256.to_ascii_lowercase(),
                });
            }
        }
        Ok(files)
    }

    fn upload_package_file(
        &self,
        package: &str,
        version: &str,
        file: &str,
        content: &[u8],
    ) -> Result<()> {
        let endpoint = UploadPackageFile::builder()
            .project(self.project())
            .package_name(package)
            .package_version(version)
            .file_name(file)
            .contents(content)
            .build()
            .expect("the upload endpoint has its fields");
        self.send(
            &format!("upload `{file}` to the generic package `{package}` {version}"),
            endpoint,
        )
    }

    fn package_file_url(&self, package: &str, version: &str, file: &str) -> String {
        format!(
            "{}/api/v4/projects/{}/packages/generic/{}/{}/{}",
            self.server,
            self.project(),
            path_escaped(package),
            path_escaped(version),
            path_escaped(file)
        )
    }

    fn create_release(&self, release: &NewRelease) -> Result<()> {
        let links = release.links.iter().map(|link| {
            CreateReleaseAssetLinks::builder()
                .name(link.name.as_str())
                .url(link.url.as_str())
                .direct_asset_path(link.path.as_str())
                .link_type(LinkType::Package)
                .build()
                .expect("a release link has its fields")
        });
        let endpoint = CreateRelease::builder()
            .project(self.project())
            .tag_name(release.tag)
            .ref_sha(release.commit)
            .name(release.name)
            .description(release.notes)
            .assets(links)
            .build()
            .expect("the release endpoint has its fields");
        self.send(&format!("create the release `{}`", release.tag), endpoint)
    }
}

#[cfg(test)]
mod tests {
    use mockito::{Matcher, Server, ServerGuard};

    use super::*;
    use crate::forge::FileAction;

    const TOKEN: &str = "glpat-unit-test-token";

    /// A server that accepts the token, and the forge of project 7 on it.
    fn forge() -> (ServerGuard, GitLab) {
        let mut server = Server::new();
        server
            .mock("GET", "/api/v4/user")
            .match_query(Matcher::Any)
            .match_header("private-token", TOKEN)
            .with_body(r#"{"id": 1, "username": "release-bot"}"#)
            .create();
        let forge = GitLab::connect(Connection::new(
            server.url(),
            "7".to_string(),
            "CUTWATER_GITLAB_TOKEN".to_string(),
            TOKEN.to_string(),
        ))
        .unwrap();
        (server, forge)
    }

    fn merge_request_json(iid: u64, source_project: u64) -> String {
        format!(
            r#"{{"iid": {iid}, "title": "Release", "description": "notes",
                "web_url": "https://gitlab.example/p/-/merge_requests/{iid}",
                "assignees": [{{"username": "queue-bot"}}],
                "merge_when_pipeline_succeeds": true,
                "source_project_id": {source_project}, "target_project_id": 7}}"#
        )
    }

    #[test]
    fn a_refused_token_is_named_by_its_variable_and_never_shown() {
        let mut server = Server::new();
        server
            .mock("GET", "/api/v4/user")
            .match_query(Matcher::Any)
            .with_status(401)
            .with_body(r#"{"message": "401 Unauthorized"}"#)
            .create();
        let err = GitLab::connect(Connection::new(
            server.url(),
            "7".to_string(),
            "BOT_TOKEN".to_string(),
            TOKEN.to_string(),
        ))
        .err()
        .unwrap();
        let message = format!("{err:#}");
        assert!(
            message.contains("refuses the token in BOT_TOKEN"),
            "{message}"
        );
        assert!(!message.contains(TOKEN), "{message}");
        let connection = Connection::new(
            server.url(),
            "7".to_string(),
            "BOT_TOKEN".to_string(),
            TOKEN.to_string(),
        );
        assert!(!format!("{connection:?}").contains(TOKEN));
    }

    #[test]
    fn merge_settings() {
        let (mut server, forge) = forge();
        let cases = [
            (
                r#"{"merge_method": "merge"}"#,
                MergeMethod::Merge,
                false,
                false,
            ),
            (
                r#"{"merge_method": "rebase_merge", "merge_pipelines_enabled": true}"#,
                MergeMethod::SemiLinear,
                true,
                false,
            ),
            (
                r#"{"merge_method": "ff", "merge_pipelines_enabled": false, "merge_trains_enabled": true}"#,
                MergeMethod::FastForward,
                false,
                true,
            ),
        ];
        for (body, method, merged_results, merge_trains) in cases {
            let mock = server
                .mock("GET", "/api/v4/projects/7")
                .match_query(Matcher::Any)
                .with_body(body)
                .create();
            assert_eq!(
                forge.merge_settings().unwrap(),
                MergeSettings {
                    method,
                    merged_results,
                    merge_trains
                }
            );
            mock.remove();
        }
    }

    #[test]
    fn branch_and_missing_branch() {
        let (mut server, forge) = forge();
        server
            .mock("GET", "/api/v4/projects/7/repository/branches/main")
            .match_query(Matcher::Any)
            .with_body(r#"{"name": "main", "commit": {"id": "aaa", "parent_ids": ["999"]}}"#)
            .create();
        server
            .mock(
                "GET",
                "/api/v4/projects/7/repository/branches/cutwater%2Frelease",
            )
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Branch Not Found"}"#)
            .create();
        assert_eq!(
            forge.branch("main").unwrap(),
            Some(Commit {
                id: "aaa".to_string(),
                parents: vec!["999".to_string()],
            })
        );
        assert_eq!(forge.branch("cutwater/release").unwrap(), None);
    }

    #[test]
    fn open_merge_requests_of_the_project_only() {
        let (mut server, forge) = forge();
        server
            .mock("GET", "/api/v4/projects/7/merge_requests")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("state".into(), "opened".into()),
                Matcher::UrlEncoded("source_branch".into(), "cutwater/release".into()),
                Matcher::UrlEncoded("target_branch".into(), "main".into()),
            ]))
            .with_body(format!(
                "[{}, {}]",
                merge_request_json(3, 7),
                merge_request_json(4, 99)
            ))
            .create();
        let found = forge
            .open_merge_requests("cutwater/release", "main")
            .unwrap();
        assert_eq!(
            found,
            [MergeRequest {
                number: 3,
                title: "Release".to_string(),
                description: "notes".to_string(),
                url: "https://gitlab.example/p/-/merge_requests/3".to_string(),
                assignees: vec!["queue-bot".to_string()],
                auto_merge: true,
            }]
        );
    }

    #[test]
    fn merge_train_membership() {
        let (mut server, forge) = forge();
        server
            .mock("GET", "/api/v4/projects/7/merge_trains/merge_requests/3")
            .match_query(Matcher::Any)
            .with_body(r#"{"id": 1, "status": "fresh"}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/merge_trains/merge_requests/4")
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Not found"}"#)
            .create();
        let merge_request = |number| MergeRequest {
            number,
            title: String::new(),
            description: String::new(),
            url: String::new(),
            assignees: Vec::new(),
            auto_merge: false,
        };
        assert!(forge.in_merge_train(&merge_request(3)).unwrap());
        assert!(!forge.in_merge_train(&merge_request(4)).unwrap());
    }

    #[test]
    fn write_commit_starts_from_the_parent_and_forces_the_branch() {
        let (mut server, forge) = forge();
        let mock = server
            .mock("POST", "/api/v4/projects/7/repository/commits")
            .match_query(Matcher::Any)
            .match_request(|request| {
                let form: Vec<(String, String)> = form_urlencoded::parse(request.body().unwrap())
                    .into_owned()
                    .collect();
                let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
                let actions: Vec<_> = form
                    .iter()
                    .filter(|(key, _)| key.starts_with("actions[]"))
                    .cloned()
                    .collect();
                [
                    pair("branch", "cutwater/release"),
                    pair("start_sha", "aaa"),
                    pair("force", "true"),
                    pair("commit_message", "Release\n"),
                ]
                .iter()
                .all(|expected| form.contains(expected))
                    && actions
                        == [
                            pair("actions[][action]", "update"),
                            pair("actions[][file_path]", "api/package.json"),
                            pair("actions[][content]", "{}\n"),
                            pair("actions[][encoding]", "text"),
                            pair("actions[][action]", "create"),
                            pair("actions[][file_path]", "api/CHANGELOG.md"),
                            pair("actions[][content]", "# api\n"),
                            pair("actions[][encoding]", "text"),
                            pair("actions[][action]", "delete"),
                            pair("actions[][file_path]", ".changeset/a.md"),
                        ]
            })
            .with_status(201)
            .with_body(r#"{"id": "bbb", "parent_ids": ["aaa"]}"#)
            .create();
        let actions = [
            FileAction {
                path: "api/package.json".to_string(),
                change: FileChange::Update(b"{}\n".to_vec()),
            },
            FileAction {
                path: "api/CHANGELOG.md".to_string(),
                change: FileChange::Create(b"# api\n".to_vec()),
            },
            FileAction {
                path: ".changeset/a.md".to_string(),
                change: FileChange::Delete,
            },
        ];
        let written = forge
            .write_commit(&NewCommit {
                branch: "cutwater/release",
                parent: "aaa",
                message: "Release\n",
                actions: &actions,
            })
            .unwrap();
        assert_eq!(written, "bbb");
        mock.assert();
    }

    #[test]
    fn create_edit_and_close_merge_requests() {
        let (mut server, forge) = forge();
        let create = server
            .mock("POST", "/api/v4/projects/7/merge_requests")
            .match_query(Matcher::Any)
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("source_branch".into(), "cutwater/release".into()),
                Matcher::UrlEncoded("target_branch".into(), "main".into()),
                Matcher::UrlEncoded("title".into(), "Release".into()),
                Matcher::UrlEncoded("description".into(), "notes".into()),
            ]))
            .with_status(201)
            .with_body(merge_request_json(3, 7))
            .create();
        let edit = server
            .mock("PUT", "/api/v4/projects/7/merge_requests/3")
            .match_query(Matcher::Any)
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("title".into(), "Release 2".into()),
                Matcher::UrlEncoded("description".into(), "more notes".into()),
            ]))
            .with_body(merge_request_json(3, 7))
            .create();
        let note = server
            .mock("POST", "/api/v4/projects/7/merge_requests/3/notes")
            .match_query(Matcher::Any)
            .match_body(Matcher::UrlEncoded(
                "body".into(),
                "Nothing pending.".into(),
            ))
            .with_status(201)
            .with_body(r#"{"id": 11}"#)
            .create();
        let close = server
            .mock("PUT", "/api/v4/projects/7/merge_requests/3")
            .match_query(Matcher::Any)
            .match_body(Matcher::UrlEncoded("state_event".into(), "close".into()))
            .with_body(merge_request_json(3, 7))
            .create();

        let created = forge
            .create_merge_request("cutwater/release", "main", "Release", "notes")
            .unwrap();
        assert_eq!(created.number, 3);
        forge
            .edit_merge_request(3, "Release 2", "more notes")
            .unwrap();
        forge.close_merge_request(3, "Nothing pending.").unwrap();
        for mock in [create, edit, note, close] {
            mock.assert();
        }
    }

    #[test]
    fn a_forbidden_write_names_the_token_variable() {
        let (mut server, forge) = forge();
        server
            .mock("POST", "/api/v4/projects/7/merge_requests")
            .match_query(Matcher::Any)
            .with_status(403)
            .with_body(r#"{"message": "403 Forbidden"}"#)
            .create();
        let message = format!(
            "{:#}",
            forge
                .create_merge_request("cutwater/release", "main", "Release", "")
                .unwrap_err()
        );
        assert!(
            message.contains("create the release merge request")
                && message.contains("CUTWATER_GITLAB_TOKEN"),
            "{message}"
        );
        assert!(!message.contains(TOKEN), "{message}");
    }

    #[test]
    fn bot_access_and_protected_tags() {
        let (mut server, forge) = forge();
        server
            .mock("GET", "/api/v4/projects/7/members/all/1")
            .match_query(Matcher::Any)
            .with_body(r#"{"id": 1, "username": "release-bot", "access_level": 30}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/members/all/2")
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Not found"}"#)
            .create();
        server
            .mock("GET", "/api/v4/groups/40/members/1")
            .match_query(Matcher::Any)
            .with_body(r#"{"id": 1, "access_level": 30}"#)
            .create();
        server
            .mock("GET", "/api/v4/groups/41/members/1")
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Not found"}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/protected_tags")
            .match_query(Matcher::Any)
            .with_body(
                r#"[{"name": "*-v*", "create_access_levels": [
                    {"id": 1, "access_level": 40, "access_level_description": "Maintainers"},
                    {"id": 2, "access_level": 40, "user_id": 9},
                    {"id": 3, "access_level": 40, "group_id": 40},
                    {"id": 4, "access_level": 40, "deploy_key_id": 5}]}]"#,
            )
            .create();
        assert_eq!(
            forge.bot().unwrap(),
            Bot {
                id: 1,
                username: "release-bot".to_string(),
                admin: false
            }
        );
        assert_eq!(forge.access_level(1).unwrap(), 30);
        assert_eq!(forge.access_level(2).unwrap(), 0);
        assert_eq!(forge.direct_group_member(40, 1).unwrap(), Some(30));
        assert_eq!(forge.direct_group_member(41, 1).unwrap(), None);
        let project = server
            .mock("GET", "/api/v4/projects/7")
            .match_query(Matcher::Any)
            .with_body(
                r#"{"id": 7, "shared_with_groups": [{"group_id": 40, "group_name": "Release",
                    "group_full_path": "acme/release", "group_access_level": 30}]}"#,
            )
            .create();
        assert_eq!(
            forge.shared_groups().unwrap(),
            Some(vec![GroupShare {
                group: 40,
                access: 30
            }])
        );
        project.remove();
        server
            .mock("GET", "/api/v4/projects/7")
            .match_query(Matcher::Any)
            .with_body(r#"{"id": 7}"#)
            .create();
        assert_eq!(forge.shared_groups().unwrap(), None);
        assert_eq!(
            forge.protected_tags().unwrap(),
            [ProtectedTag {
                pattern: "*-v*".to_string(),
                create: vec![
                    CreateAccess::Level(40),
                    CreateAccess::User(9),
                    CreateAccess::Group(40),
                    CreateAccess::DeployKey(5),
                ],
            }]
        );
    }

    #[test]
    fn tags_releases_and_pipelines() {
        let (mut server, forge) = forge();
        server
            .mock("GET", "/api/v4/projects/7/repository/tags/api-v1.4.3")
            .match_query(Matcher::Any)
            .with_body(r#"{"name": "api-v1.4.3", "target": "fff", "commit": {"id": "aaa", "parent_ids": []}}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/repository/tags/api-v1.4.4")
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Tag Not Found"}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/releases/api-v1.4.3")
            .match_query(Matcher::Any)
            .with_body(r#"{"tag_name": "api-v1.4.3"}"#)
            .create();
        server
            .mock(
                "GET",
                "/api/v4/projects/7/repository/tags/@acme%2Fcore@2.0.0",
            )
            .match_query(Matcher::Any)
            .with_body(r#"{"name": "@acme/core@2.0.0", "commit": {"id": "ccc"}}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/releases/@acme%2Fcore@2.0.0")
            .match_query(Matcher::Any)
            .with_body(r#"{"tag_name": "@acme/core@2.0.0"}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/releases/api-v1.4.4")
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Not Found"}"#)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/pipelines")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("sha".into(), "aaa".into()),
                Matcher::UrlEncoded("ref".into(), "main".into()),
                Matcher::UrlEncoded("order_by".into(), "id".into()),
                Matcher::UrlEncoded("sort".into(), "desc".into()),
            ]))
            .with_body(
                r#"[{"id": 12, "status": "running", "web_url": "https://gitlab.example/p/-/pipelines/12"},
                    {"id": 11, "status": "failed", "web_url": "https://gitlab.example/p/-/pipelines/11"}]"#,
            )
            .create();
        server
            .mock("GET", "/api/v4/projects/7/pipelines")
            .match_query(Matcher::UrlEncoded("sha".into(), "bbb".into()))
            .with_body("[]")
            .create();
        assert_eq!(forge.tag("api-v1.4.3").unwrap().as_deref(), Some("aaa"));
        assert_eq!(forge.tag("api-v1.4.4").unwrap(), None);
        assert!(forge.has_release("api-v1.4.3").unwrap());
        assert_eq!(
            forge.tag("@acme/core@2.0.0").unwrap().as_deref(),
            Some("ccc")
        );
        assert!(forge.has_release("@acme/core@2.0.0").unwrap());
        assert!(!forge.has_release("api-v1.4.4").unwrap());
        let pipeline = forge.latest_pipeline("aaa", "main").unwrap().unwrap();
        assert_eq!(
            pipeline,
            Pipeline {
                id: 12,
                status: "running".to_string(),
                url: "https://gitlab.example/p/-/pipelines/12".to_string(),
            }
        );
        assert!(!pipeline.finished());
        assert_eq!(forge.latest_pipeline("bbb", "main").unwrap(), None);
    }

    #[test]
    fn generic_package_files_and_uploads() {
        let (mut server, forge) = forge();
        server
            .mock("GET", "/api/v4/projects/7/packages")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("package_type".into(), "generic".into()),
                Matcher::UrlEncoded("package_name".into(), "cli".into()),
                Matcher::UrlEncoded("package_version".into(), "1.0.0".into()),
            ]))
            .with_body(
                r#"[{"id": 3, "name": "cli", "version": "1.0.0", "package_type": "generic"},
                    {"id": 4, "name": "cli-extra", "version": "1.0.0", "package_type": "generic"}]"#,
            )
            .create();
        server
            .mock("GET", "/api/v4/projects/7/packages/3/package_files")
            .match_query(Matcher::Any)
            .with_body(
                r#"[{"id": 1, "package_id": 3, "file_name": "cli-linux.tar.gz", "size": 3,
                     "file_md5": null, "file_sha1": "x", "file_sha256": "ABC123"}]"#,
            )
            .create();
        let upload = server
            .mock(
                "PUT",
                "/api/v4/projects/7/packages/generic/cli/1.0.0-rc.1+b/cli-linux.tar.gz",
            )
            .match_query(Matcher::Any)
            .match_header("content-type", "application/octet-stream")
            .match_body("bytes")
            .with_status(201)
            .with_body(r#"{"message": "201 Created"}"#)
            .create();
        assert_eq!(
            forge.package_files("cli", "1.0.0").unwrap(),
            [PackageFile {
                name: "cli-linux.tar.gz".to_string(),
                sha256: "abc123".to_string(),
            }]
        );
        forge
            .upload_package_file("cli", "1.0.0-rc.1+b", "cli-linux.tar.gz", b"bytes")
            .unwrap();
        upload.assert();
        assert_eq!(
            forge.package_file_url("cli", "1.0.0", "cli-linux.tar.gz"),
            format!(
                "{}/api/v4/projects/7/packages/generic/cli/1.0.0/cli-linux.tar.gz",
                server.url()
            )
        );
    }

    #[test]
    fn create_release_tags_the_commit_with_notes_and_links() {
        let (mut server, forge) = forge();
        let create = server
            .mock("POST", "/api/v4/projects/7/releases")
            .match_query(Matcher::Any)
            .match_header("content-type", "application/json")
            .match_body(Matcher::Json(serde_json::json!({
                "tag_name": "cli-v1.0.0",
                "ref": "aaa",
                "name": "cli-v1.0.0",
                "description": "### Minor Changes\n\n- Add.",
                "assets": {"links": [{
                    "name": "cli-linux.tar.gz",
                    "url": "https://gitlab.example/api/v4/projects/7/packages/generic/cli/1.0.0/cli-linux.tar.gz",
                    "direct_asset_path": "/cli-linux.tar.gz",
                    "link_type": "package"
                }]}
            })))
            .with_status(201)
            .with_body(r#"{"tag_name": "cli-v1.0.0"}"#)
            .create();
        forge
            .create_release(&NewRelease {
                tag: "cli-v1.0.0",
                commit: "aaa",
                name: "cli-v1.0.0",
                notes: "### Minor Changes\n\n- Add.",
                links: &[crate::forge::ReleaseLink {
                    name: "cli-linux.tar.gz".to_string(),
                    url: "https://gitlab.example/api/v4/projects/7/packages/generic/cli/1.0.0/cli-linux.tar.gz".to_string(),
                    path: "/cli-linux.tar.gz".to_string(),
                }],
            })
            .unwrap();
        create.assert();
    }
}
