//! Maven units for the tests: `billing`, an aggregator with the modules
//! `billing-api` and `billing-service`; `ledger`, a single POM; and
//! `orders`, which depends on `billing-api` and `ledger` through the version
//! properties `billing.version` and `ledger.version`. Every POM uses the
//! flatten plugin, as the Maven adapter requires, and the `pom` packaging,
//! so that a build compiles nothing.

use super::Repo;

/// The plugins the Maven tests run, by their coordinates.
pub const PLUGINS: &str = include_str!("../fixtures/maven-plugins.txt");

/// The version of the plugin `group:artifact` in [`PLUGINS`].
pub fn plugin_version(plugin: &str) -> &'static str {
    PLUGINS
        .lines()
        .find_map(|line| line.strip_prefix(plugin)?.strip_prefix(':'))
        .unwrap_or_else(|| panic!("tests/fixtures/maven-plugins.txt lists no {plugin}"))
}

/// The flatten plugin, configured to resolve `revision` and to interpolate
/// dependencies and dependency management, as the README describes it.
pub fn flatten(mode: Option<&str>) -> String {
    let version = plugin_version("org.codehaus.mojo:flatten-maven-plugin");
    let mode = mode
        .map(|mode| format!("\n          <flattenMode>{mode}</flattenMode>"))
        .unwrap_or_default();
    format!(
        r#"  <build>
    <plugins>
      <plugin>
        <groupId>org.codehaus.mojo</groupId>
        <artifactId>flatten-maven-plugin</artifactId>
        <version>{version}</version>
        <configuration>
          <updatePomFile>true</updatePomFile>{mode}
          <pomElements>
            <dependencies>interpolate</dependencies>
            <dependencyManagement>interpolate</dependencyManagement>
          </pomElements>
        </configuration>
        <executions>
          <execution>
            <id>flatten</id>
            <phase>process-resources</phase>
            <goals><goal>flatten</goal></goals>
          </execution>
          <execution>
            <id>flatten-clean</id>
            <phase>clean</phase>
            <goals><goal>clean</goal></goals>
          </execution>
        </executions>
      </plugin>
    </plugins>
  </build>
"#
    )
}

/// The Maven units of a repository, with what can differ between tests.
pub struct Units {
    pub billing: &'static str,
    pub ledger: &'static str,
    pub orders: &'static str,
    /// The values of `orders`' properties `billing.version` and
    /// `ledger.version`.
    pub billing_property: &'static str,
    pub ledger_property: &'static str,
    /// The URL of the deploy repository.
    pub deploy: String,
    /// The flatten plugin's mode in `orders`, `None` for its default.
    pub orders_flatten: Option<&'static str>,
    /// The packaging of `billing-api`: `pom`, or `jar` for a module whose
    /// main artifact is a file of its own.
    pub billing_api_packaging: &'static str,
    /// The detached signature that `billing-api`'s build attaches to its
    /// JAR, as a signing plugin would, when given.
    pub billing_api_signature: Option<&'static str>,
    /// Whether `billing`'s build sets `project.build.outputTimestamp`, which
    /// makes its archives the same in every build.
    pub reproducible: bool,
}

impl Default for Units {
    fn default() -> Units {
        Units {
            billing: "1.3.1-SNAPSHOT",
            ledger: "2.1.1-SNAPSHOT",
            orders: "0.4.1-SNAPSHOT",
            billing_property: "1.3.1-SNAPSHOT",
            ledger_property: "2.1.1-SNAPSHOT",
            deploy: "file:///nonexistent/deploy".to_string(),
            orders_flatten: None,
            billing_api_packaging: "pom",
            billing_api_signature: None,
            reproducible: false,
        }
    }
}

fn project(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n{body}</project>\n"
    )
}

impl Units {
    fn distribution(&self) -> String {
        format!(
            "  <distributionManagement>\n    <repository>\n      <id>deploy</id>\n      <url>{}</url>\n    </repository>\n  </distributionManagement>\n",
            self.deploy
        )
    }

    /// The repository `deploy` for dependencies, so that builds resolve
    /// released units from it.
    fn repositories(&self) -> String {
        format!(
            "  <repositories>\n    <repository>\n      <id>deploy</id>\n      <url>{}</url>\n    </repository>\n  </repositories>\n",
            self.deploy
        )
    }

