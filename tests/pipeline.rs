//! The scripts of cutwater's own pipeline that decide what a pipeline
//! stages for publication: .gitlab/scripts/stage-npm.sh, run with `sh` in a
//! copy of the repository's layout, with a stand-in for `cutwater released`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TARGETS: [(&str, &str); 4] = [
    ("x86_64-unknown-linux-musl", "linux-x64"),
    ("aarch64-unknown-linux-musl", "linux-arm64"),
    ("x86_64-apple-darwin", "darwin-x64"),
    ("aarch64-apple-darwin", "darwin-arm64"),
];

/// The repository's layout for stage-npm.sh at version 0.1.0, with a
/// `cutwater` whose `released` prints `released`, or fails when it is
/// `None`.
struct Layout {
    dir: tempfile::TempDir,
}

impl Layout {
    fn new(released: Option<&str>) -> Layout {
        let layout = Layout {
            dir: tempfile::tempdir().unwrap(),
        };
        let root = layout.root();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"cutwater\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(root.join("LICENSE"), "MIT\n").unwrap();
        std::fs::create_dir_all(root.join("npm/cutwater")).unwrap();
        for (_, platform) in TARGETS {
            std::fs::create_dir_all(root.join(format!("npm/cutwater-bin-{platform}"))).unwrap();
        }
        let script = match released {
            Some(released) => {
                format!("#!/bin/sh\n[ \"$1\" = released ] || exit 9\nprintf '{released}'\n")
            }
            None => "#!/bin/sh\necho 'cutwater ran' >&2\nexit 9\n".to_string(),
        };
        let fake = root.join("bin/cutwater");
        std::fs::write(&fake, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        layout
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// The release archives of every target in dist/.
    fn archives(&self) {
        std::fs::create_dir_all(self.root().join("dist")).unwrap();
        for (target, _) in TARGETS {
            let file = std::fs::File::create(
                self.root()
                    .join(format!("dist/cutwater-0.1.0-{target}.tar.gz")),
            )
            .unwrap();
            let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::default(),
            ));
            for name in [
                "cutwater",
                "THIRD-PARTY-NOTICES.txt",
                "RUST-LIBRARY-COPYRIGHT.html",
            ] {
                let content = format!("{name} of {target}\n");
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                archive
                    .append_data(&mut header, name, content.as_bytes())
                    .unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
    }

    fn stage(&self, env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join(".gitlab/scripts/stage-npm.sh");
        let path = format!(
            "{}:{}",
            self.root().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new("sh")
            .arg(script)
            .current_dir(self.root())
            .env_remove("CI_MERGE_REQUEST_IID")
            .env_remove("CI_PIPELINE_SOURCE")
            .env("PATH", path)
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A later commit's default branch pipeline has no archives and publishes
/// nothing: it stages nothing and passes, so that its cutwater-publish runs
/// its checks.
#[test]
fn nothing_to_publish_stages_nothing() {
    let layout = Layout::new(Some(""));
    let output = layout.stage(&[("CI_PIPELINE_SOURCE", "push")]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("this commit releases nothing"));
    assert!(
        !layout
            .root()
            .join("npm/cutwater-bin-linux-x64/cutwater")
            .exists()
    );
}

/// The release commit's pipeline fails without its archives.
#[test]
fn release_without_its_archives_fails() {
    let layout = Layout::new(Some("cutwater 0.1.0 cargo\\ncutwater-npm 0.1.0 npm\\n"));
    let output = layout.stage(&[("CI_PIPELINE_SOURCE", "push")]);
    assert!(!output.status.success());
    assert!(
        text(&output.stderr)
            .contains("dist/ lacks cutwater-0.1.0-x86_64-unknown-linux-musl.tar.gz"),
        "{}",
        text(&output.stderr)
    );
}

/// A scheduled pipeline, or one started by hand or by the API, builds no
/// release files even on the release commit, so it stages nothing and asks
/// nothing; its cutwater-publish passes where the release is tagged and
/// fails where it is not, as its assets are missing.
#[test]
fn pipeline_other_than_a_push_stages_nothing() {
    let layout = Layout::new(None);
    for source in ["schedule", "web", "api"] {
        let output = layout.stage(&[("CI_PIPELINE_SOURCE", source)]);
        assert!(
            output.status.success(),
            "{source}: {}",
            text(&output.stderr)
        );
        assert!(
            text(&output.stdout).contains(&format!("a {source} pipeline builds no release files")),
            "{}",
            text(&output.stdout)
        );
        assert!(
            !layout
                .root()
                .join("npm/cutwater-bin-linux-x64/cutwater")
                .exists()
        );
    }
}

/// The release merge request's pipeline always stages, without asking
/// which units the commit releases.
#[test]
fn release_merge_request_stages_every_platform_package() {
    let layout = Layout::new(None);
    layout.archives();
    let output = layout.stage(&[("CI_MERGE_REQUEST_IID", "5")]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    for (target, platform) in TARGETS {
        let package = layout.root().join(format!("npm/cutwater-bin-{platform}"));
        for name in [
            "cutwater",
            "THIRD-PARTY-NOTICES.txt",
            "RUST-LIBRARY-COPYRIGHT.html",
        ] {
            assert_eq!(
                std::fs::read_to_string(package.join(name)).unwrap(),
                format!("{name} of {target}\n")
            );
        }
        assert_eq!(
            std::fs::read_to_string(package.join("LICENSE")).unwrap(),
            "MIT\n"
        );
    }
    assert!(layout.root().join("npm/cutwater/LICENSE").exists());
}
