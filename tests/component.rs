//! The shell steps of the CI/CD component, templates/release.yml, run with
//! `sh` and `set -e`, as GitLab's runners run a job's script, against local
//! servers. The tests marked ignored run cosign and shellcheck; CI's
//! `test-component` job runs them with the tools that
//! `.gitlab/scripts/install-tools.sh` pins, named by `CUTWATER_TEST_COSIGN`
//! and `CUTWATER_TEST_SHELLCHECK`.
//! They verify a bundle of Sigstore's conformance suite against the trusted
//! root it was signed under (tests/fixtures/sigstore), so they reach no
//! Sigstore service.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mockito::{Matcher, Server};
use serde_json::{Value, json};

const TEMPLATE: &str = include_str!("../templates/release.yml");
/// The release pipeline of cutwater, as GitLab.com's ID tokens name it in
/// the certificates Sigstore issues for them.
const IDENTITY: &str = "https://gitlab.com/sakiori-oss/cutwater//.gitlab-ci.yml@refs/heads/main";
const ISSUER: &str = "https://gitlab.com";
/// The signer of the conformance suite's bundles.
const FIXTURE_IDENTITY: &str = "https://github.com/sigstore-conformance/extremely-dangerous-public-oidc-beacon/.github/workflows/extremely-dangerous-oidc-beacon.yml@refs/heads/main";
const FIXTURE_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// The two items of `.cutwater-tool`'s `before_script` that the component
/// writes itself: the definitions of the steps and the installation.
fn steps() -> (String, String) {
    let jobs = jobs();
    let items = jobs[".cutwater-tool"]["before_script"]
        .as_array()
        .expect("`.cutwater-tool` has a before_script");
    let item = |index: usize| items[index].as_str().expect("a script line").to_string();
    (item(0), item(1))
}

/// The template's document of jobs.
fn jobs() -> Value {
    let documents: Vec<Value> = serde_saphyr::from_multiple(TEMPLATE).expect("the template parses");
    let [_, jobs] = documents.as_slice() else {
        panic!("the template has a spec and a document of jobs");
    };
    jobs.clone()
}

/// The `after_script` that revokes a crates.io token a canceled job left.
fn revocation_after_script() -> String {
    jobs()[".cutwater-crates-io-trusted-publishing"]["after_script"][0]
        .as_str()
        .expect("trusted publishing on crates.io has an after_script")
        .to_string()
}

/// A directory to run the steps in, with stand-ins for commands put first
/// on the `PATH`.
struct Job {
    dir: tempfile::TempDir,
}