    pub fn billing_pom(&self) -> String {
        let timestamp = if self.reproducible {
            "    <project.build.outputTimestamp>2026-01-01T00:00:00Z</project.build.outputTimestamp>\n"
        } else {
            ""
        };
        project(&format!(
            "  <groupId>com.acme</groupId>\n  <artifactId>billing</artifactId>\n  <version>${{revision}}</version>\n  <packaging>pom</packaging>\n  <properties>\n    <revision>{}</revision>\n{timestamp}  </properties>\n  <modules>\n    <module>billing-api</module>\n    <module>billing-service</module>\n  </modules>\n{}{}",
            self.billing,
            self.distribution(),
            flatten(None)
        ))
    }

    pub fn billing_module(&self, artifact: &str) -> String {
        let packaging = if artifact == "billing-api" {
            self.billing_api_packaging
        } else {
            "pom"
        };
        let signature = if artifact == "billing-api" && self.billing_api_signature.is_some() {
            format!(
                "  <build>\n    <plugins>\n      <plugin>\n        <groupId>org.codehaus.mojo</groupId>\n        <artifactId>build-helper-maven-plugin</artifactId>\n        <version>{}</version>\n        <executions>\n          <execution>\n            <id>sign</id>\n            <phase>package</phase>\n            <goals><goal>attach-artifact</goal></goals>\n            <configuration>\n              <artifacts>\n                <artifact><file>signature.asc</file><type>jar.asc</type></artifact>\n              </artifacts>\n            </configuration>\n          </execution>\n        </executions>\n      </plugin>\n    </plugins>\n  </build>\n",
                plugin_version("org.codehaus.mojo:build-helper-maven-plugin")
            )
        } else {
            String::new()
        };
        project(&format!(
            "  <parent>\n    <groupId>com.acme</groupId>\n    <artifactId>billing</artifactId>\n    <version>${{revision}}</version>\n  </parent>\n  <artifactId>{artifact}</artifactId>\n  <packaging>{packaging}</packaging>\n{signature}"
        ))
    }

    pub fn ledger_pom(&self) -> String {
        project(&format!(
            "  <groupId>com.acme</groupId>\n  <artifactId>ledger</artifactId>\n  <version>${{revision}}</version>\n  <packaging>pom</packaging>\n  <properties>\n    <revision>{}</revision>\n  </properties>\n{}{}",
            self.ledger,
            self.distribution(),
            flatten(None)
        ))
    }

    pub fn orders_pom(&self) -> String {
        project(&format!(
            "  <groupId>com.acme</groupId>\n  <artifactId>orders</artifactId>\n  <version>${{revision}}</version>\n  <packaging>pom</packaging>\n  <properties>\n    <revision>{}</revision>\n    <billing.version>{}</billing.version>\n    <ledger.version>{}</ledger.version>\n  </properties>\n  <dependencies>\n    <dependency>\n      <groupId>com.acme</groupId>\n      <artifactId>billing-api</artifactId>\n      <version>${{billing.version}}</version>\n      <type>pom</type>\n    </dependency>\n    <dependency>\n      <groupId>com.acme</groupId>\n      <artifactId>ledger</artifactId>\n      <version>${{ledger.version}}</version>\n      <type>pom</type>\n    </dependency>\n  </dependencies>\n{}{}{}",
            self.orders,
            self.billing_property,
            self.ledger_property,
            self.repositories(),
            self.distribution(),
            flatten(self.orders_flatten)
        ))
    }

    /// Writes the units' POMs into `repo`.
    pub fn write(&self, repo: &Repo) {
        repo.write("billing/pom.xml", &self.billing_pom());
        repo.write(
            "billing/billing-api/pom.xml",
            &self.billing_module("billing-api"),
        );
        if let Some(signature) = self.billing_api_signature {
            repo.write("billing/billing-api/signature.asc", signature);
        }
        repo.write(
            "billing/billing-service/pom.xml",
            &self.billing_module("billing-service"),
        );
        repo.write("ledger/pom.xml", &self.ledger_pom());
        repo.write("orders/pom.xml", &self.orders_pom());
    }
}

/// Writes `text` to the deployed file `path` with its SHA-1, as a
/// deployment of another build would leave it.
pub fn redeploy(path: &std::path::Path, text: &str) {
    use sha1::{Digest, Sha1};
    std::fs::write(path, text).unwrap();
    let sha1: String = Sha1::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut checksum = path.as_os_str().to_owned();
    checksum.push(".sha1");
    std::fs::write(checksum, sha1).unwrap();
}

