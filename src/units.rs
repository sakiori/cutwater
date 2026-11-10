//! The units of a repository and its fixed groups, as the configuration and
//! the adapters' discovery define them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Result, bail};
use bstr::BStr;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::adapter::{AdapterKind, DiscoverRequest};
use crate::config::{CONFIG_FILE, Config, GroupSettings, UnitSettings};

/// Directories of the tool's own files, which no unit ships.
const TOOL_DIRS: [&str; 2] = [".changeset/", ".cutwater/"];

/// What is versioned and released as one.
#[derive(Debug, Clone)]
pub struct Unit {
    pub name: String,
    /// The unit's directory relative to the repository root, with `/`
    /// separators; `.` for the root.
    pub path: String,
    pub adapter: AdapterKind,
    shipped: Shipped,
    /// The template of the unit's own tags; `None` for a member of a fixed
    /// group, which takes the group's.
    pub tag: Option<TagTemplate>,
    /// The package name of the unit's release assets in the generic package
    /// registry.
    pub registry_name: String,
    /// The patterns of the unit's release assets, relative to the
    /// repository root.
    pub assets: Vec<String>,
}

/// The shipped-file patterns of a unit, gitignore-style: a pattern applies
/// to the files and directories it matches, and a directory's match to
/// everything under it; `!` excludes, and of the patterns that apply to a
/// file or any directory above it, the last one decides. Whatever they
/// decide, the files of units nested under the unit are never its own.
#[derive(Debug, Clone)]
struct Shipped {
    patterns: Gitignore,
    /// The patterns by their text, for their order.
    order: HashMap<String, usize>,
    /// Whether a pattern includes; without one, every file under the unit's
    /// path is shipped unless a pattern excludes it.
    includes: bool,
    /// The paths, relative to the unit's path, of the units nested under it.
    nested: Vec<String>,
}

