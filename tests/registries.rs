//! `publish` against real registries on this machine: Verdaccio for npm and
//! cargo-http-registry for Cargo, each started for one test, with no access
//! to any other registry. The tests are ignored by default; CI's
//! `test-registries` job runs them with the tools that
//! `.gitlab/scripts/install-tools.sh` and `.gitlab/verdaccio` pin, named by
//! `CUTWATER_TEST_VERDACCIO` and `CUTWATER_TEST_CARGO_HTTP_REGISTRY`.

mod common;

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::Repo;
use common::publish::{GitLab, TOKEN};
use predicates::str::contains;
use serde_json::json;

/// How long a registry may take to start.
const START_TIMEOUT: Duration = Duration::from_secs(60);

fn tool(variable: &str) -> PathBuf {
    let path = std::env::var_os(variable)
        .unwrap_or_else(|| panic!("{variable} names no executable; see the module's comment"));
    PathBuf::from(path)
}

/// A port nothing listens on now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A registry process, stopped when dropped.
struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Verdaccio without uplinks, with one user whose token is in `npmrc`.
struct Verdaccio {
    _server: Server,
    dir: tempfile::TempDir,
    url: String,
}

impl Verdaccio {
    fn start() -> Verdaccio {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(
            &config,
            "storage: ./storage\nauth:\n  htpasswd:\n    file: ./htpasswd\nuplinks: {}\n\
             packages:\n  '**':\n    access: $all\n    publish: $authenticated\n\
             log: { type: stdout, format: pretty, level: info }\n",
        )
        .unwrap();
        let port = free_port();
        let mut child = Command::new(tool("CUTWATER_TEST_VERDACCIO"))
            .arg("--config")
            .arg(&config)
            .arg("--listen")
            .arg(format!("127.0.0.1:{port}"))
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Server { child };
        let (started, ready) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.contains("server started") {
                    let _ = started.send(());
                }
            }
        });
        ready
            .recv_timeout(START_TIMEOUT)
            .expect("Verdaccio did not start");
        let url = format!("http://127.0.0.1:{port}/");
        // Verdaccio creates the user and answers with its token.
        let token = Command::new("node")
            .arg("-e")
            .arg(
                "fetch(process.argv[1] + '-/user/org.couchdb.user:ci', {method: 'PUT', \
                 headers: {'content-type': 'application/json'}, body: JSON.stringify({name: \
                 'ci', password: 'registry-test-password'})}).then(r => r.json()).then(j => \
                 { if (!j.token) { console.error(JSON.stringify(j)); process.exit(1) } \
                 console.log(j.token) })",
            )
            .arg(&url)
            .output()
            .unwrap();
        assert!(token.status.success(), "{token:?}");
        let token = String::from_utf8(token.stdout).unwrap();
        std::fs::write(
            dir.path().join("npmrc"),
            format!(
                "//127.0.0.1:{port}/:_authToken={}\nregistry={url}\n",
                token.trim()
            ),
        )
        .unwrap();
        Verdaccio {
            _server: server,
            dir,
            url,
        }
    }

    fn npmrc(&self) -> PathBuf {
        self.dir.path().join("npmrc")
    }

    /// The versions and distribution tags `npm view` reports for `name`.
    fn view(&self, name: &str) -> serde_json::Value {
        let output = Command::new("npm")
            .args(["view", name, "versions", "dist-tags", "--json"])
            .env("npm_config_userconfig", self.npmrc())
            .env("npm_config_registry", &self.url)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

/// cargo-http-registry, as the registry `local` of a Cargo home of its own.
struct CargoRegistry {
    _server: Server,
    dir: tempfile::TempDir,
}

impl CargoRegistry {
    fn start() -> CargoRegistry {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("index");
        let port = free_port();
        let child = Command::new(tool("CUTWATER_TEST_CARGO_HTTP_REGISTRY"))
            .arg("--addr")
            .arg(format!("127.0.0.1:{port}"))
            .arg(&index)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let server = Server { child };
        // The registry binds its port before it sets up its index and state
        // but answers requests only after, so any HTTP answer means ready.
        let deadline = Instant::now() + START_TIMEOUT;
        let mut stream = loop {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => break stream,
                Err(err) => {
                    assert!(
                        Instant::now() < deadline,
                        "cargo-http-registry did not listen: {err}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        stream.set_read_timeout(Some(START_TIMEOUT)).unwrap();
        std::io::Write::write_all(&mut stream, b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut status = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut status)
            .expect("cargo-http-registry did not answer");
        assert!(status.starts_with("HTTP/"), "{status}");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            format!(
                "[registries.local]\nindex = \"file://{}\"\n",
                index.display()
            ),
        )
        .unwrap();
        CargoRegistry {
            _server: server,
            dir,
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// Whether the registry's index lists `version` of `name`.
    fn lists(&self, name: &str, version: &str) -> bool {
        let index = self.dir.path().join("index");
        let file = match name.len() {
            1 => index.join("1").join(name),
            2 => index.join("2").join(name),
            3 => index.join("3").join(&name[..1]).join(name),
            _ => index.join(&name[..2]).join(&name[2..4]).join(name),
        };
        std::fs::read_to_string(file).is_ok_and(|text| {
            text.lines().any(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .is_ok_and(|entry| entry["vers"] == version)
            })
        })
    }
}

fn publish(repo: &Repo, gitlab: &GitLab, dir: &Path) -> assert_cmd::Command {
    let mut command = repo.cutwater_in(dir);
    command
        .arg("publish")
        .arg("--json")
        .env("CI_SERVER_URL", gitlab.url())
        .env("CI_PROJECT_ID", "7")
        .env("CUTWATER_GITLAB_TOKEN", TOKEN)
        .env("npm_config_offline", "false")
        .env("CARGO_NET_OFFLINE", "false");
    command
}

fn states(output: &[u8]) -> Vec<(String, String)> {
    let record: serde_json::Value = serde_json::from_slice(output).unwrap();
    record["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| {
            (
                unit["unit"].as_str().unwrap().to_string(),
                unit["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[test]
#[ignore = "needs Verdaccio; CI's test-registries job runs it"]
fn npm_workspace_to_verdaccio() {
    let registry = Verdaccio::start();
    let package = |name: &str, version: &str, dependencies: &str| {
        format!(
            "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"dependencies\": \
             {{{dependencies}}},\n  \"publishConfig\": {{ \"registry\": \"{}\" }}\n}}\n",
            registry.url
        )
    };
    let repo = Repo::with(
        "[npm]\ndiscover = true\n",
        &[
            (
                "package.json",
                "{\n  \"name\": \"root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
            ),
            (
                ".cutwater/versions.toml",
                "[units]\n\"@acme/api\" = \"1.4.3\"\n\"@acme/core\" = \"2.0.0\"\n",
            ),
            (
                "packages/core/package.json",
                &package("@acme/core", "2.0.0", ""),
            ),
            (
                "packages/api/package.json",
                &package("@acme/api", "1.4.3", "\"@acme/core\": \"^2.0.0\""),
            ),
            (
                "packages/core/CHANGELOG.md",
                "# @acme/core\n\n## 2.0.0\n\n- Core.\n",
            ),
            (
                "packages/api/CHANGELOG.md",
                "# @acme/api\n\n## 1.4.3\n\n- Api.\n",
            ),
        ],
    );
    let mut gitlab = GitLab::new();
    gitlab.no_tag("@acme/core-v2.0.0");
    gitlab.no_tag("@acme/api-v1.4.3");
    let (create, _) = gitlab.releases();
    let assert = publish(&repo, &gitlab, repo.path())
        .env("npm_config_userconfig", registry.npmrc())
        .assert()
        .success();
    create.expect(2).assert();
    assert_eq!(
        states(&assert.get_output().stdout),
        [
            pair("@acme/core", "published"),
            pair("@acme/api", "published")
        ]
    );
    assert_eq!(
        registry.view("@acme/api"),
        json!({"versions": ["1.4.3"], "dist-tags": {"latest": "1.4.3"}})
    );

    // A retry finds both published and only tags them.
    let mut gitlab = GitLab::new();
    gitlab.no_tag("@acme/core-v2.0.0");
    gitlab.no_tag("@acme/api-v1.4.3");
    let (create, _) = gitlab.releases();
    let assert = publish(&repo, &gitlab, repo.path())
        .env("npm_config_userconfig", registry.npmrc())
        .assert()
        .success()
        .stderr(contains(format!("is in the npm registry {}", registry.url)));
    create.expect(2).assert();
    assert_eq!(
        states(&assert.get_output().stdout),
        [pair("@acme/core", "present"), pair("@acme/api", "present")]
    );
}

#[test]
#[ignore = "needs cargo-http-registry; CI's test-registries job runs it"]
fn cargo_workspace_to_a_registry() {
    let registry = CargoRegistry::start();
    let manifest = |name: &str, dependencies: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"2.0.0\"\nedition = \"2021\"\n\
             description = \"A test crate.\"\nlicense = \"MIT\"\npublish = [\"local\"]\n\n\
             [dependencies]\n{dependencies}"
        )
    };
    let repo = Repo::with(
        "[cargo]\ndiscover = true\n",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
            ),
            ("crates/cw-core/Cargo.toml", &manifest("cw-core", "")),
            ("crates/cw-core/src/lib.rs", "pub fn core() {}\n"),
            (
                "crates/cw-api/Cargo.toml",
                &manifest(
                    "cw-api",
                    "cw-core = { path = \"../cw-core\", version = \"2.0.0\", registry = \"local\" }\n",
                ),
            ),
            ("crates/cw-api/src/lib.rs", "pub use cw_core::core;\n"),
            (
                ".cutwater/versions.toml",
                "[units]\ncw-api = \"2.0.0\"\ncw-core = \"2.0.0\"\n",
            ),
            (
                "crates/cw-core/CHANGELOG.md",
                "# cw-core\n\n## 2.0.0\n\n- Core.\n",
            ),
            (
                "crates/cw-api/CHANGELOG.md",
                "# cw-api\n\n## 2.0.0\n\n- Api.\n",
            ),
        ],
    );
    let lockfile = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .env("CARGO_HOME", registry.home())
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(lockfile.status.success(), "{lockfile:?}");
    repo.git(&["add", "Cargo.lock"]);
    repo.git(&["commit", "--quiet", "--amend", "--no-edit"]);
    let mut gitlab = GitLab::new();
    gitlab.no_tag("cw-core-v2.0.0");
    gitlab.no_tag("cw-api-v2.0.0");
    let (create, _) = gitlab.releases();
    let assert = publish(&repo, &gitlab, repo.path())
        .env("CARGO_HOME", registry.home())
        .env("CARGO_REGISTRIES_LOCAL_TOKEN", "registry-test-token")
        .assert()
        .success();
    create.expect(2).assert();
    assert_eq!(
        states(&assert.get_output().stdout),
        [pair("cw-core", "published"), pair("cw-api", "published")]
    );
    assert!(registry.lists("cw-core", "2.0.0"));
    assert!(registry.lists("cw-api", "2.0.0"));

    // A retry finds both published and only tags them.
    let mut gitlab = GitLab::new();
    gitlab.no_tag("cw-core-v2.0.0");
    gitlab.no_tag("cw-api-v2.0.0");
    let (create, _) = gitlab.releases();
    let assert = publish(&repo, &gitlab, repo.path())
        .env("CARGO_HOME", registry.home())
        .env("CARGO_REGISTRIES_LOCAL_TOKEN", "registry-test-token")
        .assert()
        .success();
    create.expect(2).assert();
    assert_eq!(
        states(&assert.get_output().stdout),
        [pair("cw-core", "present"), pair("cw-api", "present")]
    );
}