/// A Maven repository over HTTP on the loopback interface, kept in a
/// temporary directory, that refuses to replace a file it holds, as release
/// repositories do; it takes `maven-metadata.xml` and its checksums again,
/// as they do. Every upload is logged. Stopped when dropped.
pub struct HttpRepository {
    dir: tempfile::TempDir,
    address: std::net::SocketAddr,
    state: std::sync::Arc<std::sync::Mutex<HttpState>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct HttpState {
    uploads: Vec<String>,
    downloads: Vec<String>,
    /// Paths whose upload fails, by suffix, with the status and reason.
    refused: Vec<(String, u16, String)>,
}

impl HttpRepository {
    pub fn start() -> HttpRepository {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state = std::sync::Arc::new(std::sync::Mutex::new(HttpState::default()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (root, shared, stopped) = (dir.path().to_path_buf(), state.clone(), stop.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (root, shared) = (root.clone(), shared.clone());
                std::thread::spawn(move || serve(stream, &root, &shared));
            }
        });
        HttpRepository {
            dir,
            address,
            state,
            stop,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/repository", self.address)
    }

    /// The directory that holds the repository's files.
    pub fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// The paths uploaded so far, in order.
    pub fn uploads(&self) -> Vec<String> {
        self.state.lock().unwrap().uploads.clone()
    }

    /// The paths downloaded so far, in order.
    pub fn downloads(&self) -> Vec<String> {
        self.state.lock().unwrap().downloads.clone()
    }

    /// Makes every upload to a path ending in `suffix` fail with `status`
    /// and `reason`.
    pub fn refuse(&self, suffix: &str, status: u16, reason: &str) {
        self.state
            .lock()
            .unwrap()
            .refused
            .push((suffix.to_string(), status, reason.to_string()));
    }
}

impl Drop for HttpRepository {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.address);
    }
}

/// Answers the requests of one connection, as HTTP/1.1 with keep-alive.
fn serve(stream: std::net::TcpStream, root: &std::path::Path, state: &std::sync::Mutex<HttpState>) {
    use std::io::{BufRead, Read, Write};
    let mut writer = stream.try_clone().unwrap();
    let mut reader = std::io::BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
            return;
        };
        let (method, target) = (method.to_string(), target.to_string());
        let mut length = 0usize;
        let mut chunked = false;
        let mut expect = false;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let Some((name, value)) = header.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
                "expect" => expect = value.eq_ignore_ascii_case("100-continue"),
                _ => {}
            }
        }
        let path = target.split('?').next().unwrap_or_default();
        let relative = path
            .strip_prefix("/repository/")
            .unwrap_or_default()
            .to_string();
        let file = root.join(&relative);
        let respond = |writer: &mut std::net::TcpStream, status: u16, reason: &str, body: &[u8]| {
            let head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = writer.write_all(head.as_bytes());
            let _ = writer.write_all(body);
        };
        match method.as_str() {
            "GET" | "HEAD" => match std::fs::read(&file).ok().filter(|_| file.is_file()) {
                Some(content) => {
                    if method == "GET" {
                        state.lock().unwrap().downloads.push(relative.clone());
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        content.len()
                    );
                    let _ = writer.write_all(head.as_bytes());
                    if method == "GET" {
                        let _ = writer.write_all(&content);
                    }
                }
                None => respond(&mut writer, 404, "Not Found", b""),
            },
            "PUT" => {
                if expect {
                    let _ = writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
                }
                let mut body = Vec::new();
                if chunked {
                    loop {
                        let mut size = String::new();
                        reader.read_line(&mut size).unwrap();
                        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                        let mut chunk = vec![0; size + 2];
                        reader.read_exact(&mut chunk).unwrap();
                        if size == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..size]);
                    }
                } else {
                    body.resize(length, 0);
                    reader.read_exact(&mut body).unwrap();
                }
                let mut state = state.lock().unwrap();
                let name = relative.rsplit('/').next().unwrap_or_default();
                let refused = state
                    .refused
                    .iter()
                    .find(|(suffix, _, _)| relative.ends_with(suffix.as_str()))
                    .cloned();
                if let Some((_, status, reason)) = refused {
                    respond(&mut writer, status, &reason, b"");
                } else if file.exists() && !name.starts_with("maven-metadata") {
                    respond(&mut writer, 409, "Conflict", b"");
                } else {
                    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                    std::fs::write(&file, &body).unwrap();
                    state.uploads.push(relative.clone());
                    respond(&mut writer, 201, "Created", b"");
                }
            }
            _ => respond(&mut writer, 405, "Method Not Allowed", b""),
        }
    }
}