impl Shipped {
    fn ships(&self, relative: &str) -> bool {
        let nested = self.nested.iter().any(|nested| {
            relative
                .strip_prefix(nested.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        });
        if nested {
            return false;
        }
        let path = Path::new(relative);
        let decision = std::iter::once((path, false))
            .chain(path.ancestors().skip(1).map(|dir| (dir, true)))
            .filter(|(dir, _)| !dir.as_os_str().is_empty())
            .filter_map(|(candidate, is_dir)| {
                let glob = match self.patterns.matched(candidate, is_dir) {
                    Match::None => return None,
                    Match::Ignore(glob) | Match::Whitelist(glob) => glob,
                };
                let order = self.order.get(glob.original()).copied().unwrap_or_default();
                Some((order, !glob.is_whitelist()))
            })
            .max_by_key(|(order, _)| *order)
            .map(|(_, included)| included);
        decision.unwrap_or(!self.includes)
    }
}

impl Unit {
    /// Whether the unit ships the file at `file`, relative to the repository
    /// root.
    pub fn ships(&self, file: &str) -> bool {
        if TOOL_DIRS.iter().any(|dir| file.starts_with(dir)) {
            return false;
        }
        let relative = if self.path == "." {
            file
        } else {
            match file
                .strip_prefix(self.path.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
            {
                Some(relative) => relative,
                None => return false,
            }
        };
        self.shipped.ships(relative)
    }
}

/// Units that always share one version and one tag.
#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub members: Vec<String>,
    pub tag: TagTemplate,
}

/// The units of a repository, ordered by name, and its fixed groups.
#[derive(Debug, Clone)]
pub struct Units {
    units: Vec<Unit>,
    groups: Vec<Group>,
}

impl Units {
    /// The units of the repository at `root`: the discovered ones less the
    /// excluded ones, overridden or completed by the listed ones.
    pub fn resolve(root: &Path, config: &Config) -> Result<Units> {
        let mut problems = Vec::new();
        let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();

        for kind in AdapterKind::ALL {
            let settings = config.discovery(kind);
            if !settings.discover {
                continue;
            }
            let discovery = kind.adapter().discover(&DiscoverRequest {
                root: root.to_path_buf(),
            })?;
            for excluded in &settings.exclude {
                if !discovery
                    .units
                    .iter()
                    .any(|unit| unit.name == *excluded.get_ref())
                {
                    problems.push(format!(
                        "{CONFIG_FILE}, line {}: `{kind}.exclude` names `{}`, which {kind} \
                         discovery does not find",
                        config.line(excluded),
                        excluded.as_ref()
                    ));
                }
            }
            for unit in discovery.units {
                if settings
                    .exclude
                    .iter()
                    .any(|name| *name.get_ref() == unit.name)
                {
                    continue;
                }
                let candidate = Candidate {
                    path: unit.path,
                    adapter: kind,
                    listed: None,
                };
                insert(&mut candidates, unit.name, candidate, &mut problems);
            }
        }

        let discovered_paths: HashMap<String, String> = candidates
            .iter()
            .map(|(name, candidate)| (name.clone(), candidate.path.clone()))
            .collect();
        let path_of = |settings: &UnitSettings| {
            settings
                .path
                .clone()
                .or_else(|| discovered_paths.get(settings.name.as_ref()).cloned())
                .unwrap_or_else(|| "no path".to_string())
        };
        let mut listed: HashMap<&str, &UnitSettings> = HashMap::new();
        for settings in &config.units {
            let name = settings.name.as_ref();
            let line = config.line(&settings.name);
            if let Some(first) = listed.insert(name, settings) {
                problems.push(format!(
                    "{CONFIG_FILE}: two units are named `{name}`: `{}` (line {}) and `{}` \
                     (line {line})",
                    path_of(first),
                    config.line(&first.name),
                    path_of(settings)
                ));
                continue;
            }
            let discovered = candidates.get(name);
            let path = match (&settings.path, discovered) {
                (Some(path), _) => match normalize_path(root, path) {
                    Ok(path) => path,
                    Err(problem) => {
                        problems.push(format!(
                            "{CONFIG_FILE}, line {line}: unit `{name}`: {problem}"
                        ));
                        continue;
                    }
                },
                (None, Some(discovered)) => discovered.path.clone(),
                (None, None) => {
                    problems.push(format!(
                        "{CONFIG_FILE}, line {line}: unit `{name}` sets no `path`, and no \
                         adapter discovers a unit of that name"
                    ));
                    continue;
                }
            };
            let adapter = match (settings.adapter, discovered) {
                (Some(adapter), _) => adapter,
                (None, Some(discovered)) => discovered.adapter,
                (None, None) => {
                    problems.push(format!(
                        "{CONFIG_FILE}, line {line}: unit `{name}` sets no `adapter`, and no \
                         adapter discovers a unit of that name"
                    ));
                    continue;
                }
            };
            candidates.insert(
                name.to_string(),
                Candidate {
                    path,
                    adapter,
                    listed: Some(settings),
                },
            );
        }

        let groups = resolve_groups(config, &candidates, &mut problems);
        let single = candidates.len() == 1;
        let mut units = Vec::new();
        for (name, candidate) in &candidates {
            let settings = candidate.listed;
            let grouped = groups.iter().find(|group| group.members.contains(name));
            let tag = match (settings.and_then(|s| s.tag.as_deref()), grouped) {
                (Some(_), Some(group)) => {
                    problems.push(format!(
                        "{CONFIG_FILE}: unit `{name}` sets a `tag`, but it belongs to the fixed \
                         group `{}`, whose tags it takes; remove its `tag`",
                        group.name
                    ));
                    None
                }
                (None, Some(_)) => None,
                (template, None) => {
                    let adapter = candidate.adapter;
                    let adapter_tag = config.discovery(adapter).tag.as_deref();
                    let (template, owner) = match (template, adapter_tag) {
                        (Some(template), _) => (template, format!("the `tag` of unit `{name}`")),
                        (None, Some(template)) => {
                            (template, format!("`{adapter}.tag`, for unit `{name}`"))
                        }
                        (None, None) if single => ("v{version}", String::new()),
                        (None, None) => ("{unit}-v{version}", String::new()),
                    };
                    match TagTemplate::parse(template, Owner::Unit) {
                        Ok(template) => Some(template),
                        Err(problem) => {
                            problems.push(format!("{CONFIG_FILE}: {owner}: {problem}"));
                            None
                        }
                    }
                }
            };
            let shipped = match shipped(settings.and_then(|s| s.shipped.as_deref())) {
                Ok(shipped) => shipped,
                Err(problem) => {
                    problems.push(format!(
                        "{CONFIG_FILE}: the `shipped` patterns of unit `{name}`: {problem}"
                    ));
                    continue;
                }
            };
            let registry_name = match settings.and_then(|s| s.registry_name.as_deref()) {
                Some(set) if is_registry_name(set) => set.to_string(),
                Some(set) => {
                    problems.push(format!(
                        "{CONFIG_FILE}: the `registry-name` of unit `{name}` is `{set}`, but the \
                         generic package registry accepts only letters, digits, `.`, `_`, `-` \
                         and `+`"
                    ));
                    continue;
                }
                None => registry_name(name),
            };
            let assets = settings.and_then(|s| s.assets.clone()).unwrap_or_default();
            if let Some(problem) = assets
                .iter()
                .find_map(|pattern| asset_pattern_problem(pattern))
            {
                problems.push(format!(
                    "{CONFIG_FILE}: the `assets` patterns of unit `{name}`: {problem}"
                ));
                continue;
            }
            units.push(Unit {
                name: name.clone(),
                path: candidate.path.clone(),
                adapter: candidate.adapter,
                shipped,
                tag,
                registry_name,
                assets,
            });
        }
        let paths: Vec<String> = units.iter().map(|unit| unit.path.clone()).collect();
        for unit in &mut units {
            unit.shipped.nested = paths
                .iter()
                .filter_map(|path| nested_under(&unit.path, path))
                .collect();
        }
        check_tags(&units, &groups, &mut problems);
        check_registry_names(&units, &mut problems);

        if !problems.is_empty() {
            bail!("{}", problems.join("\n"));
        }
        Ok(Units { units, groups })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter()
    }

