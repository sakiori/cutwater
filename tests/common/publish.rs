//! A mock of GitLab's API for `publish`: project 7, whose bot is user 1.

use std::sync::{Arc, Mutex};

use mockito::{Matcher, Mock, Server, ServerGuard};
use serde_json::Value;

pub const TOKEN: &str = "glpat-integration-test-token";

#[derive(Clone, Default)]
pub struct Bodies(Arc<Mutex<Vec<Vec<u8>>>>);

impl Bodies {
    pub fn json(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|body| serde_json::from_slice(body).unwrap())
            .collect()
    }

    pub fn raw(&self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().clone()
    }
}

/// A mock of project 7 on GitLab whose bot, user 1, is a Developer.
pub struct GitLab {
    server: ServerGuard,
}

impl GitLab {
    pub fn new() -> GitLab {
        GitLab::with(r#"{"id": 1, "username": "release-bot"}"#, 30, "[]")
    }

    /// A mock whose bot is the user `user`, with the access level `level`,
    /// in a project with the protected tags `protected`.
    pub fn with(user: &str, level: u32, protected: &str) -> GitLab {
        let mut server = Server::new();
        server
            .mock("GET", "/api/v4/user")
            .match_query(Matcher::Any)
            .match_header("private-token", TOKEN)
            .with_body(user)
            .create();
        server
            .mock("GET", "/api/v4/projects/7/members/all/1")
            .match_query(Matcher::Any)
            .with_body(format!(r#"{{"id": 1, "access_level": {level}}}"#))
            .create();
        let mut gitlab = GitLab { server };
        gitlab.get("/api/v4/projects/7/protected_tags", protected);
        gitlab
    }

    pub fn get(&mut self, path: &str, body: &str) -> Mock {
        self.server
            .mock("GET", path)
            .match_query(Matcher::Any)
            .with_body(body)
            .create()
    }

    pub fn missing(&mut self, path: &str) -> Mock {
        self.server
            .mock("GET", path)
            .match_query(Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Not Found"}"#)
            .create()
    }

    /// The tag `tag` does not exist.
    pub fn no_tag(&mut self, tag: &str) -> Mock {
        self.missing(&format!(
            "/api/v4/projects/7/repository/tags/{}",
            escaped(tag)
        ))
    }

    /// The tag `tag` points at `commit`, with a release or without one.
    pub fn tag(&mut self, tag: &str, commit: &str, release: bool) {
        let path = escaped(tag);
        self.get(
            &format!("/api/v4/projects/7/repository/tags/{path}"),
            &format!(r#"{{"name": "{tag}", "commit": {{"id": "{commit}"}}}}"#),
        );
        if release {
            self.get(
                &format!("/api/v4/projects/7/releases/{path}"),
                &format!(r#"{{"tag_name": "{tag}"}}"#),
            );
        } else {
            self.missing(&format!("/api/v4/projects/7/releases/{path}"));
        }
    }

    pub fn pipeline(&mut self, commit: &str, status: &str) -> Mock {
        self.server
            .mock("GET", "/api/v4/projects/7/pipelines")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("sha".into(), commit.into()),
                Matcher::UrlEncoded("ref".into(), "main".into()),
            ]))
            .with_body(format!(
                r#"[{{"id": 41, "status": "{status}", "web_url": "https://gitlab.example/acme/p/-/pipelines/41"}}]"#
            ))
            .create()
    }

    /// A mock that answers every request of `method` to `path` and keeps
    /// the bodies.
    pub fn capture(&mut self, method: &str, path: &str) -> (Mock, Bodies) {
        let bodies = Bodies::default();
        let kept = bodies.clone();
        let mock = self
            .server
            .mock(method, path)
            .match_query(Matcher::Any)
            .with_status(201)
            .with_body_from_request(move |request| {
                kept.0.lock().unwrap().push(request.body().unwrap().clone());
                b"{}".to_vec()
            })
            .create();
        (mock, bodies)
    }

    pub fn releases(&mut self) -> (Mock, Bodies) {
        self.capture("POST", "/api/v4/projects/7/releases")
    }

    /// Mocks that fail the test when any write reaches GitLab.
    pub fn no_writes(&mut self) -> Vec<Mock> {
        ["POST", "PUT", "DELETE"]
            .into_iter()
            .map(|method| {
                self.server
                    .mock(method, Matcher::Any)
                    .match_query(Matcher::Any)
                    .expect(0)
                    .create()
            })
            .collect()
    }

    pub fn url(&self) -> String {
        self.server.url()
    }
}

/// A tag as one segment of a URL path, as GitLab's API takes it.
fn escaped(tag: &str) -> String {
    tag.replace('%', "%25").replace('/', "%2F")
}