impl Job {
    fn new() -> Job {
        let job = Job {
            dir: tempfile::tempdir().unwrap(),
        };
        for dir in ["project", "fakes", "tmp"] {
            std::fs::create_dir(job.path(dir)).unwrap();
        }
        std::fs::write(job.path("steps.sh"), steps().0).unwrap();
        job
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Puts an executable `name` running `script` first on the `PATH`.
    fn fake(&self, name: &str, script: &str) {
        let file = self.path("fakes").join(name);
        std::fs::write(&file, format!("#!/bin/sh\n{script}")).unwrap();
        executable(&file);
    }

    /// Runs `script` in the project directory after the steps' definitions.
    fn run(&self, script: &str, env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.path("fakes").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new("sh")
            .arg("-c")
            .arg(format!("set -e\n. \"$CUTWATER_TEST_STEPS\"\n{script}"))
            .current_dir(self.path("project"))
            .env_remove("CARGO_REGISTRY_TOKEN")
            .env_remove("CUTWATER_HOME")
            .env_remove("CRATES_IO_ID_TOKEN")
            .env("CUTWATER_TEST_STEPS", self.path("steps.sh"))
            .env("TMPDIR", self.path("tmp"))
            .env("PATH", path)
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }
}

fn executable(file: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[track_caller]
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

#[track_caller]
fn assert_failure(output: &Output, message: &str) {
    assert!(!output.status.success(), "stdout:\n{}", stdout(output));
    assert!(
        stderr(output).contains(message),
        "stderr:\n{}",
        stderr(output)
    );
}

/// A release archive whose `cutwater` reports `reported`.
fn archive(reported: &str) -> Vec<u8> {
    let script = format!("#!/bin/sh\necho '{reported}'\n");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "cutwater", script.as_bytes())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn sha256(content: &[u8]) -> String {
    cutwater::assets::sha256(content)
}

/// The release target of the machine the tests run on, as the component
/// names it.
fn host_target() -> String {
    let output = Job::new().run("cutwater_target", &[]);
    assert_success(&output);
    stdout(&output).trim().to_string()
}

/// The installation line of the template with the component's context and
/// inputs as GitLab interpolates them.
fn installation(inputs: &[(&str, &str)]) -> String {
    let mut line = steps().1;
    for (name, value) in inputs {
        let placeholder = format!("$[[ {name} ]]");
        assert!(line.contains(&placeholder), "{line} lacks {placeholder}");
        line = line.replace(&placeholder, value);
    }
    assert!(!line.contains("$[["), "{line} has an input left");
    line
}

/// A download location holding release 1.4.0 for the host, whose archive
/// `SHA256SUMS` lists with `listed`, or with its own checksum.
fn release(server: &mut Server, listed: Option<&str>) -> Vec<mockito::Mock> {
    let name = format!("cutwater-1.4.0-{}.tar.gz", host_target());
    let content = archive("cutwater 1.4.0");
    let sums = format!(
        "{}  cutwater-1.4.0-x86_64-apple-darwin.tar.gz\n{}  {name}\n",
        sha256(b"another"),
        listed.map_or_else(|| sha256(&content), str::to_string)
    );
    vec![
        server
            .mock("GET", format!("/cutwater/1.4.0/{name}").as_str())
            .with_body(content)
            .create(),
        server
            .mock("GET", "/cutwater/1.4.0/SHA256SUMS")
            .with_body(sums)
            .create(),
        server
            .mock("GET", "/cutwater/1.4.0/SHA256SUMS.sigstore.json")
            .with_body("{}")
            .create(),
    ]
}

/// cosign and its verification as stand-ins that record their arguments.
const STAND_INS: &str = r#"
cutwater_cosign() { echo "cosign $*" >> "$CUTWATER_TEST_LOG"; : > "$3/cosign"; }
cutwater_verify() { echo "verify $4 $5 [$6]" >> "$CUTWATER_TEST_LOG"; return "${CUTWATER_TEST_VERIFY:-0}"; }
"#;

fn install(server: &Server, reference: &str, verified: bool) -> (Job, Output) {
    let job = Job::new();
    let line = installation(&[
        ("inputs.download-url", &format!("{}/cutwater", server.url())),
        ("component.version", ""),
        ("component.reference", reference),
        ("inputs.cosign-url", &format!("{}/cosign", server.url())),
        ("inputs.trusted-root", ""),
        ("inputs.binary", ""),
    ]);
    let log = job.path("log");
    let output = job.run(
        &format!("{STAND_INS}{line}\ncutwater --version\n"),
        &[
            ("CUTWATER_TEST_LOG", log.to_str().unwrap()),
            ("CUTWATER_TEST_VERIFY", if verified { "0" } else { "1" }),
        ],
    );
    (job, output)
}

#[test]
fn target_of_each_runner() {
    let job = Job::new();
    job.fake(
        "uname",
        "case $1 in -s) echo \"$FAKE_SYSTEM\" ;; -m) echo \"$FAKE_MACHINE\" ;; esac\n",
    );
    for (system, machine, target) in [
        ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
        ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
        ("Darwin", "arm64", "aarch64-apple-darwin"),
    ] {
        let output = job.run(
            "cutwater_target",
            &[("FAKE_SYSTEM", system), ("FAKE_MACHINE", machine)],
        );
        assert_success(&output);
        assert_eq!(stdout(&output).trim(), target);
    }
    let output = job.run(
        "cutwater_target",
        &[("FAKE_SYSTEM", "Linux"), ("FAKE_MACHINE", "riscv64")],
    );
    assert_failure(&output, "no release for Linux on riscv64");
}

#[test]
fn version_from_the_component_context() {
    let job = Job::new();
    for (version, reference, expected) in [
        ("1.4.0", "~latest", "1.4.0"),
        ("1.4.0", "1.4", "1.4.0"),
        ("", "1.4.0", "1.4.0"),
        ("null", "2.0.0-rc.1", "2.0.0-rc.1"),
    ] {
        let output = job.run(&format!("cutwater_version '{version}' '{reference}'"), &[]);
        assert_success(&output);
        assert_eq!(stdout(&output).trim(), expected, "{version} @{reference}");
    }
    // cutwater's tags carry no `v`, so `v1.4.0` names no release.
    for reference in [
        "main",
        "0d1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b",
        "1.4",
        "v1.4.0",
    ] {
        let output = job.run(&format!("cutwater_version '' '{reference}'"), &[]);
        assert_failure(
            &output,
            &format!("included as @{reference}, which names no release"),
        );
    }
}

#[test]
fn installation_verifies_the_signature_then_the_archive() {
    let mut server = Server::new();
    let mocks = release(&mut server, None);
    let (job, output) = install(&server, "1.4.0", true);
    assert_success(&output);
    for mock in mocks {
        mock.assert();
    }
    assert!(stdout(&output).contains("installed cutwater 1.4.0"));
    assert!(stdout(&output).ends_with("cutwater 1.4.0\n"));
    let log = std::fs::read_to_string(job.path("log")).unwrap();
    let cosign = format!("{}/cosign", server.url());
    assert!(
        log.starts_with(&format!("cosign {cosign} {} ", host_target())),
        "{log}"
    );
    assert!(
        log.ends_with(&format!("verify {IDENTITY} {ISSUER} []\n")),
        "{log}"
    );
    // Nothing of the installation lies in the project directory.
    assert_eq!(job.path("project").read_dir().unwrap().count(), 0);
}

#[test]
fn failed_verification_installs_nothing() {
    let mut server = Server::new();
    let _mocks = release(&mut server, None);
    let (_job, output) = install(&server, "1.4.0", false);
    assert!(!output.status.success());
    assert!(
        !stdout(&output).contains("installed"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn archive_with_another_checksum_is_refused() {
    let mut server = Server::new();
    let _mocks = release(&mut server, Some(&sha256(b"what was signed")));
    let (_job, output) = install(&server, "1.4.0", true);
    assert_failure(
        &output,
        "does not have the checksum SHA256SUMS lists for it",
    );
}

#[test]
fn archive_that_sha256sums_does_not_list_is_refused() {
    let job = Job::new();
    std::fs::write(
        job.path("tmp/SHA256SUMS"),
        format!("{}  other.tar.gz\n", sha256(b"x")),
    )
    .unwrap();
    std::fs::write(
        job.path("tmp/cutwater-1.4.0-x.tar.gz"),
        archive("cutwater 1.4.0"),
    )
    .unwrap();
    let output = job.run(
        "cutwater_unpack ../tmp/SHA256SUMS ../tmp/cutwater-1.4.0-x.tar.gz ../tmp",
        &[],
    );
    assert_failure(&output, "SHA256SUMS does not list cutwater-1.4.0-x.tar.gz");
}

#[test]
fn missing_release_is_named() {
    let server = Server::new();
    let (_job, output) = install(&server, "1.4.0", true);
    assert_failure(
        &output,
        &format!("cannot download {}/cutwater/1.4.0/", server.url()),
    );
}

#[test]
fn reference_without_a_release_installs_nothing() {
    let server = Server::new();
    let (_job, output) = install(&server, "main", true);
    assert_failure(&output, "included as @main, which names no release");
}

#[test]
fn cosign_with_another_checksum_is_refused() {
    let mut server = Server::new();
    let target = host_target();
    let file = match target.as_str() {
        "x86_64-unknown-linux-musl" => "cosign-linux-amd64",
        "aarch64-unknown-linux-musl" => "cosign-linux-arm64",
        "x86_64-apple-darwin" => "cosign-darwin-amd64",
        _ => "cosign-darwin-arm64",
    };
    let mock = server
        .mock("GET", format!("/v3.1.4/{file}").as_str())
        .with_body("not cosign")
        .create();
    let job = Job::new();
    let output = job.run(
        &format!("cutwater_cosign '{}' {target} ../tmp", server.url()),
        &[],
    );
    mock.assert();
    assert_failure(&output, "is not cosign 3.1.4");
    assert!(!job.path("tmp/cosign").exists());
}

#[test]
fn binary_input_runs_the_pipeline_s_executable() {
    let server = Server::new();
    let job = Job::new();
    std::fs::create_dir_all(job.path("project/target")).unwrap();
    std::fs::write(
        job.path("project/target/cutwater"),
        "#!/bin/sh\necho 'cutwater 0.1.0'\n",
    )
    .unwrap();
    let line = installation(&[
        ("inputs.download-url", &format!("{}/cutwater", server.url())),
        ("component.version", ""),
        (
            "component.reference",
            "0d1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b",
        ),
        ("inputs.cosign-url", &format!("{}/cosign", server.url())),
        ("inputs.trusted-root", ""),
        ("inputs.binary", "target/cutwater"),
    ]);
    let output = job.run(&format!("{line}\ncutwater --version\n"), &[]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "running target/cutwater: cutwater 0.1.0\ncutwater 0.1.0\n"
    );
}

/// crates.io's endpoint of trusted publishing tokens, which exchanges
/// `ID_TOKEN` for `TOKEN` and revokes `TOKEN`.
const ID_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJjcmF0ZXMuaW8ifQ.c2lnbmF0dXJl";
const TOKEN: &str = "cioAbCdEf0123456789";

fn exchange(server: &mut Server, status: usize, body: &str) -> mockito::Mock {
    server
        .mock("POST", "/api/v1/trusted_publishing/tokens")
        .match_header("content-type", "application/json")
        .match_header(
            "user-agent",
            Matcher::Regex("^cutwater-ci-component ".to_string()),
        )
        .match_body(Matcher::Json(json!({ "jwt": ID_TOKEN })))
        .with_status(status)
        .with_body(body)
        .create()
}

fn revocation(server: &mut Server, status: usize) -> mockito::Mock {
    server
        .mock("DELETE", "/api/v1/trusted_publishing/tokens")
        .match_header("authorization", format!("Bearer {TOKEN}").as_str())
        .match_header(
            "user-agent",
            Matcher::Regex("^cutwater-ci-component ".to_string()),
        )
        .with_status(status)
        .create()
}

/// Runs the exchange around a command that records the token it was given
/// and exits with `exit`.
fn with_token(server: &Server, exit: u8) -> (Job, Output) {
    let job = Job::new();
    job.fake(
        "publish-crates",
        &format!("printf %s \"$CARGO_REGISTRY_TOKEN\" > ../given\nexit {exit}\n"),
    );
    let output = job.run(
        &format!(
            "cutwater_with_crates_io_token '{}/api/v1/trusted_publishing/tokens' publish-crates",
            server.url()
        ),
        &[("CRATES_IO_ID_TOKEN", ID_TOKEN)],
    );
    for text in [stdout(&output), stderr(&output)] {
        assert!(!text.contains(TOKEN) && !text.contains(ID_TOKEN), "{text}");
    }
    (job, output)
}

#[test]
fn crates_io_token_is_exchanged_for_the_command_and_revoked() {
    let mut server = Server::new();
    let exchanged = exchange(&mut server, 200, &format!(r#"{{"token":"{TOKEN}"}}"#));
    let revoked = revocation(&mut server, 204);
    let (job, output) = with_token(&server, 0);
    assert_success(&output);
    exchanged.assert();
    revoked.assert();
    assert_eq!(std::fs::read_to_string(job.path("given")).unwrap(), TOKEN);
    assert!(stdout(&output).contains("revoked the crates.io token"));
    // The secrets' directory is gone.
    assert_eq!(job.path("tmp").read_dir().unwrap().count(), 0);
}

#[test]
fn crates_io_token_is_revoked_after_a_failed_command() {
    let mut server = Server::new();
    let _exchanged = exchange(&mut server, 200, &format!(r#"{{"token":"{TOKEN}"}}"#));
    let revoked = revocation(&mut server, 204);
    let (_job, output) = with_token(&server, 3);
    assert_eq!(output.status.code(), Some(3));
    revoked.assert();
}

#[test]
fn failed_revocation_only_warns() {
    let mut server = Server::new();
    let _exchanged = exchange(&mut server, 200, &format!(r#"{{"token":"{TOKEN}"}}"#));
    let revoked = revocation(&mut server, 500);
    let (_job, output) = with_token(&server, 0);
    assert_success(&output);
    revoked.assert();
    assert!(stderr(&output).contains("warning: crates.io did not revoke the token (HTTP 500)"));
}

#[test]
fn refused_exchange_runs_nothing() {
    let mut server = Server::new();
    let exchanged = exchange(
        &mut server,
        400,
        r#"{"errors":[{"detail":"No matching Trusted Publishing config found"}]}"#,
    );
    let revoked = revocation(&mut server, 204).expect(0);
    let (job, output) = with_token(&server, 0);
    assert_failure(
        &output,
        "crates.io did not exchange the job's ID token (HTTP 400): {\"errors\":[{\"detail\":\"No matching",
    );
    exchanged.assert();
    revoked.assert();
    assert!(!job.path("given").exists());
}

#[test]
fn exchange_without_an_id_token_runs_nothing() {
    let job = Job::new();
    let output = job.run("cutwater_with_crates_io_token http://127.0.0.1:9 true", &[]);
    assert_failure(&output, "CRATES_IO_ID_TOKEN is not set");
}

#[test]
fn publish_record_is_kept_when_publish_fails() {
    let job = Job::new();
    job.fake(
        "cutwater",
        "test \"$*\" = 'publish --json' || exit 9\necho '{\"units\":[]}'\nexit 1\n",
    );
    let home = job.path("tmp");
    let output = job.run(
        "cutwater_publish token http://127.0.0.1:9",
        &[("CUTWATER_HOME", home.to_str().unwrap())],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(job.path("project/cutwater-publish.json")).unwrap(),
        "{\"units\":[]}\n"
    );
}

#[test]
fn trusted_publishing_publishes_with_trusted_publishing() {
    let mut server = Server::new();
    let exchanged = exchange(&mut server, 200, &format!(r#"{{"token":"{TOKEN}"}}"#));
    let revoked = revocation(&mut server, 204);
    let job = publishing_job("tool 0.2.0 cargo\napp 0.2.0 npm\n");
    let output = publish_trusted(&job, &server);
    assert_success(&output);
    exchanged.assert();
    revoked.assert();
    assert_eq!(std::fs::read_to_string(job.path("given")).unwrap(), TOKEN);
}

/// A job with a stand-in for cutwater: `released` names `released`, and
/// `publish` records the Cargo token it was given.
fn publishing_job(released: &str) -> Job {
    let job = Job::new();
    job.fake(
        "cutwater",
        &format!(
            "if [ \"$1\" = released ]; then printf '{released}'; exit 0; fi\n\
             printf %s \"$CARGO_REGISTRY_TOKEN\" > ../given\necho '{{}}'\n"
        ),
    );
    job
}

fn publish_trusted(job: &Job, server: &Server) -> Output {
    let home = job.path("tmp");
    job.run(
        &format!(
            "cutwater_publish trusted-publishing '{}/api/v1/trusted_publishing/tokens'",
            server.url()
        ),
        &[
            ("CUTWATER_HOME", home.to_str().unwrap()),
            ("CRATES_IO_ID_TOKEN", ID_TOKEN),
        ],
    )
}

/// A crates.io outage cannot fail a pipeline that publishes no crate.
#[test]
fn no_exchange_without_a_cargo_release() {
    let mut server = Server::new();
    let exchanged = exchange(&mut server, 200, "{}").expect(0);
    for released in ["", "app 0.2.0 npm\n"] {
        let job = publishing_job(released);
        let output = publish_trusted(&job, &server);
        assert_success(&output);
        assert!(stdout(&output).contains("exchanges no crates.io token"));
        assert_eq!(std::fs::read_to_string(job.path("given")).unwrap(), "");
    }
    exchanged.assert();
}

/// A job canceled while it held the token leaves its authorization, which
/// the after_script revokes; without one it does nothing.
#[test]
fn after_script_revokes_a_token_the_job_left() {
    let mut server = Server::new();
    let revoked = revocation(&mut server, 204);
    let job = Job::new();
    let secrets = job.path("tmp/cutwater-crates-io-41");
    std::fs::create_dir(&secrets).unwrap();
    std::fs::write(
        secrets.join("authorization"),
        format!("Authorization: Bearer {TOKEN}\n"),
    )
    .unwrap();
    let tokens = format!("{}/api/v1/trusted_publishing/tokens", server.url());
    let env = [
        ("CI_JOB_ID", "41"),
        ("CUTWATER_CRATES_IO_TOKENS", tokens.as_str()),
    ];
    let output = job.run(&revocation_after_script(), &env);
    assert_success(&output);
    revoked.assert();
    assert!(stdout(&output).contains("revoked the crates.io token that the job left"));
    assert!(!secrets.exists());

    let revoked = revocation(&mut server, 204).expect(0);
    let output = job.run(&revocation_after_script(), &env);
    assert_success(&output);
    revoked.assert();
}

#[test]
fn registry_token_takes_the_place_of_trusted_publishing() {
    let mut server = Server::new();
    let exchanged = exchange(&mut server, 200, "{}").expect(0);
    let job = Job::new();
    job.fake(
        "cutwater",
        "printf %s \"$CARGO_REGISTRY_TOKEN\" > ../given\necho '{}'\n",
    );
    let home = job.path("tmp");
    let output = job.run(
        &format!(
            "cutwater_publish trusted-publishing '{}/api/v1/trusted_publishing/tokens'",
            server.url()
        ),
        &[
            ("CUTWATER_HOME", home.to_str().unwrap()),
            ("CRATES_IO_ID_TOKEN", ID_TOKEN),
            ("CARGO_REGISTRY_TOKEN", "cio-first-publication"),
        ],
    );
    assert_success(&output);
    exchanged.assert();
    assert_eq!(
        std::fs::read_to_string(job.path("given")).unwrap(),
        "cio-first-publication"
    );
    assert!(stderr(&output).contains("warning: CARGO_REGISTRY_TOKEN is set"));
}

fn cosign() -> PathBuf {
    PathBuf::from(std::env::var_os("CUTWATER_TEST_COSIGN").unwrap_or_else(|| {
        panic!("CUTWATER_TEST_COSIGN names no cosign; see the module's comment")
    }))
}

#[test]
#[ignore = "needs cosign, as CI's test-component job provides"]
fn pinned_cosign_is_the_one_install_tools_pins() {
    let mut server = Server::new();
    let target = host_target();
    let file = match target.as_str() {
        "x86_64-unknown-linux-musl" => "cosign-linux-amd64",
        "aarch64-unknown-linux-musl" => "cosign-linux-arm64",
        "x86_64-apple-darwin" => "cosign-darwin-amd64",
        _ => "cosign-darwin-arm64",
    };
    let mock = server
        .mock("GET", format!("/v3.1.4/{file}").as_str())
        .with_body_from_file(cosign())
        .create();
    let job = Job::new();
    let output = job.run(
        &format!("cutwater_cosign '{}' {target} ../tmp", server.url()),
        &[],
    );
    mock.assert();
    assert_success(&output);
    assert!(job.path("tmp/cosign").exists());
}

#[test]
#[ignore = "needs cosign, as CI's test-component job provides"]
fn signature_is_verified_against_the_signer_and_the_trusted_root() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sigstore");
    let job = Job::new();
    std::fs::copy(fixtures.join("a.txt"), job.path("tmp/signed")).unwrap();
    std::fs::write(job.path("tmp/tampered"), "DO NOT MODIFY ME!\n").unwrap();
    let verify = |file: &str, identity: &str, issuer: &str| {
        job.run(
            &format!(
                "cutwater_verify '{}' ../tmp/{file} '{}' '{identity}' '{issuer}' '{}'",
                cosign().display(),
                fixtures.join("a.txt.sigstore.json").display(),
                fixtures.join("trusted_root.json").display()
            ),
            &[],
        )
    };
    assert_success(&verify("signed", FIXTURE_IDENTITY, FIXTURE_ISSUER));
    assert_failure(
        &verify("tampered", FIXTURE_IDENTITY, FIXTURE_ISSUER),
        "is not that of cutwater's release pipeline",
    );
    assert_failure(
        &verify("signed", IDENTITY, ISSUER),
        &format!("is not that of cutwater's release pipeline, {IDENTITY}"),
    );
    let missing = job.run(
        "cutwater_verify cosign ../tmp/signed bundle identity issuer missing-root.json",
        &[],
    );
    assert_failure(&missing, "the trusted root missing-root.json is not a file");
}

#[test]
#[ignore = "needs shellcheck, as CI's test-component job provides"]
fn steps_pass_shellcheck() {
    let shellcheck = std::env::var_os("CUTWATER_TEST_SHELLCHECK").unwrap_or_else(|| {
        panic!("CUTWATER_TEST_SHELLCHECK names no shellcheck; see the module's comment")
    });
    let job = Job::new();
    std::fs::write(job.path("after_script.sh"), revocation_after_script()).unwrap();
    let output = Command::new(shellcheck)
        .arg("--shell=sh")
        .arg(job.path("steps.sh"))
        .arg(job.path("after_script.sh"))
        .output()
        .unwrap();
    assert_success(&output);
}