    pub fn get(&self, name: &str) -> Option<&Unit> {
        self.units.iter().find(|unit| unit.name == name)
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// The fixed group `unit` belongs to.
    pub fn group_of(&self, unit: &str) -> Option<&Group> {
        self.groups
            .iter()
            .find(|group| group.members.iter().any(|member| member == unit))
    }
}

struct Candidate<'a> {
    path: String,
    adapter: AdapterKind,
    listed: Option<&'a UnitSettings>,
}

fn insert<'a>(
    candidates: &mut BTreeMap<String, Candidate<'a>>,
    name: String,
    candidate: Candidate<'a>,
    problems: &mut Vec<String>,
) {
    if let Some(first) = candidates.get(&name) {
        problems.push(format!(
            "two units are named `{name}`: `{}` ({}) and `{}` ({}); exclude one in {CONFIG_FILE}",
            first.path, first.adapter, candidate.path, candidate.adapter
        ));
        return;
    }
    candidates.insert(name, candidate);
}

fn resolve_groups(
    config: &Config,
    candidates: &BTreeMap<String, Candidate>,
    problems: &mut Vec<String>,
) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut membership: HashMap<&str, &GroupSettings> = HashMap::new();
    let mut names: HashMap<&str, &GroupSettings> = HashMap::new();
    for settings in &config.groups {
        let name = settings.name.as_ref();
        let line = config.line(&settings.name);
        if let Some(first) = names.insert(name, settings) {
            problems.push(format!(
                "{CONFIG_FILE}: two fixed groups are named `{name}`, at lines {} and {line}",
                config.line(&first.name)
            ));
            continue;
        }
        let mut members: Vec<String> = Vec::new();
        for member in &settings.units {
            if members.contains(member) {
                problems.push(format!(
                    "{CONFIG_FILE}, line {line}: fixed group `{name}` lists unit `{member}` twice"
                ));
                continue;
            }
            if !candidates.contains_key(member) {
                problems.push(format!(
                    "{CONFIG_FILE}, line {line}: fixed group `{name}` lists `{member}`, which \
                     is not a unit"
                ));
            }
            if let Some(other) = membership.insert(member, settings) {
                problems.push(format!(
                    "{CONFIG_FILE}: unit `{member}` is in two fixed groups, `{}` (line {}) and \
                     `{name}` (line {line}); a unit belongs to at most one",
                    other.name.as_ref(),
                    config.line(&other.name)
                ));
            }
            members.push(member.clone());
        }
        if members.len() < 2 {
            problems.push(format!(
                "{CONFIG_FILE}, line {line}: fixed group `{name}` needs two or more units"
            ));
        }
        // A group of every unit is tagged like a repository's single unit.
        let every_unit = candidates.keys().all(|name| members.contains(name));
        let default = if every_unit {
            "v{version}"
        } else {
            "{group}-v{version}"
        };
        let template = settings.tag.as_deref().unwrap_or(default);
        match TagTemplate::parse(template, Owner::Group) {
            Ok(tag) => groups.push(Group {
                name: name.to_string(),
                members,
                tag,
            }),
            Err(problem) => problems.push(format!(
                "{CONFIG_FILE}, line {line}: the `tag` of fixed group `{name}`: {problem}"
            )),
        }
    }
    groups
}

/// Two owners whose templates give the same tag for the same version.
/// A version with every kind of part, to try a template's tags with.
const SAMPLE_VERSION: &str = "1.2.3-rc.1+build.4";

/// Tags must be valid ref names, and no two owners may get the same tags.
fn check_tags(units: &[Unit], groups: &[Group], problems: &mut Vec<String>) {
    let owners = units
        .iter()
        .filter_map(|unit| {
            Some((
                format!("unit `{}`", unit.name),
                unit.tag.as_ref()?.resolve(&unit.name),
            ))
        })
        .chain(groups.iter().map(|group| {
            (
                format!("fixed group `{}`", group.name),
                group.tag.resolve(&group.name),
            )
        }));
    let mut seen: HashMap<String, String> = HashMap::new();
    for (owner, tag) in owners {
        let sample = tag.replace("{version}", SAMPLE_VERSION);
        let reference = format!("refs/tags/{sample}");
        if let Err(err) = gix_validate::reference::name(BStr::new(&reference)) {
            problems.push(format!(
                "{CONFIG_FILE}: {owner} would be tagged `{sample}`, which git refuses as a tag \
                 name: {err}; give it a tag template that makes valid tags"
            ));
            continue;
        }
        if let Some(first) = seen.get(&tag) {
            problems.push(format!(
                "{CONFIG_FILE}: {first} and {owner} would both be tagged `{tag}`; give one a \
                 tag template of its own"
            ));
        } else {
            seen.insert(tag, owner);
        }
    }
}

/// Whether the generic package registry accepts `name` as a package name.
fn is_registry_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(registry_name_char) && !name.chars().all(|c| c == '.')
}

fn registry_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+')
}

/// The default registry name of a unit: its name with every character the
/// generic package registry rejects replaced by `-`.
pub fn registry_name(unit: &str) -> String {
    unit.chars()
        .map(|c| if registry_name_char(c) { c } else { '-' })
        .collect()
}

/// What is wrong with an asset pattern, `None` when it is a glob relative
/// to the repository root.
fn asset_pattern_problem(pattern: &str) -> Option<String> {
    if pattern.starts_with('/') || pattern.split('/').any(|part| part == "..") {
        return Some(format!(
            "`{pattern}` leaves the repository; give it relative to the repository root"
        ));
    }
    globset::Glob::new(pattern)
        .err()
        .map(|err| format!("`{pattern}`: {err}"))
}

/// No two units may share a registry name, as their assets would mix.
fn check_registry_names(units: &[Unit], problems: &mut Vec<String>) {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for unit in units {
        match seen.get(unit.registry_name.as_str()) {
            Some(first) => problems.push(format!(
                "{CONFIG_FILE}: units `{first}` and `{}` both have the registry name `{}` in the \
                 generic package registry; set `registry-name` on one of them",
                unit.name, unit.registry_name
            )),
            None => {
                seen.insert(&unit.registry_name, &unit.name);
            }
        }
    }
}

/// `path` relative to `outer` when it lies strictly under it.
fn nested_under(outer: &str, path: &str) -> Option<String> {
    if outer == "." {
        return (path != ".").then(|| path.to_string());
    }
    path.strip_prefix(outer)?
        .strip_prefix('/')
        .map(str::to_string)
}

fn shipped(patterns: Option<&[String]>) -> Result<Shipped, String> {
    let mut builder = GitignoreBuilder::new(".");
    let mut order = HashMap::new();
    let mut includes = false;
    for (index, pattern) in patterns.unwrap_or_default().iter().enumerate() {
        builder
            .add_line(None, pattern)
            .map_err(|err| format!("`{pattern}`: {err}"))?;
        // The text the matcher keeps as the pattern's original.
        let text = if pattern.ends_with("\\ ") {
            pattern.as_str()
        } else {
            pattern.trim_end()
        };
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        order.insert(text.to_string(), index);
        includes |= !text.starts_with('!');
    }
    Ok(Shipped {
        patterns: builder.build().map_err(|err| err.to_string())?,
        order,
        includes,
        nested: Vec::new(),
    })
}

/// A unit's path as the configuration gives it, made relative to the root
/// with `/` separators; it must name a directory inside the repository.
fn normalize_path(root: &Path, path: &str) -> Result<String, String> {
    if path.starts_with('/') {
        return Err(format!(
            "`path` `{path}` is absolute; give it relative to the repository root"
        ));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("`path` `{path}` lies outside the repository"));
                }
            }
            part => parts.push(part),
        }
    }
    let normalized = if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    };
    if !root.join(&normalized).is_dir() {
        return Err(format!("`path` `{path}` is not a directory"));
    }
    Ok(normalized)
}

/// Whose tags a template makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Unit,
    Group,
}

/// A tag template: text with `{version}` and the owner's name placeholder,
/// `{unit}` or `{group}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagTemplate(String);

impl TagTemplate {
    fn parse(template: &str, owner: Owner) -> Result<Self, String> {
        let name = match owner {
            Owner::Unit => "unit",
            Owner::Group => "group",
        };
        let mut rest = template;
        let mut has_version = false;
        while let Some(start) = rest.find(['{', '}']) {
            if rest[start..].starts_with('}') {
                return Err(format!("`{template}` has a `}}` without a `{{`"));
            }
            let Some(end) = rest[start..].find('}') else {
                return Err(format!("`{template}` has a `{{` without a `}}`"));
            };
            match &rest[start + 1..start + end] {
                "version" => has_version = true,
                placeholder if placeholder == name => {}
                placeholder => {
                    return Err(format!(
                        "`{template}` has the unknown placeholder `{{{placeholder}}}`; \
                         use `{{{name}}}` and `{{version}}`"
                    ));
                }
            }
            rest = &rest[start + end + 1..];
        }
        if !has_version {
            return Err(format!("`{template}` lacks the `{{version}}` placeholder"));
        }
        Ok(Self(template.to_string()))
    }

    /// The template with the owner's name filled in.
    fn resolve(&self, name: &str) -> String {
        self.0.replace("{unit}", name).replace("{group}", name)
    }

    /// The tag of version `version` of the unit or group `name`.
    pub fn render(&self, name: &str, version: &str) -> String {
        self.resolve(name).replace("{version}", version)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(dirs: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for dir in dirs {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        root
    }

    fn resolve(root: &Path, config: &str) -> Result<Units> {
        Units::resolve(root, &Config::parse(config).unwrap())
    }

    fn error(root: &Path, config: &str) -> String {
        format!("{:#}", resolve(root, config).unwrap_err())
    }

    fn write(root: &Path, file: &str, content: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const CORE_AND_API: &str = "[[unit]]\nname = \"core\"\npath = \"core\"\nadapter = \"cargo\"\n\
                                [[unit]]\nname = \"api\"\npath = \"api\"\nadapter = \"npm\"\n";

    #[test]
    fn tag_defaults_with_several_units() {
        let root = repo(&["core", "api"]);
        let units = resolve(root.path(), CORE_AND_API).unwrap();
        let api = units.get("api").unwrap();
        assert_eq!(
            api.tag.as_ref().unwrap().render("api", "1.4.3"),
            "api-v1.4.3"
        );
    }

    #[test]
    fn adapter_tag_template() {
        let root = repo(&["core", "api", "web"]);
        let units = resolve(
            root.path(),
            &format!(
                "[npm]\ntag = \"{{unit}}@{{version}}\"\n{CORE_AND_API}\
                 [[unit]]\nname = \"web\"\npath = \"web\"\nadapter = \"npm\"\ntag = \"web/v{{version}}\"\n"
            ),
        )
        .unwrap();
        let tag = |name: &str| {
            units
                .get(name)
                .unwrap()
                .tag
                .as_ref()
                .unwrap()
                .render(name, "1.0.0")
        };
        assert_eq!(tag("api"), "api@1.0.0");
        assert_eq!(tag("core"), "core-v1.0.0");
        assert_eq!(tag("web"), "web/v1.0.0");
        let message = error(
            root.path(),
            &format!("[cargo]\ntag = \"{{unit}}\"\n{CORE_AND_API}"),
        );
        assert!(
            message.contains("`cargo.tag`, for unit `core`")
                && message.contains("lacks the `{version}` placeholder"),
            "{message}"
        );
    }

    #[test]
    fn tag_default_with_one_unit() {
        let root = repo(&[]);
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"tool\"\npath = \".\"\nadapter = \"cargo\"\n",
        )
        .unwrap();
        let tool = units.get("tool").unwrap();
        assert_eq!(tool.tag.as_ref().unwrap().render("tool", "0.3.0"), "v0.3.0");
    }

    #[test]
    fn fixed_group_takes_the_tags() {
        let root = repo(&["web", "ui", "api"]);
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"web\"\npath = \"web\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"ui\"\npath = \"ui\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"api\"\npath = \"api\"\nadapter = \"npm\"\n\
             [[group]]\nname = \"frontend\"\nunits = [\"web\", \"ui\"]\n",
        )
        .unwrap();
        assert!(units.get("web").unwrap().tag.is_none());
        let group = units.group_of("ui").unwrap();
        assert_eq!(group.tag.render("frontend", "2.2.0"), "frontend-v2.2.0");
    }

    #[test]
    fn group_of_every_unit_is_tagged_like_a_single_unit() {
        let root = repo(&["core", "api"]);
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"core\"\npath = \"core\"\nadapter = \"cargo\"\n\
             [[unit]]\nname = \"api\"\npath = \"api\"\nadapter = \"cargo\"\n\
             [[group]]\nname = \"crates\"\nunits = [\"core\", \"api\"]\n",
        )
        .unwrap();
        let group = units.group_of("api").unwrap();
        assert_eq!(group.tag.render("crates", "2.0.0"), "v2.0.0");
    }

    #[test]
    fn unit_in_two_groups() {
        let root = repo(&["a", "b", "c"]);
        let message = error(
            root.path(),
            "[[unit]]\nname = \"a\"\npath = \"a\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"b\"\npath = \"b\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"c\"\npath = \"c\"\nadapter = \"npm\"\n\
             [[group]]\nname = \"first\"\nunits = [\"a\", \"b\"]\n\
             [[group]]\nname = \"second\"\nunits = [\"b\", \"c\"]\n",
        );
        assert!(
            message.contains("unit `b` is in two fixed groups, `first`")
                && message.contains("`second`"),
            "{message}"
        );
    }

    #[test]
    fn group_problems() {
        let root = repo(&["a", "b"]);
        let message = error(
            root.path(),
            "[[unit]]\nname = \"a\"\npath = \"a\"\nadapter = \"npm\"\ntag = \"a@{version}\"\n\
             [[unit]]\nname = \"b\"\npath = \"b\"\nadapter = \"npm\"\n\
             [[group]]\nname = \"solo\"\nunits = [\"a\"]\n\
             [[group]]\nname = \"ghost\"\nunits = [\"b\", \"missing\"]\ntag = \"{unit}-{version}\"\n",
        );
        for expected in [
            "fixed group `solo` needs two or more units",
            "fixed group `ghost` lists `missing`, which is not a unit",
            "unknown placeholder `{unit}`",
            "unit `a` sets a `tag`, but it belongs to the fixed group `solo`",
        ] {
            assert!(message.contains(expected), "{expected}\n---\n{message}");
        }
    }

    #[test]
    fn tag_template_problems() {
        let root = repo(&["a", "b"]);
        let message = error(
            root.path(),
            "[[unit]]\nname = \"a\"\npath = \"a\"\nadapter = \"npm\"\ntag = \"release\"\n\
             [[unit]]\nname = \"b\"\npath = \"b\"\nadapter = \"npm\"\ntag = \"v{version}\"\n\
             [[unit]]\nname = \"c\"\npath = \"a/c\"\nadapter = \"npm\"\ntag = \"v{version}\"\n",
        );
        assert!(
            message.contains("lacks the `{version}` placeholder"),
            "{message}"
        );
        assert!(message.contains("`a/c` is not a directory"), "{message}");

        let root = repo(&["a", "b"]);
        let message = error(
            root.path(),
            "[[unit]]\nname = \"a\"\npath = \"a\"\nadapter = \"npm\"\ntag = \"v{version}\"\n\
             [[unit]]\nname = \"b\"\npath = \"b\"\nadapter = \"npm\"\ntag = \"v{version}\"\n",
        );
        assert!(
            message.contains("unit `a` and unit `b` would both be tagged `v{version}`"),
            "{message}"
        );
    }

    #[test]
    fn duplicate_discovered_names_name_both_paths() {
        let root = repo(&[]);
        write(
            root.path(),
            "package.json",
            r#"{"private": true, "workspaces": ["js/*"]}"#,
        );
        write(root.path(), "js/core/package.json", r#"{"name": "core"}"#);
        write(
            root.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"rust/*\"]\n",
        );
        write(
            root.path(),
            "rust/core/Cargo.toml",
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\n",
        );
        write(root.path(), "rust/core/src/lib.rs", "");
        let config = "[npm]\ndiscover = true\n[cargo]\ndiscover = true\n";
        let message = error(root.path(), config);
        assert!(
            message.contains("two units are named `core`")
                && message.contains("`js/core`")
                && message.contains("`rust/core`"),
            "{message}"
        );
        let units = resolve(root.path(), &format!("{config}exclude = [\"core\"]\n")).unwrap();
        assert_eq!(units.get("core").unwrap().path, "js/core");
    }

    #[test]
    fn tags_must_be_valid_ref_names() {
        let root = repo(&["a", "b", "c"]);
        let message = error(
            root.path(),
            "[[unit]]\nname = \"my unit\"\npath = \"a\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"b\"\npath = \"b\"\nadapter = \"npm\"\ntag = \"{unit}..{version}\"\n\
             [[unit]]\nname = \"c\"\npath = \"c\"\nadapter = \"npm\"\ntag = \"c/v{version}.lock\"\n",
        );
        for expected in [
            "unit `my unit` would be tagged `my unit-v1.2.3-rc.1+build.4`",
            "unit `b` would be tagged `b..1.2.3-rc.1+build.4`",
            "unit `c` would be tagged `c/v1.2.3-rc.1+build.4.lock`",
        ] {
            assert!(message.contains(expected), "{expected}\n---\n{message}");
        }
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"@acme/web\"\npath = \"a\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"b\"\npath = \"b\"\nadapter = \"npm\"\ntag = \"{unit}@{version}\"\n",
        )
        .unwrap();
        let web = units.get("@acme/web").unwrap();
        assert_eq!(
            web.tag.as_ref().unwrap().render("@acme/web", "1.0.0"),
            "@acme/web-v1.0.0"
        );
    }

    #[test]
    fn duplicate_listed_names() {
        let root = repo(&["a", "b"]);
        let message = error(
            root.path(),
            "[[unit]]\nname = \"a\"\npath = \"a\"\nadapter = \"npm\"\n\
             [[unit]]\nname = \"a\"\npath = \"b\"\nadapter = \"npm\"\n",
        );
        assert!(
            message.contains("two units are named `a`: `a` (line 2) and `b` (line 6)"),
            "{message}"
        );
    }

    #[test]
    fn colliding_registry_names_are_refused() {
        let root = repo(&["b-c", "c", "d"]);
        let units = "[[unit]]\nname = \"@a/b-c\"\npath = \"b-c\"\nadapter = \"npm\"\n\
                     [[unit]]\nname = \"@a-b/c\"\npath = \"c\"\nadapter = \"npm\"\n";
        let message = error(root.path(), units);
        assert!(
            message.contains(
                "units `@a-b/c` and `@a/b-c` both have the registry name `-a-b-c` in the generic \
                 package registry"
            ),
            "{message}"
        );
        let renamed = resolve(
            root.path(),
            &units.replace("path = \"c\"\n", "path = \"c\"\nregistry-name = \"ab-c\"\n"),
        )
        .unwrap();
        assert_eq!(renamed.get("@a/b-c").unwrap().registry_name, "-a-b-c");
        assert_eq!(renamed.get("@a-b/c").unwrap().registry_name, "ab-c");
        let message = error(
            root.path(),
            "[[unit]]\nname = \"d\"\npath = \"d\"\nadapter = \"npm\"\nregistry-name = \"d/e\"\n",
        );
        assert!(
            message.contains("`registry-name` of unit `d` is `d/e`"),
            "{message}"
        );
    }

    #[test]
    fn listed_unit_overrides_a_discovered_one() {
        let root = repo(&[]);
        write(
            root.path(),
            "package.json",
            r#"{"private": true, "workspaces": ["packages/*"]}"#,
        );
        write(
            root.path(),
            "packages/api/package.json",
            r#"{"name": "@acme/api"}"#,
        );
        write(
            root.path(),
            "packages/web/package.json",
            r#"{"name": "@acme/web"}"#,
        );
        let units = resolve(
            root.path(),
            "[npm]\ndiscover = true\n\
             [[unit]]\nname = \"@acme/api\"\nshipped = [\"src/**\", \"package.json\", \"!src/**/*.test.ts\"]\n",
        )
        .unwrap();
        let api = units.get("@acme/api").unwrap();
        assert_eq!(
            (api.path.as_str(), api.adapter),
            ("packages/api", AdapterKind::Npm)
        );
        assert!(api.ships("packages/api/src/index.ts"));
        assert!(api.ships("packages/api/package.json"));
        assert!(!api.ships("packages/api/src/index.test.ts"));
        assert!(!api.ships("packages/api/README.md"));
        assert!(!api.ships("packages/apis/src/index.ts"));
        let web = units.get("@acme/web").unwrap();
        assert!(web.ships("packages/web/README.md"));
    }

    #[test]
    fn exclusion_of_an_unknown_package() {
        let root = repo(&[]);
        write(root.path(), "package.json", r#"{"name": "tool"}"#);
        let message = error(root.path(), "[npm]\ndiscover = true\nexclude = [\"tol\"]\n");
        assert!(
            message.contains("line 3") && message.contains("`tol`"),
            "{message}"
        );
    }

    #[test]
    fn directory_patterns_cover_their_subtree() {
        let root = repo(&[]);
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"a\"\npath = \".\"\nadapter = \"npm\"\n\
             shipped = [\"src\", \"/package.json\", \"!src/generated\"]\n\
             [[unit]]\nname = \"b\"\npath = \".\"\nadapter = \"cargo\"\n\
             shipped = [\"!src/generated\", \"src/\"]\n",
        )
        .unwrap();
        let a = units.get("a").unwrap();
        assert!(a.ships("src/index.ts") && a.ships("src/deep/er/x.ts"));
        assert!(a.ships("package.json") && !a.ships("lib/package.json"));
        assert!(!a.ships("src/generated/types.ts"));
        assert!(!a.ships("docs/src.md"));
        let b = units.get("b").unwrap();
        assert!(
            b.ships("src/generated/types.ts"),
            "the later pattern decides"
        );
    }

    #[test]
    fn patterns_never_reach_into_nested_units() {
        let root = repo(&["crates/core"]);
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"app\"\npath = \".\"\nadapter = \"cargo\"\n\
             shipped = [\"src/**\", \"crates/core/src/**\"]\n\
             [[unit]]\nname = \"lean\"\npath = \".\"\nadapter = \"npm\"\nshipped = [\"!*.md\"]\n\
             [[unit]]\nname = \"core\"\npath = \"crates/core\"\nadapter = \"cargo\"\n",
        )
        .unwrap();
        let app = units.get("app").unwrap();
        assert!(app.ships("src/main.rs"));
        assert!(!app.ships("crates/core/src/lib.rs"));
        let lean = units.get("lean").unwrap();
        assert!(lean.ships("src/main.ts") && !lean.ships("README.md"));
        assert!(!lean.ships("crates/core/src/lib.rs"));
        assert!(units.get("core").unwrap().ships("crates/core/src/lib.rs"));
    }

    #[test]
    fn root_unit_ships_everything_but_the_tool_files() {
        let root = repo(&[]);
        let units = resolve(
            root.path(),
            "[[unit]]\nname = \"tool\"\npath = \".\"\nadapter = \"cargo\"\n",
        )
        .unwrap();
        let tool = units.get("tool").unwrap();
        assert!(tool.ships("src/main.rs"));
        assert!(!tool.ships(".changeset/brave-lions-dance.md"));
        assert!(!tool.ships(".cutwater/config.toml"));
    }
}
